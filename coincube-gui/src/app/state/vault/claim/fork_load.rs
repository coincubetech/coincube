//! Reopen the fork side through authenticated services and owned constructions.
//! This loader never signs, submits, or treats journal observations as current.
use super::{pairing::Pairing, ConnectSession, TxMap, CHECK_POLICY, SESSION_ENDED};
use crate::{
    app::wallet::Wallet,
    daemon::Daemon,
    dir::CoincubeDirectory,
    services::{
        claim_coordinator::{
            fork::{Coordinator, ForkProduction, Preparation},
            Revoker,
        },
        claim_workflow::{Context, Controller, WalletIdentity},
    },
};
use coincube_core::{
    chain::ChainId,
    claim_finalize::verify_claim_fork_transaction,
    claim_spend::{
        create_ancestry_fork_sweep, create_claim_fork_sweep, reconstruct_claim_fork_sweep,
        reconstruct_poison_self_transfer, AncestrySelfTransfer, PoisonSelfTransfer,
    },
    miniscript::bitcoin::{
        absolute::LockTime,
        hashes::{sha256, Hash},
        secp256k1,
    },
    psbt_unified::UnifiedPsbt,
    spend::CandidateCoin,
};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};
use tokio::sync::watch;

#[derive(Debug)]
pub struct Opened {
    pub handoff: crate::app::claim_intent::ForkHandoff,
    pub generation: u64,
    pub result: Result<(Loaded, Vec<crate::daemon::model::Coin>), String>,
}

enum Source {
    OpReturn(PoisonSelfTransfer),
    Ancestry(AncestrySelfTransfer),
}

pub enum Loaded {
    Signing {
        preparation: Preparation,
        psbt: UnifiedPsbt,
        context: Context,
    },
    Tracking {
        coordinator: Coordinator,
        context: Context,
    },
}
impl Loaded {
    pub fn revoker(&self) -> Revoker {
        match self {
            Self::Signing { preparation, .. } => preparation.revoker(),
            Self::Tracking { coordinator, .. } => coordinator.revoker(),
        }
    }
}
impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Signing { .. } => "ClaimAwaitingSigning",
            Self::Tracking { .. } => "ClaimTracking",
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn load(
    root: &CoincubeDirectory,
    wallet: Arc<Wallet>,
    daemon: Arc<dyn Daemon + Send + Sync>,
    connect: ConnectSession,
    bitcoin_cube: &str,
    fork_cube: &str,
    expected: u64,
    generation: watch::Receiver<u64>,
    feerate_vb: u64,
) -> Result<Loaded, String> {
    let current = || {
        if generation.has_changed().is_err() || *generation.borrow() != expected {
            Err(SESSION_ENDED.to_string())
        } else {
            Ok(())
        }
    };
    current()?;
    let pair = Pairing::read(root, bitcoin_cube, fork_cube, &wallet)?;
    let production = ForkProduction::new(
        connect.client,
        daemon.clone(),
        connect.account,
        expected,
        generation.clone(),
    )
    .map_err(super::describe_production)?;
    let context = production.context().clone();
    let identity = WalletIdentity {
        bitcoin_cube: pair.bitcoin_cube().into(),
        fork_cube: pair.fork_cube().into(),
        descriptor_digest: sha256::Hash::hash(wallet.main_descriptor.to_string().as_bytes()),
    };
    let directory = pair.journal_directory(root);
    let mut controller = Controller::reopen_settling(&directory, &identity, context.clone())
        .await
        .map_err(|e| super::describe(crate::services::claim_coordinator::Error::Journal(e)))?;
    let plan = controller.plan();
    // A Claim loader: the identity above has a Bitcoin Cube, which no Split
    // journal has, so a tracked (Split) plan cannot reach here. Refuse one
    // anyway rather than rely on that (#622 F3).
    if plan.bitcoin_chain != ChainId::Bitcoin
        || plan.fork_chain != wallet.chain
        || plan.tracked_txid.is_some()
        || controller.signed_txid() != Some(plan.step1_txid())
    {
        return Err("The Bitcoin step must be recorded before continuing this Claim.".into());
    }
    let source_index = controller.recorded_bitcoin_change_index().ok_or_else(|| {
        "Open the Bitcoin Cube and resume this older Claim before continuing here.".to_string()
    })?;
    let recorded = controller.recorded_fork_sweep().cloned();
    let fork_index = controller.recorded_fork_change_index();
    let submission = controller.recorded_fork_submission();
    // Keep the journal lock while reading/reconstructing; no competing writer
    // can add an intent while we decide whether this is signing or tracking.
    let coins = daemon
        .list_coins(&[], &plan.claimed_prevouts)
        .await
        .map_err(|e| e.to_string())?
        .coins;
    current()?;
    if coins.len() != plan.claimed_prevouts.len() {
        return Err("Some recorded Claim inputs are missing from this fork Vault.".into());
    }
    let by_outpoint: HashMap<_, _> = coins.iter().map(|coin| (coin.outpoint, coin)).collect();
    if by_outpoint.len() != coins.len() {
        return Err("The fork service returned duplicate Claim inputs.".into());
    }
    let candidates = plan
        .step1
        .input
        .iter()
        .map(|input| &input.previous_output)
        .filter(|outpoint| plan.claimed_prevouts.contains(outpoint))
        .map(|outpoint| {
            let coin = by_outpoint
                .get(outpoint)
                .ok_or_else(|| "A recorded Claim input is not owned by this Vault.".to_string())?;
            if submission.is_none() && (coin.spend_info.is_some() || coin.is_immature) {
                return Err("A Claim input is spent or not yet mature on Bitcoin Blake2b.".into());
            }
            Ok(CandidateCoin {
                outpoint: coin.outpoint,
                amount: coin.amount,
                deriv_index: coin.derivation_index,
                is_change: coin.is_change,
                must_select: true,
                sequence: None,
                ancestor_info: None,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let txids: Vec<_> = plan
        .claimed_prevouts
        .iter()
        .map(|p| p.txid)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let previous = daemon.list_txs(&txids).await.map_err(|e| e.to_string())?;
    current()?;
    let mut getter = TxMap(
        previous
            .transactions
            .into_iter()
            .map(|entry| (entry.tx.compute_txid(), entry.tx))
            .collect(),
    );
    let secp = secp256k1::Secp256k1::verification_only();
    let source = if controller
        .recorded_ancestry()
        .map_err(|e| super::describe(crate::services::claim_coordinator::Error::Journal(e)))?
        .is_some()
    {
        let (selected, transaction) = controller
            .recorded_ancestry_input(&context, &wallet.main_descriptor)
            .map_err(|e| super::describe(crate::services::claim_coordinator::Error::Journal(e)))?
            .ok_or_else(|| {
                "Open the Bitcoin Cube to recover this Claim's input metadata.".to_string()
            })?;
        let mut bitcoin_inputs = candidates.clone();
        bitcoin_inputs.push(selected);
        // This transaction is excluded from the fork. Its only source here is
        // the journal's reverified retained path, never a fork-node query.
        let mut bitcoin_getter = TxMap(getter.0.clone());
        bitcoin_getter
            .0
            .insert(transaction.compute_txid(), transaction);
        let (source, signed) = controller
            .restore_ancestry(
                &context,
                &wallet.main_descriptor,
                &mut bitcoin_getter,
                &bitcoin_inputs,
            )
            .map_err(|e| super::describe(crate::services::claim_coordinator::Error::Journal(e)))?;
        if signed.is_none() {
            return Err("The recorded Bitcoin witness is required to continue this Claim.".into());
        }
        Source::Ancestry(source)
    } else {
        Source::OpReturn(
            reconstruct_poison_self_transfer(
                plan.bitcoin_chain,
                &wallet.main_descriptor,
                &secp,
                &mut getter,
                &candidates,
                source_index,
                &plan.step1,
            )
            .map_err(|e| e.to_string())?,
        )
    };
    let construction = if let Some(recorded) = recorded {
        let index = fork_index.ok_or_else(|| "This older fork plan has no change index; return to the Bitcoin Claim to recover it.".to_string())?;
        match &source {
            Source::OpReturn(source) => reconstruct_claim_fork_sweep(
                source,
                wallet.chain,
                &secp,
                &mut getter,
                &candidates,
                index,
                &recorded,
            )
            .map_err(|e| e.to_string())?,
            Source::Ancestry(source) => controller
                .restore_ancestry_fork_sweep(&context, source, &mut getter, &candidates)
                .map_err(|e| {
                    super::describe(crate::services::claim_coordinator::Error::Journal(e))
                })?,
        }
    } else {
        if feerate_vb == 0 {
            return Err("A current positive fee rate is required to build the fork sweep.".into());
        }
        let index = daemon.reserve_change().await.map_err(|e| e.to_string())?;
        current()?;
        match &source {
            Source::OpReturn(source) => create_claim_fork_sweep(
                source,
                wallet.chain,
                &secp,
                &mut getter,
                &candidates,
                index,
                feerate_vb,
                LockTime::ZERO,
            ),
            Source::Ancestry(source) => create_ancestry_fork_sweep(
                source,
                wallet.chain,
                &secp,
                &mut getter,
                &candidates,
                index,
                feerate_vb,
                LockTime::ZERO,
            ),
        }
        .map_err(|e| e.to_string())?
    };
    if let Some(submission) = submission {
        let recovered = daemon.list_txs(&[submission.txid()]).await.map_err(|e| e.to_string())?
            .transactions.into_iter().find(|entry| entry.tx.compute_txid() == submission.txid())
            .ok_or_else(|| "The recorded fork transaction is unavailable. Read again to track it; it will not be resubmitted.".to_string())?.tx;
        current()?;
        if recovered.compute_wtxid() != submission.wtxid() {
            return Err("The recovered fork witness differs from the recorded submission.".into());
        }
        let verified = verify_claim_fork_transaction(&construction, &recovered, &secp)
            .map_err(|e| e.to_string())?;
        drop(controller);
        let coordinator = match &source {
            Source::OpReturn(source) => Coordinator::resume(
                &directory,
                identity.bitcoin_cube,
                identity.fork_cube,
                source,
                construction,
                verified,
                production,
                CHECK_POLICY,
            ),
            Source::Ancestry(source) => Coordinator::resume_ancestry(
                &directory,
                identity.bitcoin_cube,
                identity.fork_cube,
                source,
                construction,
                verified,
                production,
                CHECK_POLICY,
            ),
        }
        .map_err(super::describe)?;
        current()?;
        Ok(Loaded::Tracking {
            coordinator,
            context,
        })
    } else {
        let psbt =
            UnifiedPsbt::from_psbt(construction.psbt().clone()).map_err(|e| e.to_string())?;
        drop(controller);
        let preparation = match &source {
            Source::OpReturn(source) => Preparation::resume(
                &directory,
                identity.bitcoin_cube,
                identity.fork_cube,
                source,
                construction,
                production,
                CHECK_POLICY,
            ),
            Source::Ancestry(source) => Preparation::resume_ancestry(
                &directory,
                identity.bitcoin_cube,
                identity.fork_cube,
                source,
                construction,
                production,
                CHECK_POLICY,
            ),
        }
        .map_err(super::describe)?;
        current()?;
        Ok(Loaded::Signing {
            preparation,
            psbt,
            context,
        })
    }
}
