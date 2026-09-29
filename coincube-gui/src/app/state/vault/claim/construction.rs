//! Keep ancestry and OP_RETURN artifacts distinct throughout the panel.
use super::*;
use coincube_core::{
    claim_ancestry::retained::RetainedPath,
    claim_finalize::{
        finalize_ancestry_transfer, verify_ancestry_transaction, VerifiedAncestryTransfer,
        VerifiedPoisonTransfer,
    },
    claim_spend::AncestrySelfTransfer,
    spend::CreateSpendWarning,
};

#[derive(Debug)]
pub enum Construction {
    OpReturn(PoisonSelfTransfer),
    Ancestry {
        transfer: AncestrySelfTransfer,
        path: RetainedPath,
    },
}

pub(super) enum Verified {
    OpReturn(VerifiedPoisonTransfer),
    Ancestry(VerifiedAncestryTransfer),
}

#[cfg(test)]
impl Verified {
    pub(super) fn transaction(&self) -> &Transaction {
        match self {
            Self::OpReturn(tx) => tx.transaction(),
            Self::Ancestry(tx) => tx.transaction(),
        }
    }
}

impl Construction {
    pub fn change_index(&self) -> ChildNumber {
        match self {
            Self::OpReturn(transfer) => transfer.change_index(),
            Self::Ancestry { transfer, .. } => transfer.change_index(),
        }
    }

    pub fn psbt(&self) -> &Psbt {
        match self {
            Self::OpReturn(transfer) => transfer.psbt(),
            Self::Ancestry { transfer, .. } => transfer.psbt(),
        }
    }
    pub fn warnings(&self) -> &[CreateSpendWarning] {
        match self {
            Self::OpReturn(transfer) => transfer.warnings(),
            Self::Ancestry { transfer, .. } => transfer.warnings(),
        }
    }
    pub fn selected_ancestry_input(&self) -> Option<OutPoint> {
        match self {
            Self::OpReturn(_) => None,
            Self::Ancestry { transfer, .. } => Some(transfer.poison_input()),
        }
    }
    pub(super) fn verify(
        &self,
        signed: &Psbt,
        recovered: Option<&Transaction>,
    ) -> Result<Verified, String> {
        let secp = secp256k1::Secp256k1::verification_only();
        match self {
            Self::OpReturn(transfer) => match recovered {
                Some(tx) => verify_poison_transaction(transfer, tx, &secp),
                None => finalize_poison_transfer(transfer, signed, &secp),
            }
            .map(Verified::OpReturn)
            .map_err(|e| e.to_string()),
            Self::Ancestry { transfer, .. } => match recovered {
                Some(tx) => verify_ancestry_transaction(transfer, tx, &secp),
                None => finalize_ancestry_transfer(transfer, signed, &secp),
            }
            .map(Verified::Ancestry)
            .map_err(|e| e.to_string()),
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn open(
        &self,
        directory: &std::path::Path,
        bitcoin_cube: String,
        fork_cube: String,
        verified: Verified,
        production: Production,
        resume: bool,
    ) -> Result<Coordinator, String> {
        match (self, verified) {
            (Self::OpReturn(transfer), Verified::OpReturn(verified)) => {
                let open = if resume {
                    Coordinator::resume
                } else {
                    Coordinator::create
                };
                open(
                    directory,
                    bitcoin_cube,
                    fork_cube,
                    transfer,
                    verified,
                    production,
                    CHECK_POLICY,
                )
            }
            (Self::Ancestry { transfer, path }, Verified::Ancestry(verified)) => {
                let open = if resume {
                    Coordinator::resume_ancestry
                } else {
                    Coordinator::create_ancestry
                };
                open(
                    directory,
                    bitcoin_cube,
                    fork_cube,
                    transfer,
                    path,
                    verified,
                    production,
                    CHECK_POLICY,
                )
            }
            _ => {
                return Err("The Claim signature artifact does not match its construction.".into())
            }
        }
        .map_err(describe)
    }
}

/// Reconstruct saved ownership and signatures without restoring live eligibility.
#[allow(clippy::too_many_arguments)]
pub(super) async fn restore_ancestry(
    controller: &mut claim_workflow::Controller,
    context: &Context,
    daemon: Arc<dyn Daemon + Send + Sync>,
    wallet: &Wallet,
    expected: u64,
    generation: &watch::Receiver<u64>,
) -> Result<Box<Construction>, String> {
    let check = || {
        if generation.has_changed().is_err() || *generation.borrow() != expected {
            Err(SESSION_ENDED.to_string())
        } else {
            Ok(())
        }
    };
    check()?;
    let journal_error = |e| describe(claim_coordinator::Error::Journal(e));
    let path = controller
        .recorded_ancestry()
        .map_err(journal_error)?
        .ok_or_else(|| "The recorded Claim has no ancestry path.".to_string())?;
    let (selected, transaction) = controller
        .recorded_ancestry_input(context, &wallet.main_descriptor)
        .map_err(journal_error)?
        .ok_or_else(|| "This older Claim has no verified input derivation metadata.".to_string())?;
    let plan = controller.plan();
    let coins = daemon
        .list_coins(&[], &plan.claimed_prevouts)
        .await
        .map_err(|e| e.to_string())?
        .coins;
    check()?;
    let outpoints: std::collections::BTreeSet<_> = coins.iter().map(|c| c.outpoint).collect();
    if outpoints.len() != coins.len()
        || outpoints != plan.claimed_prevouts.iter().copied().collect()
    {
        return Err("Some recorded Claim inputs are missing or duplicated in this Vault.".into());
    }
    let mut candidates: Vec<_> = coins
        .iter()
        .map(|coin| CandidateCoin {
            outpoint: coin.outpoint,
            amount: coin.amount,
            deriv_index: coin.derivation_index,
            is_change: coin.is_change,
            must_select: true,
            sequence: None,
            ancestor_info: None,
        })
        .collect();
    candidates.push(selected);
    let txids: Vec<_> = outpoints
        .iter()
        .map(|p| p.txid)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let previous = daemon.list_txs(&txids).await.map_err(|e| e.to_string())?;
    check()?;
    let mut getter = TxMap(
        previous
            .transactions
            .into_iter()
            .map(|t| (t.tx.compute_txid(), t.tx))
            .collect(),
    );
    getter.0.insert(transaction.compute_txid(), transaction);
    let (transfer, _) = controller
        .restore_ancestry(context, &wallet.main_descriptor, &mut getter, &candidates)
        .map_err(journal_error)?;
    check()?;
    Ok(Box::new(Construction::Ancestry { transfer, path }))
}
