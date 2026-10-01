use super::*;
use crate::services::claim_observation::http::{AncestryContext, DiscoveredAncestry};
use coincube_core::{
    claim_ancestry::retained::RetainedPath,
    miniscript::bitcoin::{absolute, transaction, Amount, ScriptBuf, Transaction, TxIn, TxOut},
};

pub(super) fn check(
    observed: &DiscoveredAncestry,
    path: &RetainedPath,
    source: &HttpObservationSource,
    sender: watch::Sender<u64>,
) {
    let plan = plan(path);
    let shared = plan.claimed_prevouts[0];
    let provider = format!("bitcoin|{}/api/v1/esplora/bitcoin/mainnet", source.base);
    assert_eq!(source.provider_identity(), provider);
    let context = || AncestryContext {
        provider: &provider,
        generation: 4,
        policy: Policy {
            max_observation_age_seconds: 60,
            expiry_margin_seconds: 600,
        },
        now: source.now(),
        tips: PreflightTips {
            bitcoin: observed.pair().bitcoin().tip,
            fork: observed.pair().fork().tip,
        },
    };
    assert_eq!(observed.validate_for_plan(path, &plan, context()), Ok(()));
    for case in 0..18 {
        let mut changed = plan.clone();
        let mut current = context();
        let expected = match case {
            0 => {
                current.provider = "https://different.invalid";
                FailureKind::Changed
            }
            1 => {
                current.generation += 1;
                FailureKind::Cancelled
            }
            2 => {
                current.now = observed.observed_at() + 61;
                FailureKind::Stale
            }
            3 => {
                current.now = observed.observed_at() - 1;
                FailureKind::Stale
            }
            4 => {
                current.tips.bitcoin.height += 1;
                FailureKind::Changed
            }
            5 => {
                current.tips.fork.hash = BlockHash::from_str(&"99".repeat(32)).unwrap();
                FailureKind::Changed
            }
            6 => {
                changed.bitcoin_chain = ChainId::Testnet4;
                FailureKind::WrongChain
            }
            7 => {
                changed.poison = Poison::OpReturn;
                FailureKind::InvalidPlan
            }
            8 => {
                changed.claimed_prevouts.push(path.selected());
                FailureKind::InvalidPlan
            }
            9 => {
                changed.claimed_prevouts.push(shared);
                FailureKind::InvalidPlan
            }
            10 => {
                changed.step1.input.push(changed.step1.input[0].clone());
                FailureKind::InvalidPlan
            }
            11 => {
                changed.step1.input.remove(0);
                FailureKind::InvalidPlan
            }
            12 => {
                changed.step1.input[1].previous_output = OutPoint::null();
                FailureKind::InvalidPlan
            }
            13 => {
                changed.step1.output[0].value = Amount::ZERO;
                FailureKind::InvalidPlan
            }
            14 => {
                changed.step1.input[0].witness.push([1]);
                FailureKind::InvalidPlan
            }
            15 => {
                current.policy.max_observation_age_seconds = 0;
                FailureKind::InvalidPlan
            }
            16 => {
                current.provider = &source.base;
                FailureKind::Changed
            }
            17 => {
                current.provider = "bitcoin-blake2b|https://different.invalid/api/v1/esplora/bitcoin-blake2b/mainnet";
                FailureKind::Changed
            }
            _ => unreachable!(),
        };
        assert_eq!(
            observed.validate_for_plan(path, &changed, current),
            Err(expected),
            "case {}",
            case
        );
    }
    // The same root can support a different retained path; that is not the saved intent's path.
    let root = path.reverify().unwrap().root();
    let different = RetainedPath::new(root, vec![path.links().last().unwrap().clone()]).unwrap();
    assert_eq!(
        observed.validate_for_plan(&different, &plan, context()),
        Err(FailureKind::Changed)
    );
    // Txid authentication excludes witness bytes. Even structurally valid raw
    // evidence with the same selected outpoint must match the retained record.
    let mut changed_links = path.links().to_vec();
    let mut changed_tx: Transaction =
        coincube_core::miniscript::bitcoin::consensus::deserialize(&changed_links[0].transaction)
            .unwrap();
    changed_tx.input[0].witness.push([1]);
    assert_eq!(changed_tx.compute_txid(), path.selected().txid);
    changed_links[0].transaction =
        coincube_core::miniscript::bitcoin::consensus::serialize(&changed_tx);
    let changed_path = RetainedPath::new(path.selected(), changed_links).unwrap();
    assert_eq!(
        observed.validate_for_plan(&changed_path, &plan, context()),
        Err(FailureKind::Changed)
    );
    // A successfully collected object must observe session closure at consumption.
    drop(sender);
    assert_eq!(
        observed.validate_for_plan(path, &plan, context()),
        Err(FailureKind::Cancelled)
    );
}

pub(super) fn plan(path: &RetainedPath) -> ClaimPlan {
    let shared = OutPoint::new(Txid::from_str(&"77".repeat(32)).unwrap(), 0);
    let mut script = vec![0, 32];
    script.extend([1; 32]);
    ClaimPlan {
        bitcoin_chain: ChainId::Bitcoin,
        fork_chain: ChainId::BitcoinBlake2b,
        step1: Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![
                TxIn {
                    previous_output: path.selected(),
                    ..TxIn::default()
                },
                TxIn {
                    previous_output: shared,
                    ..TxIn::default()
                },
            ],
            output: vec![TxOut {
                value: Amount::from_sat(1),
                script_pubkey: ScriptBuf::from_bytes(script),
            }],
        },
        claimed_prevouts: vec![shared],
        poison: Poison::InputAncestry,
        previous_confirmation: None,
        tracked_txid: None,
    }
}
