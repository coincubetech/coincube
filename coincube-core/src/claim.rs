//! Dormant Claim/Split observation checks, not spend or replay authorization.
//!
//! Inputs must come from chain-bound, independently checked observations. This
//! module does not authenticate a server, verify chain work/inclusion, sign,
//! broadcast, or construct the GUI's `SplitEvidence`. In particular, a txid
//! absent on the other chain and a post-fork timestamp are never poison proof.

use std::collections::BTreeSet;

use miniscript::bitcoin::{BlockHash, OutPoint, Transaction, Txid};
use serde::{Deserialize, Serialize};

use crate::chain::ChainId;

pub const MIN_CONFIRMATIONS: u64 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRef {
    pub height: u64,
    pub hash: BlockHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Poison {
    /// The actual transaction must contain an OP_RETURN script larger than
    /// RDTS's 83-byte output-script limit. Its validity is time-dependent.
    OpReturn,
    /// No verified ancestry constructor exists yet. Always unsupported;
    /// never replace this with an absent-txid or post-fork-time heuristic.
    InputAncestry,
}

/// Restart-persistable intent and last observed inclusion. This records no seed
/// and confers no authority; every assessment rechecks the supplied observations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimPlan {
    pub bitcoin_chain: ChainId,
    pub fork_chain: ChainId,
    pub step1: Transaction,
    pub claimed_prevouts: Vec<OutPoint>,
    pub poison: Poison,
    pub previous_confirmation: Option<BlockRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentState {
    Unconfigured,
    ConfigurationError,
    Unavailable,
    RpcUnavailable,
    Malformed,
    ForkAbsent,
    ForkInactive,
    RdtsAbsent,
    RdtsUnsupported,
    /// Project only a validated typed status response. Do not infer `active`
    /// from height/time or hard-code an expiry for either fork network.
    Flagday {
        height: u64,
        expiry_time: i64,
        active: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeploymentObservation {
    pub chain: ChainId,
    /// The status endpoint exposes height only. Its adapter must bracket the
    /// request with same-hash tip reads before attaching this anchor.
    pub tip: BlockRef,
    pub state: DeploymentState,
    pub observed_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkTransactionPresence {
    Unknown,
    Present,
    /// Not itself poison evidence; checked only alongside actual OP_RETURN
    /// poison and current active RDTS observations.
    NotObserved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForkObservation {
    pub chain: ChainId,
    pub step1_txid: Txid,
    pub step1_presence: ForkTransactionPresence,
    pub tip: BlockRef,
    /// Chain median-time-past at exactly `tip`, not its block timestamp or
    /// this machine's wall clock. Required separately from the status DTO.
    pub median_time_past: i64,
    pub observed_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionLocation {
    /// Includes transport failures and data not yet checked.
    Unknown,
    /// A successful fresh chain query found no current confirmation. This
    /// can invalidate a prior inclusion, but never proves chain exclusivity.
    Unconfirmed,
    Confirmed {
        txid: Txid,
        block: BlockRef,
        /// Fresh block-hash-at-height read from the same best-chain view.
        best_chain_hash_at_height: BlockHash,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitcoinObservation {
    pub chain: ChainId,
    pub tip: BlockRef,
    pub location: TransactionLocation,
    pub observed_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Operational values selected by the caller, never an expiry substitute.
    /// Zero and negative values are refused rather than disabling a guard.
    pub max_observation_age_seconds: i64,
    pub expiry_margin_seconds: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreflightTips {
    /// New tip reads immediately before preflight, not cached preview tips.
    pub bitcoin: BlockRef,
    pub fork: BlockRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assessment {
    InvalidPlan,
    WrongChain,
    Unknown,
    StaleObservation,
    Deployment(DeploymentState),
    RdtsScheduled,
    RdtsInactive,
    RdtsExpired,
    ExpiryMargin,
    InputProofUnsupported,
    PoisonMissing,
    Step1AlreadyOnFork,
    WaitingForConfirmation,
    WaitingForDepth {
        confirmations: u64,
    },
    Reorged,
    NeedsPreflightRecheck,
    /// Only these supplied observations satisfy the local checks. This is
    /// NOT permission to sign/broadcast or a persistent "split" assertion.
    ObservationsEligibleForPreflight,
}

fn fresh(observed_at: i64, now: i64, max_age: i64) -> bool {
    observed_at >= 0
        && now
            .checked_sub(observed_at)
            .is_some_and(|age| age >= 0 && age <= max_age)
}

/// The RDTS deployment gate on its own: whether the fork's observed
/// deployment state admits an OP_RETURN poison *now*.
///
/// `Ok(())` when RDTS is active on the fork with more than
/// `policy.expiry_margin_seconds` left before its expiry (measured against
/// the fork's median-time-past at `fork.tip`, never a local clock);
/// otherwise the [`Assessment`] that refuses. This is exactly the check
/// [`assess`] applies before it looks at the Bitcoin-side transaction, split
/// out so a caller that has no transaction yet — a wizard deciding whether
/// to build one — gets the same answer it would get later, from the same
/// code, rather than a hard-coded margin of its own. Freshness of the two
/// observations is the caller's to have checked; `assess` does so first.
pub fn assess_deployment(
    fork: &ForkObservation,
    deployment: &DeploymentObservation,
    policy: Policy,
) -> Result<(), Assessment> {
    if policy.expiry_margin_seconds <= 0 {
        return Err(Assessment::InvalidPlan);
    }
    match deployment.state {
        DeploymentState::Flagday {
            height,
            expiry_time,
            active,
        } => {
            let Some(next_height) = fork.tip.height.checked_add(1) else {
                return Err(Assessment::Unknown);
            };
            // Contradictory "active" claims are malformed, not permission.
            if expiry_time <= 0
                || (active && (next_height < height || fork.median_time_past >= expiry_time))
            {
                return Err(Assessment::Deployment(DeploymentState::Malformed));
            }
            if !active {
                return Err(if fork.median_time_past >= expiry_time {
                    Assessment::RdtsExpired
                } else if next_height < height {
                    Assessment::RdtsScheduled
                } else {
                    Assessment::RdtsInactive
                });
            }
            if expiry_time
                .checked_sub(fork.median_time_past)
                .is_none_or(|remaining| remaining <= policy.expiry_margin_seconds)
            {
                return Err(Assessment::ExpiryMargin);
            }
            Ok(())
        }
        state => Err(Assessment::Deployment(state)),
    }
}

/// Evaluate from scratch. Stale/reorg/unknown results must replace any previous
/// eligibility in the caller; never cache eligibility as completed split proof.
pub fn assess(
    plan: &ClaimPlan,
    bitcoin: BitcoinObservation,
    fork: ForkObservation,
    deployment: DeploymentObservation,
    policy: Policy,
    now: i64,
    preflight: Option<PreflightTips>,
) -> Assessment {
    if !matches!(
        (plan.bitcoin_chain, plan.fork_chain),
        (ChainId::Bitcoin, ChainId::BitcoinBlake2b)
            | (ChainId::Testnet4, ChainId::BitcoinBlake2bTestnet4)
    ) || bitcoin.chain != plan.bitcoin_chain
        || fork.chain != plan.fork_chain
        || deployment.chain != plan.fork_chain
    {
        return Assessment::WrongChain;
    }
    let claimed: BTreeSet<_> = plan.claimed_prevouts.iter().copied().collect();
    let actual: BTreeSet<_> = plan.step1.input.iter().map(|i| i.previous_output).collect();
    if claimed.is_empty()
        || claimed.len() != plan.claimed_prevouts.len()
        || actual.len() != plan.step1.input.len()
        || !claimed.is_subset(&actual)
        || plan.step1.input.iter().any(|i| i.previous_output.is_null())
        || plan.step1.output.is_empty()
        || policy.max_observation_age_seconds <= 0
        || policy.expiry_margin_seconds <= 0
    {
        return Assessment::InvalidPlan;
    }
    if !fresh(bitcoin.observed_at, now, policy.max_observation_age_seconds)
        || !fresh(fork.observed_at, now, policy.max_observation_age_seconds)
        || !fresh(
            deployment.observed_at,
            now,
            policy.max_observation_age_seconds,
        )
    {
        return Assessment::StaleObservation;
    }
    if fork.tip != deployment.tip || fork.median_time_past < 0 {
        return Assessment::Unknown;
    }
    if plan.poison == Poison::InputAncestry {
        return Assessment::InputProofUnsupported;
    }
    if !plan
        .step1
        .output
        .iter()
        .any(|o| o.script_pubkey.is_op_return() && o.script_pubkey.len() > 83)
    {
        return Assessment::PoisonMissing;
    }
    if fork.step1_txid != plan.step1.compute_txid() {
        return Assessment::Unknown;
    }
    match fork.step1_presence {
        ForkTransactionPresence::Unknown => return Assessment::Unknown,
        ForkTransactionPresence::Present => return Assessment::Step1AlreadyOnFork,
        ForkTransactionPresence::NotObserved => {}
    }
    if let Err(refused) = assess_deployment(&fork, &deployment, policy) {
        return refused;
    }
    let block = match bitcoin.location {
        TransactionLocation::Unknown => return Assessment::Unknown,
        TransactionLocation::Unconfirmed => {
            return if plan.previous_confirmation.is_some() {
                Assessment::Reorged
            } else {
                Assessment::WaitingForConfirmation
            };
        }
        TransactionLocation::Confirmed {
            txid,
            block,
            best_chain_hash_at_height,
        } => {
            if txid != plan.step1.compute_txid() {
                return Assessment::Unknown;
            }
            if block.hash != best_chain_hash_at_height
                || plan
                    .previous_confirmation
                    .is_some_and(|previous| previous != block)
            {
                return Assessment::Reorged;
            }
            block
        }
    };
    let Some(confirmations) = bitcoin
        .tip
        .height
        .checked_sub(block.height)
        .and_then(|d| d.checked_add(1))
    else {
        return Assessment::Unknown;
    };
    if confirmations < MIN_CONFIRMATIONS {
        return Assessment::WaitingForDepth { confirmations };
    }
    if !preflight.is_some_and(|tips| tips.bitcoin == bitcoin.tip && tips.fork == fork.tip) {
        return Assessment::NeedsPreflightRecheck;
    }
    Assessment::ObservationsEligibleForPreflight
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniscript::bitcoin::{
        absolute, hashes::Hash, transaction, Amount, ScriptBuf, TxIn, TxOut,
    };

    struct Fixture {
        plan: ClaimPlan,
        bitcoin: BitcoinObservation,
        fork: ForkObservation,
        deployment: DeploymentObservation,
        policy: Policy,
        now: i64,
    }

    impl Fixture {
        fn new() -> Self {
            let input = OutPoint {
                txid: Txid::from_byte_array([1; 32]),
                vout: 0,
            };
            let block = BlockRef {
                height: 100,
                hash: BlockHash::from_byte_array([2; 32]),
            };
            let step1 = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: input,
                    ..TxIn::default()
                }],
                output: vec![TxOut {
                    value: Amount::ZERO,
                    script_pubkey: ScriptBuf::from_bytes(vec![0x6a; 84]),
                }],
            };
            let now = 10_000;
            let fork = ForkObservation {
                chain: ChainId::BitcoinBlake2b,
                step1_txid: step1.compute_txid(),
                step1_presence: ForkTransactionPresence::NotObserved,
                tip: BlockRef {
                    height: 200,
                    hash: BlockHash::from_byte_array([3; 32]),
                },
                median_time_past: 8_000,
                observed_at: now,
            };
            let bitcoin = BitcoinObservation {
                chain: ChainId::Bitcoin,
                tip: BlockRef {
                    height: 105,
                    hash: BlockHash::from_byte_array([4; 32]),
                },
                location: TransactionLocation::Confirmed {
                    txid: step1.compute_txid(),
                    block,
                    best_chain_hash_at_height: block.hash,
                },
                observed_at: now,
            };
            Self {
                plan: ClaimPlan {
                    bitcoin_chain: bitcoin.chain,
                    fork_chain: fork.chain,
                    step1,
                    claimed_prevouts: vec![input],
                    poison: Poison::OpReturn,
                    previous_confirmation: Some(block),
                },
                bitcoin,
                fork,
                deployment: DeploymentObservation {
                    chain: fork.chain,
                    tip: fork.tip,
                    state: DeploymentState::Flagday {
                        height: 150,
                        expiry_time: 20_000,
                        active: true,
                    },
                    observed_at: now,
                },
                policy: Policy {
                    max_observation_age_seconds: 60,
                    expiry_margin_seconds: 600,
                },
                now,
            }
        }

        fn evaluate(&self) -> Assessment {
            assess(
                &self.plan,
                self.bitcoin,
                self.fork,
                self.deployment,
                self.policy,
                self.now,
                Some(PreflightTips {
                    bitcoin: self.bitcoin.tip,
                    fork: self.fork.tip,
                }),
            )
        }
    }

    #[test]
    fn depth_and_fresh_tip_recheck_are_required() {
        let mut f = Fixture::new();
        f.bitcoin.tip.height = 104;
        assert_eq!(
            f.evaluate(),
            Assessment::WaitingForDepth { confirmations: 5 }
        );
        f.bitcoin.tip.height = 105;
        assert_eq!(f.evaluate(), Assessment::ObservationsEligibleForPreflight);
        assert_eq!(
            assess(
                &f.plan,
                f.bitcoin,
                f.fork,
                f.deployment,
                f.policy,
                f.now,
                None
            ),
            Assessment::NeedsPreflightRecheck
        );
        let stale_tip = BlockRef {
            height: 105,
            hash: BlockHash::from_byte_array([9; 32]),
        };
        assert_eq!(
            assess(
                &f.plan,
                f.bitcoin,
                f.fork,
                f.deployment,
                f.policy,
                f.now,
                Some(PreflightTips {
                    bitcoin: stale_tip,
                    fork: f.fork.tip
                })
            ),
            Assessment::NeedsPreflightRecheck
        );
    }

    #[test]
    fn expiry_uses_dynamic_schedule_and_chain_mtp() {
        let mut f = Fixture::new();
        // Wall clock is later than expiry, but consensus comparison uses MTP.
        f.deployment.state = DeploymentState::Flagday {
            height: 150,
            expiry_time: 8_601,
            active: true,
        };
        assert_eq!(f.evaluate(), Assessment::ObservationsEligibleForPreflight);
        f.deployment.state = DeploymentState::Flagday {
            height: 150,
            expiry_time: 8_600,
            active: true,
        };
        assert_eq!(f.evaluate(), Assessment::ExpiryMargin);
        f.fork.median_time_past = 8_600;
        assert_eq!(
            f.evaluate(),
            Assessment::Deployment(DeploymentState::Malformed)
        );
        f.deployment.state = DeploymentState::Flagday {
            height: 150,
            expiry_time: 8_600,
            active: false,
        };
        assert_eq!(f.evaluate(), Assessment::RdtsExpired);
    }

    #[test]
    fn unavailable_scheduled_and_inactive_never_authorize_poison() {
        let mut f = Fixture::new();
        for state in [
            DeploymentState::Unconfigured,
            DeploymentState::ConfigurationError,
            DeploymentState::Unavailable,
            DeploymentState::RpcUnavailable,
            DeploymentState::ForkAbsent,
            DeploymentState::Malformed,
            DeploymentState::ForkInactive,
            DeploymentState::RdtsAbsent,
            DeploymentState::RdtsUnsupported,
        ] {
            f.deployment.state = state;
            assert_eq!(f.evaluate(), Assessment::Deployment(state));
        }
        f.deployment.state = DeploymentState::Flagday {
            height: 202,
            expiry_time: 20_000,
            active: false,
        };
        assert_eq!(f.evaluate(), Assessment::RdtsScheduled);
        f.deployment.state = DeploymentState::Flagday {
            height: 150,
            expiry_time: 20_000,
            active: false,
        };
        assert_eq!(f.evaluate(), Assessment::RdtsInactive);
        f.deployment.state = DeploymentState::Flagday {
            height: 202,
            expiry_time: 20_000,
            active: true,
        };
        assert_eq!(
            f.evaluate(),
            Assessment::Deployment(DeploymentState::Malformed)
        );
    }

    #[test]
    fn every_stale_or_future_observation_revokes_eligibility() {
        for n in 0..3 {
            for offset in [-61, 1] {
                let mut f = Fixture::new();
                match n {
                    0 => f.bitcoin.observed_at += offset,
                    1 => f.fork.observed_at += offset,
                    _ => f.deployment.observed_at += offset,
                }
                assert_eq!(f.evaluate(), Assessment::StaleObservation);
            }
        }
        let mut f = Fixture::new();
        f.bitcoin.observed_at -= 60;
        assert_eq!(f.evaluate(), Assessment::ObservationsEligibleForPreflight);
    }

    #[test]
    fn transaction_and_all_claimed_prevouts_are_bound() {
        let mut f = Fixture::new();
        f.plan.step1.lock_time = absolute::LockTime::from_consensus(1);
        assert_eq!(f.evaluate(), Assessment::Unknown);
        let mut f = Fixture::new();
        f.plan.claimed_prevouts[0].vout = 10;
        assert_eq!(f.evaluate(), Assessment::InvalidPlan);
        let mut f = Fixture::new();
        f.plan.claimed_prevouts.push(f.plan.claimed_prevouts[0]);
        assert_eq!(f.evaluate(), Assessment::InvalidPlan);
        let mut f = Fixture::new();
        f.plan.step1.input.push(f.plan.step1.input[0].clone());
        assert_eq!(f.evaluate(), Assessment::InvalidPlan);
    }

    #[test]
    fn actual_op_return_size_is_required() {
        for size in [0, 1, 83] {
            let mut f = Fixture::new();
            f.plan.step1.output[0].script_pubkey = ScriptBuf::from_bytes(vec![0x6a; size]);
            assert_eq!(f.evaluate(), Assessment::PoisonMissing);
        }
        let mut f = Fixture::new();
        f.plan.step1.output[0].script_pubkey = ScriptBuf::from_bytes(vec![0x51; 100]);
        assert_eq!(f.evaluate(), Assessment::PoisonMissing);
    }

    #[test]
    fn fork_transaction_presence_is_not_exclusivity_proof() {
        let mut f = Fixture::new();
        f.fork.step1_presence = ForkTransactionPresence::Present;
        assert_eq!(f.evaluate(), Assessment::Step1AlreadyOnFork);
        f.fork.step1_presence = ForkTransactionPresence::Unknown;
        assert_eq!(f.evaluate(), Assessment::Unknown);
        f.fork.step1_presence = ForkTransactionPresence::NotObserved;
        f.plan.poison = Poison::InputAncestry;
        assert_eq!(f.evaluate(), Assessment::InputProofUnsupported);
    }

    #[test]
    fn negative_evidence_cannot_create_input_poison() {
        let mut f = Fixture::new();
        f.plan.poison = Poison::InputAncestry;
        assert_eq!(f.evaluate(), Assessment::InputProofUnsupported);
        // Missing confirmation and later heights/times do not change that.
        f.bitcoin.location = TransactionLocation::Unconfirmed;
        f.bitcoin.tip.height = 2_000_000;
        assert_eq!(f.evaluate(), Assessment::InputProofUnsupported);
    }

    #[test]
    fn reorg_and_unknown_queries_revoke_prior_confirmation() {
        let mut f = Fixture::new();
        f.bitcoin.location = TransactionLocation::Unknown;
        assert_eq!(f.evaluate(), Assessment::Unknown);
        f.bitcoin.location = TransactionLocation::Unconfirmed;
        assert_eq!(f.evaluate(), Assessment::Reorged);
        f.plan.previous_confirmation = None;
        assert_eq!(f.evaluate(), Assessment::WaitingForConfirmation);
        let mut f = Fixture::new();
        if let TransactionLocation::Confirmed {
            best_chain_hash_at_height,
            ..
        } = &mut f.bitcoin.location
        {
            *best_chain_hash_at_height = BlockHash::from_byte_array([9; 32]);
        }
        assert_eq!(f.evaluate(), Assessment::Reorged);
        let mut f = Fixture::new();
        f.plan.previous_confirmation.as_mut().unwrap().hash = BlockHash::from_byte_array([9; 32]);
        assert_eq!(f.evaluate(), Assessment::Reorged);
    }

    #[test]
    fn wrong_chain_and_incoherent_tip_data_refuse() {
        let mut f = Fixture::new();
        f.deployment.chain = ChainId::BitcoinBlake2bTestnet4;
        assert_eq!(f.evaluate(), Assessment::WrongChain);
        let mut f = Fixture::new();
        f.deployment.tip.hash = BlockHash::from_byte_array([9; 32]);
        assert_eq!(f.evaluate(), Assessment::Unknown);
        let mut f = Fixture::new();
        f.bitcoin.tip.height = 99;
        assert_eq!(f.evaluate(), Assessment::Unknown);
        let mut f = Fixture::new();
        f.plan.bitcoin_chain = ChainId::Testnet4;
        f.plan.fork_chain = ChainId::BitcoinBlake2bTestnet4;
        f.bitcoin.chain = f.plan.bitcoin_chain;
        f.fork.chain = f.plan.fork_chain;
        f.deployment.chain = f.plan.fork_chain;
        assert_eq!(f.evaluate(), Assessment::ObservationsEligibleForPreflight);
    }

    #[test]
    fn invalid_policy_and_numeric_boundaries_fail_closed() {
        let mut f = Fixture::new();
        f.policy.max_observation_age_seconds = 0;
        assert_eq!(f.evaluate(), Assessment::InvalidPlan);
        f.policy.max_observation_age_seconds = 60;
        f.policy.expiry_margin_seconds = -1;
        assert_eq!(f.evaluate(), Assessment::InvalidPlan);
        let mut f = Fixture::new();
        f.fork.tip.height = u64::MAX;
        f.deployment.tip = f.fork.tip;
        assert_eq!(f.evaluate(), Assessment::Unknown);
        let mut f = Fixture::new();
        f.deployment.state = DeploymentState::Flagday {
            height: 150,
            expiry_time: 0,
            active: false,
        };
        assert_eq!(
            f.evaluate(),
            Assessment::Deployment(DeploymentState::Malformed)
        );
    }

    #[test]
    fn persisted_plan_does_not_persist_permission() {
        let f = Fixture::new();
        let encoded = serde_json::to_string(&f.plan).unwrap();
        let plan: ClaimPlan = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            assess(
                &plan,
                f.bitcoin,
                f.fork,
                f.deployment,
                f.policy,
                f.now + 61,
                None
            ),
            Assessment::StaleObservation
        );
    }
}
