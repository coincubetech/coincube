//! Read-only recovery of possible sends on each chain. A 404 is an observed
//! absence, never proof that re-broadcasting is safe. No result from this
//! module grants signing or submission authority.

use super::{paired_submission::PairedSubmissionRecord, Context, Controller, Error};
use crate::services::claim_observation::{
    http::HttpObservationSource, FailureKind, FreshRead, ObservationSource, TransactionObservation,
    MAX_COLLECTION_TIME,
};
#[cfg(test)]
use coincube_core::miniscript::bitcoin::hashes::Hash;
use coincube_core::{
    chain::ChainId,
    claim::{BlockRef, MIN_CONFIRMATIONS},
    miniscript::bitcoin::{consensus, Transaction, Txid, Wtxid},
};

const MAX_CONFIRMATION_AGE_SECONDS: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChainState {
    Absent,
    Unconfirmed,
    Confirmed {
        block: BlockRef,
        tip: BlockRef,
        confirmations: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReconciliationError {
    Read(FailureKind),
    WitnessMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SideReport {
    NotAttempted,
    Observed(ChainState),
    Indeterminate(ReconciliationError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PairedReconciliation {
    pub bitcoin: SideReport,
    pub fork: SideReport,
    bitcoin_confirmation: Option<BitcoinConfirmation>,
}

impl PairedReconciliation {
    #[allow(dead_code)] // Consumed by the future gated coordinator.
    pub(crate) fn bitcoin_confirmation(&self) -> Option<&BitcoinConfirmation> {
        self.bitcoin_confirmation.as_ref()
    }
}

/// Opaque, short-lived result of independently rechecking the exact Bitcoin
/// witness against its canonical chain. It is still not proof of ancestry or
/// permission to send on BTCB2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BitcoinConfirmation {
    txid: Txid,
    wtxid: Wtxid,
    block: BlockRef,
    tip: BlockRef,
    context: Context,
    started_at: i64,
}

impl BitcoinConfirmation {
    pub(crate) fn block(&self) -> BlockRef {
        self.block
    }

    pub(crate) fn tip(&self) -> BlockRef {
        self.tip
    }

    #[cfg(test)]
    pub(crate) fn with_block_for_test(mut self, block: BlockRef) -> Self {
        self.block = block;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_tip_for_test(mut self, tip: BlockRef) -> Self {
        self.tip = tip;
        self
    }

    pub(crate) fn accepted_for(
        &self,
        current: &Context,
        transaction: &Transaction,
        now: i64,
    ) -> bool {
        self.context == *current
            && self.txid == transaction.compute_txid()
            && self.wtxid == transaction.compute_wtxid()
            && self.started_at >= 0
            && now
                .checked_sub(self.started_at)
                .is_some_and(|age| (0..=MAX_CONFIRMATION_AGE_SECONDS).contains(&age))
    }
}

fn checked<T>(
    read: FreshRead<T>,
    chain: ChainId,
    source: &dyn ObservationSource,
    max_age_seconds: i64,
) -> Result<T, ReconciliationError>
where
    T: Clone,
{
    if read.chain() != chain {
        return Err(ReconciliationError::Read(FailureKind::WrongChain));
    }
    if max_age_seconds <= 0
        || !source
            .now()
            .checked_sub(read.observed_at())
            .is_some_and(|age| age >= 0 && age <= max_age_seconds)
    {
        return Err(ReconciliationError::Read(FailureKind::Stale));
    }
    Ok(read.value().clone())
}

async fn reconcile_side(
    source: &HttpObservationSource,
    chain: ChainId,
    transaction: &Transaction,
    max_age_seconds: i64,
) -> Result<ChainState, ReconciliationError> {
    let txid = transaction.compute_txid();
    let tip_before = checked(
        source.tip(chain).await.map_err(ReconciliationError::Read)?,
        chain,
        source,
        max_age_seconds,
    )?;
    let observation = checked(
        source
            .transaction(chain, txid)
            .await
            .map_err(ReconciliationError::Read)?,
        chain,
        source,
        max_age_seconds,
    )?;
    if !matches!(observation, TransactionObservation::Absent) {
        let raw = source
            .ancestry_transaction(chain, txid)
            .await
            .map_err(ReconciliationError::Read)?;
        if raw != consensus::serialize(transaction) {
            return Err(ReconciliationError::WitnessMismatch);
        }
    }
    let state = match observation {
        TransactionObservation::Absent => ChainState::Absent,
        TransactionObservation::Unconfirmed { txid: observed } if observed == txid => {
            ChainState::Unconfirmed
        }
        TransactionObservation::Confirmed {
            txid: observed,
            block,
        } if observed == txid => {
            let canonical = checked(
                source
                    .hash_at_height(chain, block.height)
                    .await
                    .map_err(ReconciliationError::Read)?,
                chain,
                source,
                max_age_seconds,
            )?;
            if canonical != block.hash || tip_before.height < block.height {
                return Err(ReconciliationError::Read(FailureKind::Changed));
            }
            ChainState::Confirmed {
                block,
                tip: tip_before,
                confirmations: tip_before.height - block.height + 1,
            }
        }
        _ => return Err(ReconciliationError::Read(FailureKind::Malformed)),
    };
    let tip_after = checked(
        source.tip(chain).await.map_err(ReconciliationError::Read)?,
        chain,
        source,
        max_age_seconds,
    )?;
    if tip_before != tip_after {
        return Err(ReconciliationError::Read(FailureKind::Changed));
    }
    Ok(state)
}

async fn report_side(
    source: &HttpObservationSource,
    record: &PairedSubmissionRecord,
    chain: ChainId,
    max_age_seconds: i64,
) -> SideReport {
    if record.possible_attempt(chain).is_none() {
        return SideReport::NotAttempted;
    }
    let transaction = match chain {
        ChainId::Bitcoin => record.bitcoin(),
        ChainId::BitcoinBlake2b => record.fork(),
        _ => return SideReport::Indeterminate(ReconciliationError::Read(FailureKind::WrongChain)),
    };
    match tokio::time::timeout(
        MAX_COLLECTION_TIME,
        reconcile_side(source, chain, transaction, max_age_seconds),
    )
    .await
    {
        Ok(Ok(state)) => SideReport::Observed(state),
        Ok(Err(error)) => SideReport::Indeterminate(error),
        Err(_) => SideReport::Indeterminate(ReconciliationError::Read(FailureKind::Deadline)),
    }
}

impl Controller {
    /// Read both possible sends independently. The journal remains unchanged
    /// and unchecked even if both providers report confirmations. A future
    /// coordinator must separately refresh ancestry, wallet state and policy.
    #[allow(dead_code)] // The paired recovery UI is not yet opened.
    pub(crate) async fn reconcile_possible_ancestry_attempts(
        &mut self,
        current: &Context,
        source: &HttpObservationSource,
        max_age_seconds: i64,
    ) -> Result<PairedReconciliation, Error> {
        self.ensure_context(current)?;
        source
            .validate_context(&current.provider, current.generation)
            .map_err(|_| Error::LateObservation)?;
        let started_at = source.now();
        let record = self
            .intent
            .paired_submission
            .as_ref()
            .ok_or(Error::Unchecked)?
            .clone();
        let max_age_seconds = max_age_seconds.min(MAX_CONFIRMATION_AGE_SECONDS);
        let (bitcoin, fork) = tokio::join!(
            report_side(source, &record, ChainId::Bitcoin, max_age_seconds),
            report_side(source, &record, ChainId::BitcoinBlake2b, max_age_seconds)
        );
        self.ensure_context(current)?;
        source
            .validate_context(&current.provider, current.generation)
            .map_err(|_| Error::LateObservation)?;
        let finished_at = source.now();
        let bitcoin_confirmation = match bitcoin {
            SideReport::Observed(ChainState::Confirmed {
                block,
                tip,
                confirmations,
            }) if confirmations >= MIN_CONFIRMATIONS
                && started_at >= 0
                && finished_at
                    .checked_sub(started_at)
                    .is_some_and(|age| (0..=MAX_CONFIRMATION_AGE_SECONDS).contains(&age)) =>
            {
                let (txid, wtxid) = record.bitcoin_identity();
                Some(BitcoinConfirmation {
                    txid,
                    wtxid,
                    block,
                    tip,
                    context: current.clone(),
                    started_at,
                })
            }
            _ => None,
        };
        Ok(PairedReconciliation {
            bitcoin,
            fork,
            bitcoin_confirmation,
        })
    }
}

#[cfg(test)]
pub(crate) fn confirmed_bitcoin_for_test(
    transaction: &Transaction,
    context: Context,
    started_at: i64,
) -> BitcoinConfirmation {
    BitcoinConfirmation {
        txid: transaction.compute_txid(),
        wtxid: transaction.compute_wtxid(),
        block: BlockRef {
            height: 1,
            hash: coincube_core::miniscript::bitcoin::BlockHash::all_zeros(),
        },
        tip: BlockRef {
            height: 1,
            hash: coincube_core::miniscript::bitcoin::BlockHash::all_zeros(),
        },
        context,
        started_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{claim_observation::CollectionContext, coincube::CoincubeClient};
    use coincube_core::miniscript::bitcoin::{
        absolute, transaction, Amount, ScriptBuf, TxIn, TxOut,
    };
    use httpmock::prelude::*;
    use tokio::sync::watch;

    fn source(server: &MockServer) -> (HttpObservationSource, watch::Sender<u64>) {
        let mut client = CoincubeClient::for_test(server.base_url());
        client.set_token("reconciliation-test-token");
        let (sender, generation) = watch::channel(4);
        let source = HttpObservationSource::new(
            client,
            ChainId::Bitcoin,
            ChainId::BitcoinBlake2b,
            CollectionContext {
                expected_generation: 4,
                generation,
            },
        )
        .unwrap();
        (source, sender)
    }

    fn transaction() -> Transaction {
        let mut input = TxIn::default();
        input.witness.push([1u8; 32]);
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![input],
            output: vec![TxOut {
                value: Amount::from_sat(5000),
                script_pubkey: ScriptBuf::new(),
            }],
        }
    }

    fn fresh(then: httpmock::Then) -> httpmock::Then {
        then.header("x-coincube-observation", "fresh")
            .header("x-cache", "BYPASS")
            .header("cache-control", "no-store")
    }

    #[test]
    fn six_confirmation_token_is_bound_to_session_witness_and_short_lifetime() {
        let signed = transaction();
        let context = Context {
            generation: 4,
            account: "account".into(),
            provider: "bitcoin|https://api.example/api/v1/esplora/bitcoin/mainnet".into(),
        };
        let token = confirmed_bitcoin_for_test(&signed, context.clone(), 100);
        assert!(token.accepted_for(&context, &signed, 130));
        assert!(!token.accepted_for(&context, &signed, 131));
        let mut changed_session = context.clone();
        changed_session.generation += 1;
        assert!(!token.accepted_for(&changed_session, &signed, 100));
        let mut changed_witness = signed.clone();
        changed_witness.input[0].witness.clear();
        changed_witness.input[0].witness.push([2u8; 32]);
        assert_eq!(signed.compute_txid(), changed_witness.compute_txid());
        assert!(!token.accepted_for(&context, &changed_witness, 100));
    }

    #[tokio::test]
    async fn attempt_recovery_distinguishes_absence_from_exact_and_wrong_witness() {
        let signed = transaction();
        let mut other = signed.clone();
        other.input[0].witness.clear();
        other.input[0].witness.push([2u8; 32]);
        assert_eq!(signed.compute_txid(), other.compute_txid());
        assert_ne!(signed.compute_wtxid(), other.compute_wtxid());

        for case in [
            "absent",
            "exact",
            "different-witness",
            "cached-status",
            "confirmed",
            "noncanonical",
        ] {
            let server = MockServer::start();
            let (source, _sender) = source(&server);
            let base = "/api/v1/esplora/bitcoin/mainnet";
            let hash = "11".repeat(32);
            server.mock(|when, then| {
                when.method(GET)
                    .path(format!("{base}/blocks/tip/hash"))
                    .header("x-coincube-observation", "fresh");
                fresh(then).status(200).body(hash.clone());
            });
            server.mock(|when, then| {
                when.method(GET)
                    .path(format!("{base}/block/{hash}/status"))
                    .header("x-coincube-observation", "fresh");
                fresh(then)
                    .status(200)
                    .body(r#"{"in_best_chain":true,"height":10}"#);
            });
            server.mock(|when, then| {
                when.method(GET)
                    .path(format!("{base}/block-height/10"))
                    .header("x-coincube-observation", "fresh");
                fresh(then).status(200).body(hash.clone());
            });
            server.mock(|when, then| {
                when.method(GET)
                    .path(format!("{base}/block-height/5"))
                    .header("x-coincube-observation", "fresh");
                fresh(then).status(200).body(if case == "noncanonical" {
                    "22".repeat(32)
                } else {
                    hash.clone()
                });
            });
            server.mock(|when, then| {
                when.method(GET)
                    .path(format!("{base}/tx/{}", signed.compute_txid()))
                    .header("x-coincube-observation", "fresh");
                let then = then
                    .header("x-coincube-observation", "fresh")
                    .header(
                        "x-cache",
                        if case == "cached-status" {
                            "HIT"
                        } else {
                            "BYPASS"
                        },
                    )
                    .header("cache-control", "no-store");
                if case == "absent" {
                    then.status(404);
                } else if case == "confirmed" || case == "noncanonical" {
                    then.status(200).body(format!(
                        r#"{{"txid":"{}","status":{{"confirmed":true,"block_height":5,"block_hash":"{hash}"}}}}"#,
                        signed.compute_txid()
                    ));
                } else {
                    then.status(200).body(format!(
                        r#"{{"txid":"{}","status":{{"confirmed":false}}}}"#,
                        signed.compute_txid()
                    ));
                }
            });
            let raw = if case == "different-witness" {
                &other
            } else {
                &signed
            };
            let raw_mock = server.mock(|when, then| {
                when.method(GET)
                    .path(format!("{base}/tx/{}/hex", signed.compute_txid()));
                then.status(200)
                    .body(hex::encode(consensus::serialize(raw)));
            });
            let result = reconcile_side(&source, ChainId::Bitcoin, &signed, 30).await;
            match case {
                "absent" => assert_eq!(result, Ok(ChainState::Absent)),
                "exact" => assert_eq!(result, Ok(ChainState::Unconfirmed)),
                "confirmed" => assert_eq!(
                    result,
                    Ok(ChainState::Confirmed {
                        block: BlockRef {
                            height: 5,
                            hash: hash.parse().unwrap(),
                        },
                        tip: BlockRef {
                            height: 10,
                            hash: hash.parse().unwrap(),
                        },
                        confirmations: 6,
                    })
                ),
                "noncanonical" => {
                    assert_eq!(result, Err(ReconciliationError::Read(FailureKind::Changed)))
                }
                "different-witness" => {
                    assert_eq!(result, Err(ReconciliationError::WitnessMismatch));
                }
                "cached-status" => assert_eq!(
                    result,
                    Err(ReconciliationError::Read(FailureKind::FreshnessUnverified))
                ),
                _ => unreachable!(),
            }
            raw_mock.assert_hits(if case == "absent" || case == "cached-status" {
                0
            } else {
                1
            });
        }
    }
}
