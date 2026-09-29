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
        create_claim_fork_sweep, reconstruct_claim_fork_sweep, reconstruct_poison_self_transfer,
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
    let controller = Controller::reopen(&directory, &identity, context.clone())
        .map_err(|e| super::describe(crate::services::claim_coordinator::Error::Journal(e)))?;
    let plan = controller.plan();
    if plan.bitcoin_chain != ChainId::Bitcoin
        || plan.fork_chain != wallet.chain
        || controller.signed_txid() != Some(plan.step1.compute_txid())
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
        .map(|input| {
            let coin = by_outpoint
                .get(&input.previous_output)
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
    let source = reconstruct_poison_self_transfer(
        plan.bitcoin_chain,
        &wallet.main_descriptor,
        &secp,
        &mut getter,
        &candidates,
        source_index,
        &plan.step1,
    )
    .map_err(|e| e.to_string())?;
    let construction = if let Some(recorded) = recorded {
        let index = fork_index.ok_or_else(|| "This older fork plan has no change index; return to the Bitcoin Claim to recover it.".to_string())?;
        reconstruct_claim_fork_sweep(
            &source,
            wallet.chain,
            &secp,
            &mut getter,
            &candidates,
            index,
            &recorded,
        )
        .map_err(|e| e.to_string())?
    } else {
        if feerate_vb == 0 {
            return Err("A current positive fee rate is required to build the fork sweep.".into());
        }
        let index = daemon.reserve_change().await.map_err(|e| e.to_string())?;
        current()?;
        create_claim_fork_sweep(
            &source,
            wallet.chain,
            &secp,
            &mut getter,
            &candidates,
            index,
            feerate_vb,
            LockTime::ZERO,
        )
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
        let coordinator = Coordinator::resume(
            &directory,
            identity.bitcoin_cube,
            identity.fork_cube,
            &source,
            construction,
            verified,
            production,
            CHECK_POLICY,
        )
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
        let preparation = Preparation::resume(
            &directory,
            identity.bitcoin_cube,
            identity.fork_cube,
            &source,
            construction,
            production,
            CHECK_POLICY,
        )
        .map_err(super::describe)?;
        current()?;
        Ok(Loaded::Signing {
            preparation,
            psbt,
            context,
        })
    }
}
