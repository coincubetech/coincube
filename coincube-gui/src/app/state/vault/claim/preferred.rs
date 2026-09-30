//! Prefer positively qualified ancestry; provider failures never select fallback.
//!
//! While the production ancestry gate is closed no ancestry construction could
//! be signed, so the build makes no discovery request and uses OP_RETURN, with
//! its RDTS timing refusals (#547).
use super::*;
use crate::services::claim_ancestry_gate;
use crate::services::claim_observation::http::{AncestryContext, MAX_ANCESTRY_CANDIDATES};
use coincube_core::claim::PreflightTips;
use coincube_core::{
    claim_spend::create_ancestry_self_transfer, miniscript::bitcoin::consensus::deserialize,
};
use std::collections::BTreeSet;

#[allow(clippy::too_many_arguments)]
pub(super) async fn build_preferred(
    daemon: Arc<dyn Daemon + Send + Sync>,
    wallet: Arc<Wallet>,
    coins: CoinSet,
    feerate_vb: u64,
    window: ForkWindow,
    connect: ConnectSession,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<Box<Construction>, String> {
    let current = || {
        if generation.has_changed().is_err() || *generation.borrow() != expected {
            Err(SESSION_ENDED.to_string())
        } else {
            Ok(())
        }
    };
    current()?;
    // Admit the exact account/backend/provider pair before discovery or address
    // reservation, using the same admission as journal creation and submission.
    let production = Production::new(
        connect.client.clone(),
        daemon.clone(),
        connect.account,
        expected,
        generation.clone(),
    )
    .map_err(describe_production)?;
    if daemon
        .config()
        .is_none_or(|c| c.main_descriptor != wallet.main_descriptor)
    {
        return Err("The selected backend does not match this Vault.".into());
    }
    let context = production.context().clone();
    drop(production);
    if !claim_ancestry_gate::discovery_open(&connect.client) {
        return tokio::time::timeout(
            CHECK_POLICY.collection_budget,
            op_return(daemon, wallet, coins, feerate_vb, &window, &current),
        )
        .await
        .map_err(|_| "The Claim input search timed out. Read the Vault again.".to_string())?;
    }
    let source = HttpObservationSource::new(
        connect.client,
        ChainId::Bitcoin,
        ChainId::BitcoinBlake2b,
        CollectionContext {
            expected_generation: expected,
            generation: generation.clone(),
        },
    )
    .map_err(|e| format!("Couldn't prepare input ancestry checks: {e:?}"))?;
    tokio::time::timeout(CHECK_POLICY.collection_budget, async {
        let mut candidates: Vec<_> = coins
            .ancestry_candidates
            .iter()
            .map(|c| c.outpoint)
            .collect();
        candidates.sort_unstable();
        if candidates.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("The ancestry candidate inventory contains duplicates.".into());
        }
        let truncated = candidates.len() > MAX_ANCESTRY_CANDIDATES;
        candidates.truncate(MAX_ANCESTRY_CANDIDATES);
        let proof = source
            .discover_preferred_ancestry(&candidates, CHECK_POLICY.observations)
            .await
            .map_err(|e| format!("Couldn't establish input ancestry: {e:?}"))?;
        current()?;
        let Some(proof) = proof else {
            if truncated {
                return Err("No input qualified within the 32-candidate ancestry search limit; remaining inputs were not checked.".into());
            }
            // An absent qualifying input is not permission to ignore RDTS.
            return op_return(daemon, wallet, coins, feerate_vb, &window, &current).await;
        };
        let path = proof
            .retained_path()
            .map_err(|e| format!("Invalid retained ancestry: {e:?}"))?;
        let selected = path.selected();
        if !candidates.contains(&selected) || coins.pre_fork.iter().any(|c| c.outpoint == selected)
        {
            return Err("The selected ancestry input does not match this Claim.".into());
        }
        let expected_inputs: BTreeSet<_> = coins
            .pre_fork
            .iter()
            .map(|c| c.outpoint)
            .chain(std::iter::once(selected))
            .collect();
        if expected_inputs.len() != coins.pre_fork.len() + 1 {
            return Err("The Claim contains duplicate inputs.".into());
        }
        let requested: Vec<_> = expected_inputs.iter().copied().collect();
        let mut fresh = daemon
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
        wallet.apply_coin_overrides(&mut fresh);
        let actual: BTreeSet<_> = fresh.iter().map(|c| c.outpoint).collect();
        if actual != expected_inputs
            || fresh.len() != expected_inputs.len()
            || fresh.iter().any(|c| {
                c.spend_info.is_some()
                    || c.is_immature
                    || c.block_height.is_none_or(|h| {
                        h < 0
                            || h > tip
                            || if c.outpoint == selected {
                                (h as u64) < window.fork_height
                            } else {
                                h as u64 >= window.fork_height
                            }
                    })
            })
        {
            return Err(
                "Claim inputs changed, are spent, or are not yet mature. Read the Vault again."
                    .into(),
            );
        }
        let txids: Vec<_> = coins
            .pre_fork
            .iter()
            .map(|c| c.outpoint.txid)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let transactions = daemon.list_txs(&txids).await.map_err(|e| e.to_string())?;
        current()?;
        let mut getter = TxMap(
            transactions
                .transactions
                .into_iter()
                .map(|t| (t.tx.compute_txid(), t.tx))
                .collect(),
        );
        let selected_tx: Transaction =
            deserialize(&path.links()[0].transaction).map_err(|e| e.to_string())?;
        getter.0.insert(selected_tx.compute_txid(), selected_tx);
        let index = daemon.reserve_change().await.map_err(|e| e.to_string())?;
        current()?;
        let owned: Vec<_> = fresh
            .iter()
            .map(|c| CandidateCoin {
                outpoint: c.outpoint,
                amount: c.amount,
                deriv_index: c.derivation_index,
                is_change: c.is_change,
                must_select: true,
                sequence: None,
                ancestor_info: None,
            })
            .collect();
        let locktime = u32::try_from(tip)
            .ok()
            .and_then(|h| LockTime::from_height(h).ok())
            .ok_or_else(|| "The Bitcoin tip height is invalid.".to_string())?;
        let transfer = create_ancestry_self_transfer(
            wallet.chain,
            &wallet.main_descriptor,
            &secp256k1::Secp256k1::verification_only(),
            &mut getter,
            &owned,
            index,
            feerate_vb,
            locktime,
            &path
                .reverify()
                .map_err(|e| format!("Invalid ancestry: {e:?}"))?,
        )
        .map_err(|e| e.to_string())?;
        let plan = claim::ClaimPlan {
            bitcoin_chain: wallet.chain,
            fork_chain: ChainId::BitcoinBlake2b,
            step1: transfer.psbt().unsigned_tx.clone(),
            claimed_prevouts: transfer.claimed_prevouts().to_vec(),
            poison: claim::Poison::InputAncestry,
            previous_confirmation: None,
            tracked_txid: None,
        };
        proof
            .validate_for_plan(
                &path,
                &plan,
                AncestryContext {
                    provider: &context.provider,
                    generation: expected,
                    policy: CHECK_POLICY.observations,
                    now: source.now(),
                    tips: PreflightTips {
                        bitcoin: proof.pair().bitcoin().tip,
                        fork: proof.pair().fork().tip,
                    },
                },
            )
            .map_err(|e| format!("The input ancestry changed during construction: {e:?}"))?;
        current()?;
        Ok(Box::new(Construction::Ancestry { transfer, path }))
    })
    .await
    .map_err(|_| "The Claim input search timed out. Read the Vault again.".to_string())?
}

/// The OP_RETURN construction, refused while RDTS disallows it.
async fn op_return(
    daemon: Arc<dyn Daemon + Send + Sync>,
    wallet: Arc<Wallet>,
    coins: CoinSet,
    feerate_vb: u64,
    window: &ForkWindow,
    current: &impl Fn() -> Result<(), String>,
) -> Result<Box<Construction>, String> {
    if let Err(assessment) = window.rdts {
        return Err(rdts_refusal(assessment, window));
    }
    let result = build(daemon, wallet, coins, feerate_vb, window.fork_hash).await;
    current()?;
    result
}
