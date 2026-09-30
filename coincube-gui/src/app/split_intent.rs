//! One-use, memory-only handoff from Home's foreign-wallet scan to the
//! ordinary unlock path of the selected BTCB2 Cube.
//!
//! The handoff retains authenticated evidence; it is not spend authority. Any
//! Cube open consumes the slot, matching `claim_intent`: opening the wrong Cube
//! must destroy the request rather than leave it armed for a surprising later
//! open. Account credentials are represented only by a one-way session binding.

use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::{
    chain::ChainId,
    services::{
        coincube::CoincubeClient,
        foreign_scan::{ScanDescriptor, ScanReport},
        foreign_split_inventory::{SplitInventory, TwoChainScan},
    },
};

/// How long armed scan evidence stays usable: long enough to unlock the
/// target Cube and start its daemon, short enough that an abandoned unlock
/// cannot surface an old scan on a later ordinary open.
pub const MAX_AGE: Duration = Duration::from_secs(10 * 60);

pub struct SplitIntent {
    created: Instant,
    target_cube_id: String,
    target_source: ChainId,
    account_session_generation: u64,
    scan_generation: u64,
    session_binding: [u8; 32],
    /// The BTCB2 scan.
    pub report: ScanReport,
    /// The Bitcoin scan of the same descriptors and generation.
    pub bitcoin_report: ScanReport,
    /// Both scans joined; carries the step-1 inputs and fresh receive index.
    pub inventory: SplitInventory,
    pub external: ScanDescriptor,
    pub internal: Option<ScanDescriptor>,
}

impl SplitIntent {
    pub fn new(
        target_cube_id: String,
        target_source: ChainId,
        account_session_generation: u64,
        client: &CoincubeClient,
        scan: TwoChainScan,
        external: ScanDescriptor,
        internal: Option<ScanDescriptor>,
    ) -> Option<Self> {
        let TwoChainScan {
            btcb2: report,
            bitcoin: bitcoin_report,
            inventory,
        } = scan;
        let scan_generation = report.generation();
        if target_cube_id.is_empty()
            || target_source != ChainId::BitcoinBlake2b
            || report.chain() != target_source
            || bitcoin_report.chain() != ChainId::Bitcoin
            || bitcoin_report.generation() != scan_generation
            || inventory.generation() != scan_generation
            || inventory.btcb2_tip() != report.tip()
            || inventory.bitcoin_tip() != bitcoin_report.tip()
            || external.branch() != crate::services::foreign_scan::Branch::External
            || internal.as_ref().is_some_and(|descriptor| {
                descriptor.branch() != crate::services::foreign_scan::Branch::Internal
            })
        {
            return None;
        }
        Some(Self {
            created: Instant::now(),
            target_cube_id,
            target_source,
            account_session_generation,
            scan_generation,
            session_binding: bind(client)?,
            report,
            bitcoin_report,
            inventory,
            external,
            internal,
        })
    }

    pub fn target_cube_id(&self) -> &str {
        &self.target_cube_id
    }

    pub fn target_source(&self) -> ChainId {
        self.target_source
    }

    pub fn account_session_generation(&self) -> u64 {
        self.account_session_generation
    }

    pub fn scan_generation(&self) -> u64 {
        self.scan_generation
    }

    pub fn matches_client(&self, client: &CoincubeClient) -> bool {
        bind(client).is_some_and(|binding| binding == self.session_binding)
    }

    pub fn is_internally_current(&self) -> bool {
        self.scan_generation == self.report.generation()
            && self.scan_generation == self.bitcoin_report.generation()
            && self.scan_generation == self.inventory.generation()
            && self.report.chain() == self.target_source
            && self.created.elapsed() <= MAX_AGE
    }

    #[cfg(test)]
    pub(crate) fn aged(mut self, age: Duration) -> Self {
        self.created = Instant::now()
            .checked_sub(age)
            .expect("test age fits the monotonic clock");
        self
    }
}

fn bind(client: &CoincubeClient) -> Option<[u8; 32]> {
    let token = client.token()?.trim();
    if token.is_empty() {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update((client.base_url.len() as u64).to_be_bytes());
    digest.update(client.base_url.as_bytes());
    digest.update((token.len() as u64).to_be_bytes());
    digest.update(token.as_bytes());
    Some(digest.finalize().into())
}

/// Run `f` on the one-intent slot. Production has one process-wide slot.
#[cfg(not(test))]
fn with_slot<R>(f: impl FnOnce(&mut Option<SplitIntent>) -> R) -> Option<R> {
    use std::sync::{Mutex, OnceLock};
    static INTENT: OnceLock<Mutex<Option<SplitIntent>>> = OnceLock::new();
    let mut slot = INTENT.get_or_init(|| Mutex::new(None)).lock().ok()?;
    Some(f(&mut slot))
}

/// Tests get one slot per test thread. Many unrelated tests clear the slot as
/// a side effect (every `App` construction, `session::close`, any
/// `SplitWalletPanel::cancel`), so a process-wide slot made every test that
/// arms and then reads it racy under the parallel test runner. Arm, clear
/// and take in a test all run on that test's own thread.
#[cfg(test)]
fn with_slot<R>(f: impl FnOnce(&mut Option<SplitIntent>) -> R) -> Option<R> {
    thread_local! {
        static INTENT: std::cell::RefCell<Option<SplitIntent>> =
            const { std::cell::RefCell::new(None) };
    }
    INTENT.with(|slot| Some(f(&mut slot.borrow_mut())))
}

pub fn arm(intent: SplitIntent) {
    with_slot(|slot| *slot = Some(intent));
}

/// Consume the single slot on every Cube open. A mismatch returns no evidence;
/// the caller cannot retry it against another Cube later.
pub fn take_for_open(cube_id: &str, source: ChainId) -> Option<SplitIntent> {
    let intent = with_slot(Option::take)??;
    (intent.target_cube_id == cube_id
        && intent.target_source == source
        && intent.is_internally_current())
    .then_some(intent)
}

/// Arm a fresh, valid intent for `cube_id` (tests of the exits that must
/// clear it).
#[cfg(test)]
pub(crate) fn arm_fresh_for_test(cube_id: &str) {
    let mut client = CoincubeClient::new();
    client.set_token("split-exit-fixture");
    arm(SplitIntent::new(
        cube_id.to_owned(),
        ChainId::BitcoinBlake2b,
        0,
        &client,
        empty_scan_for_test(1),
        ScanDescriptor::parse(
            crate::services::foreign_scan::Branch::External,
            "wpkh(02c6047f9441ed7d6d3045406e95c07cd85aeb5c6b7a3c2e21b73cdb1e24ff3a64)",
        )
        .expect("fixture descriptor"),
        None,
    )
    .expect("fixture intent"));
}

/// A complete, empty two-chain scan of `generation`.
#[cfg(test)]
pub(crate) fn empty_scan_for_test(generation: u64) -> TwoChainScan {
    use coincube_core::miniscript::bitcoin::{hashes::Hash, BlockHash};
    let report = |chain, byte| {
        ScanReport::for_test(
            chain,
            generation,
            BlockHash::from_byte_array([byte; 32]),
            Vec::new(),
        )
        .with_coverage(vec![crate::services::foreign_scan::BranchCoverage {
            branch: crate::services::foreign_scan::Branch::External,
            start: 0,
            end_exclusive: 1,
            last_used: None,
        }])
    };
    let btcb2 = report(ChainId::BitcoinBlake2b, 3).with_fork_height(Some(100));
    let bitcoin = report(ChainId::Bitcoin, 4);
    let inventory =
        SplitInventory::join(&btcb2, &bitcoin, generation, false).expect("empty scans join");
    TwoChainScan {
        btcb2,
        bitcoin,
        inventory,
    }
}

pub fn clear() {
    with_slot(|slot| *slot = None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(target: &str, client: &CoincubeClient, generation: u64) -> SplitIntent {
        SplitIntent::new(
            target.to_owned(),
            ChainId::BitcoinBlake2b,
            7,
            client,
            empty_scan_for_test(generation),
            ScanDescriptor::parse(
                crate::services::foreign_scan::Branch::External,
                "wpkh(02c6047f9441ed7d6d3045406e95c07cd85aeb5c6b7a3c2e21b73cdb1e24ff3a64)",
            )
            .unwrap(),
            None,
        )
        .unwrap()
    }

    /// #568 B1a: the intent carries both scans and their join, all of one
    /// generation; a mismatched Bitcoin report or inventory is refused.
    #[test]
    fn split_intent_requires_matching_bitcoin_report_and_inventory() {
        let mut client = CoincubeClient::new();
        client.set_token("session-a");
        let external = || {
            ScanDescriptor::parse(
                crate::services::foreign_scan::Branch::External,
                "wpkh(02c6047f9441ed7d6d3045406e95c07cd85aeb5c6b7a3c2e21b73cdb1e24ff3a64)",
            )
            .unwrap()
        };
        let make = |scan: TwoChainScan| {
            SplitIntent::new(
                "cube-a".to_owned(),
                ChainId::BitcoinBlake2b,
                7,
                &client,
                scan,
                external(),
                None,
            )
        };
        let intent = make(empty_scan_for_test(9)).unwrap();
        assert_eq!(intent.bitcoin_report.chain(), ChainId::Bitcoin);
        assert_eq!(intent.inventory.generation(), 9);

        let mut other_generation = empty_scan_for_test(9);
        other_generation.bitcoin = empty_scan_for_test(8).bitcoin;
        let mut btcb2_as_bitcoin = empty_scan_for_test(9);
        btcb2_as_bitcoin.bitcoin = btcb2_as_bitcoin.btcb2.clone();
        let mut stale_inventory = empty_scan_for_test(9);
        stale_inventory.inventory = empty_scan_for_test(8).inventory;
        let mut other_tip = empty_scan_for_test(9);
        other_tip.bitcoin = crate::services::foreign_scan::ScanReport::for_test(
            ChainId::Bitcoin,
            9,
            coincube_core::miniscript::bitcoin::hashes::Hash::from_byte_array([5; 32]),
            Vec::new(),
        );
        for scan in [
            other_generation,
            btcb2_as_bitcoin,
            stale_inventory,
            other_tip,
        ] {
            assert!(make(scan).is_none());
        }
    }

    #[test]
    fn an_intent_older_than_the_bound_is_not_current_and_is_not_taken() {
        let _session_guard = crate::app::session::test_guard();
        clear();
        let mut client = CoincubeClient::new();
        client.set_token("session-a");
        assert!(intent("cube-a", &client, 11).is_internally_current());
        let stale = intent("cube-a", &client, 11).aged(MAX_AGE + Duration::from_secs(1));
        assert!(!stale.is_internally_current());
        arm(stale);
        assert!(take_for_open("cube-a", ChainId::BitcoinBlake2b).is_none());
    }

    #[test]
    fn intent_is_one_shot_and_bound_to_target_session_and_generation() {
        let _session_guard = crate::app::session::test_guard();
        clear();
        let mut client = CoincubeClient::new();
        client.set_token("session-a");
        arm(intent("cube-a", &client, 11));

        assert!(take_for_open("cube-b", ChainId::BitcoinBlake2b).is_none());
        assert!(take_for_open("cube-a", ChainId::BitcoinBlake2b).is_none());

        arm(intent("cube-a", &client, 12));
        let taken = take_for_open("cube-a", ChainId::BitcoinBlake2b).unwrap();
        assert_eq!(taken.account_session_generation(), 7);
        assert_eq!(taken.scan_generation(), 12);
        assert!(taken.matches_client(&client));
        client.set_token("session-b");
        assert!(!taken.matches_client(&client));
        assert!(take_for_open("cube-a", ChainId::BitcoinBlake2b).is_none());

        client.set_token("session-c");
        arm(intent("cube-a", &client, 13));
        clear();
        assert!(take_for_open("cube-a", ChainId::BitcoinBlake2b).is_none());

        arm(intent("cube-a", &client, 14));
        crate::app::session::close();
        assert!(
            take_for_open("cube-a", ChainId::BitcoinBlake2b).is_none(),
            "locking the app must destroy the handoff"
        );
    }
}
