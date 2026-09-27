//! Recollect ancestry and owned-input state before opening any signer.
use super::*;
use std::collections::BTreeSet;

pub(super) async fn check(
    built: &Construction,
    daemon: Arc<dyn Daemon + Send + Sync>,
    wallet: Arc<Wallet>,
    connect: ConnectSession,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<Vec<Coin>, String> {
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
        connect.account,
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
        // Full positive-proof signing admission is still gated on independent
        // acceptance. Neither this successful collection nor a journal is a
        // signing capability. Keep the refusal explicit until that gate changes.
        if collected.assessment().assessment == Assessment::InputProofUnsupported {
            return Err("Bitcoin-only input signing is not available yet.".into());
        }
        Err("The ancestry signing check did not produce signing authorization.".into())
    })
    .await
    .map_err(|_| "Checking the Claim inputs timed out. Read the Vault again.".to_string())?
}
