//! Recollect ancestry and owned-input state before opening any signer.
use super::*;
use coincube_core::miniscript::bitcoin::hashes::sha256;
use std::collections::BTreeSet;

/// One-use result of checking this exact unsigned construction and session.
/// No serialization or clone: a delayed GUI result must be consumed afresh.
#[derive(Debug)]
pub struct CheckedInputs {
    coins: Vec<Coin>,
    psbt_digest: sha256::Hash,
    path_digest: sha256::Hash,
    descriptor: String,
    account: String,
    origin: String,
    expected: u64,
    generation: watch::Receiver<u64>,
    deadline: std::time::Instant,
}
impl CheckedInputs {
    pub(super) fn consume(
        self,
        built: &Construction,
        wallet: &Wallet,
        connect: Option<&ConnectSession>,
        generation: u64,
    ) -> Result<Vec<Coin>, String> {
        let Construction::Ancestry { path, .. } = built else {
            return Err("The ancestry construction changed.".into());
        };
        let valid_session = connect.is_some_and(|c| {
            c.account == self.account && c.client.base_url.trim_end_matches('/') == self.origin
        });
        if std::time::Instant::now() >= self.deadline
            || generation != self.expected
            || *self.generation.borrow() != self.expected
            || self.generation.has_changed().is_err()
            || !valid_session
            || wallet.chain != ChainId::Bitcoin
            || wallet.main_descriptor.to_string() != self.descriptor
            || sha256::Hash::hash(&path.encode()) != self.path_digest
            || sha256::Hash::hash(&built.psbt().serialize()) != self.psbt_digest
        {
            return Err(
                "The ancestry signing check expired or changed. Check the Claim again.".into(),
            );
        }
        // Hashing bounded proof data may still take time or be descheduled.
        // Recheck live authority at the final consumption boundary as well.
        if std::time::Instant::now() >= self.deadline
            || *self.generation.borrow() != self.expected
            || self.generation.has_changed().is_err()
        {
            return Err(
                "The ancestry signing check expired or changed. Check the Claim again.".into(),
            );
        }
        Ok(self.coins)
    }
}

pub(super) async fn check(
    built: &Construction,
    daemon: Arc<dyn Daemon + Send + Sync>,
    wallet: Arc<Wallet>,
    connect: ConnectSession,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<CheckedInputs, String> {
    let _checked = check_inputs(built, daemon, wallet, connect, expected, generation).await?;
    // Full flow acceptance is still required before returning this to a signer.
    Err("Bitcoin-only input signing is not available yet.".into())
}

pub(super) async fn check_inputs(
    built: &Construction,
    daemon: Arc<dyn Daemon + Send + Sync>,
    wallet: Arc<Wallet>,
    connect: ConnectSession,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<CheckedInputs, String> {
    let current = || {
        if generation.has_changed().is_err() || *generation.borrow() != expected {
            Err(SESSION_ENDED.to_string())
        } else {
            Ok(())
        }
    };
    current()?;
    let Construction::Ancestry { transfer, path } = built else {
        return Err("An ancestry construction is required for this check.".into());
    };
    let production = Production::new(
        connect.client.clone(),
        daemon.clone(),
        connect.account.clone(),
        expected,
        generation.clone(),
    )
    .map_err(describe_production)?;
    if transfer.descriptor() != &wallet.main_descriptor
        || daemon
            .config()
            .is_none_or(|c| c.main_descriptor != wallet.main_descriptor)
    {
        return Err("The Claim does not match this Vault and backend.".into());
    }
    drop(production);
    let account = connect.account.clone();
    let origin = connect.client.base_url.trim_end_matches('/').to_string();
    let source = HttpObservationSource::new(
        connect.client,
        ChainId::Bitcoin,
        ChainId::BitcoinBlake2b,
        CollectionContext {
            expected_generation: expected,
            generation: generation.clone(),
        },
    )
    .map_err(|e| format!("Couldn't prepare ancestry checks: {e:?}"))?;
    tokio::time::timeout(CHECK_POLICY.collection_budget, async {
        let input_check_started = std::time::Instant::now();
        let requested: Vec<_> = transfer
            .psbt()
            .unsigned_tx
            .input
            .iter()
            .map(|i| i.previous_output)
            .collect();
        let mut coins = daemon
            .list_coins(&[CoinStatus::Confirmed], &requested)
            .await
            .map_err(|e| e.to_string())?
            .coins;
        current()?;
        let tip = daemon
            .get_info()
            .await
            .map_err(|e| e.to_string())?
            .block_height;
        current()?;
        wallet.apply_coin_overrides(&mut coins);
        let actual: BTreeSet<_> = coins.iter().map(|c| c.outpoint).collect();
        if actual.len() != coins.len()
            || actual != requested.iter().copied().collect()
            || coins.iter().any(|c| {
                c.spend_info.is_some()
                    || c.is_immature
                    || c.block_height.is_none_or(|h| h < 0 || h > tip)
            })
        {
            return Err(
                "Claim inputs changed, are spent, or are not yet mature. Read the Vault again."
                    .into(),
            );
        }
        let secp = secp256k1::Secp256k1::verification_only();
        for coin in &coins {
            let index = requested
                .iter()
                .position(|p| *p == coin.outpoint)
                .ok_or("A Claim input is missing.")?;
            let prevout = transfer.psbt().inputs[index]
                .witness_utxo
                .as_ref()
                .ok_or("A Claim prevout is missing.")?;
            if coin.derivation_index.is_hardened() {
                return Err("The Claim input derivation is invalid.".into());
            }
            let derived = if coin.is_change {
                wallet.main_descriptor.change_descriptor()
            } else {
                wallet.main_descriptor.receive_descriptor()
            }
            .derive(coin.derivation_index, &secp);
            if coin.amount != prevout.value || derived.script_pubkey() != prevout.script_pubkey {
                return Err("The owned Claim input metadata changed. Read the Vault again.".into());
            }
        }
        let plan = claim::ClaimPlan {
            bitcoin_chain: wallet.chain,
            fork_chain: ChainId::BitcoinBlake2b,
            step1: transfer.psbt().unsigned_tx.clone(),
            claimed_prevouts: transfer.claimed_prevouts().to_vec(),
            poison: claim::Poison::InputAncestry,
            previous_confirmation: None,
        };
        let collected = source
            .collect_ancestry(
                path,
                &plan,
                CHECK_POLICY.observations,
                CHECK_POLICY.collection_budget,
            )
            .await
            .map_err(|e| format!("Couldn't recheck the Bitcoin-only input: {e:?}"))?;
        current()?;
        let assessment = collected
            .assess_verified_observations(
                path,
                &plan,
                crate::services::claim_observation::http::AncestryContext {
                    provider: &source.provider_identity(),
                    generation: expected,
                    policy: CHECK_POLICY.observations,
                    now: source.now(),
                    tips: collected.assessment().observations.preflight,
                },
            )
            .map_err(|e| format!("The ancestry signing observations changed: {e:?}"))?;
        if assessment.assessment != Assessment::WaitingForConfirmation {
            return Err("The ancestry transaction is no longer ready for signing.".into());
        }
        // Anchor before sampling wall time or hashing: local computation and
        // scheduling delays consume freshness rather than extending it.
        let sampled = std::time::Instant::now();
        let deadline = evidence_deadline(
            sampled,
            source.now(),
            collected.observed_at(),
            CHECK_POLICY.observations.max_observation_age_seconds,
        )
        .ok_or("The ancestry signing check expired.")?
        .min(
            input_check_started
                .checked_add(CHECK_POLICY.collection_budget)
                .ok_or("The ancestry signing check expired.")?,
        );
        current()?;
        Ok(CheckedInputs {
            coins,
            psbt_digest: sha256::Hash::hash(&built.psbt().serialize()),
            path_digest: sha256::Hash::hash(&path.encode()),
            descriptor: wallet.main_descriptor.to_string(),
            account,
            origin,
            expected,
            generation: generation.clone(),
            deadline,
        })
    })
    .await
    .map_err(|_| "Checking the Claim inputs timed out. Read the Vault again.".to_string())?
}

// Match the coordinator's conservative allowance for UNIX-second quantization.
fn evidence_deadline(
    sampled: std::time::Instant,
    now: i64,
    observed_at: i64,
    max_age: i64,
) -> Option<std::time::Instant> {
    if observed_at < 0 || max_age <= 0 {
        return None;
    }
    let age = now.checked_sub(observed_at).filter(|age| *age >= 0)?;
    let remaining = max_age
        .checked_sub(age)?
        .checked_sub(1)
        .filter(|v| *v > 0)?;
    sampled.checked_add(Duration::from_secs(remaining as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_excludes_quantization_and_delayed_construction() {
        let sampled = std::time::Instant::now();
        assert_eq!(evidence_deadline(sampled, 100, 99, 2), None);
        assert_eq!(
            evidence_deadline(sampled, 100, 100, 2),
            Some(sampled + Duration::from_secs(1))
        );
        assert_eq!(evidence_deadline(sampled, 100, 101, 20), None);
        assert_eq!(evidence_deadline(sampled, 100, -1, 20), None);
        assert_eq!(evidence_deadline(sampled, i64::MAX, 0, 20), None);
        // Model two seconds of work after the wall-time sample without sleeping.
        let delayed = sampled.checked_sub(Duration::from_secs(2)).unwrap();
        assert!(evidence_deadline(delayed, 100, 100, 2).unwrap() < sampled);
    }
}
