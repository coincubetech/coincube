//! One-use, memory-only handoff from Home's foreign-wallet scan to the
//! ordinary unlock path of the selected BTCB2 Cube.
//!
//! The handoff retains authenticated evidence; it is not spend authority. Any
//! Cube open consumes the slot, matching `claim_intent`: opening the wrong Cube
//! must destroy the request rather than leave it armed for a surprising later
//! open. Account credentials are represented only by a one-way session binding.

use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use crate::{
    chain::ChainId,
    services::{
        coincube::CoincubeClient,
        foreign_scan::{ScanDescriptor, ScanReport},
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
    pub report: ScanReport,
    pub external: ScanDescriptor,
    pub internal: Option<ScanDescriptor>,
}

impl SplitIntent {
    pub fn new(
        target_cube_id: String,
        target_source: ChainId,
        account_session_generation: u64,
        client: &CoincubeClient,
        report: ScanReport,
        external: ScanDescriptor,
        internal: Option<ScanDescriptor>,
    ) -> Option<Self> {
        let scan_generation = report.generation();
        if target_cube_id.is_empty()
            || target_source != ChainId::BitcoinBlake2b
            || report.chain() != target_source
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

fn cell() -> &'static Mutex<Option<SplitIntent>> {
    static INTENT: OnceLock<Mutex<Option<SplitIntent>>> = OnceLock::new();
    INTENT.get_or_init(|| Mutex::new(None))
}

pub fn arm(intent: SplitIntent) {
    if let Ok(mut slot) = cell().lock() {
        *slot = Some(intent);
    }
}

/// Consume the single slot on every Cube open. A mismatch returns no evidence;
/// the caller cannot retry it against another Cube later.
pub fn take_for_open(cube_id: &str, source: ChainId) -> Option<SplitIntent> {
    let intent = cell().lock().ok()?.take()?;
    (intent.target_cube_id == cube_id
        && intent.target_source == source
        && intent.is_internally_current())
    .then_some(intent)
}

/// Arm a fresh, valid intent for `cube_id` (tests of the exits that must
/// clear it).
#[cfg(test)]
pub(crate) fn arm_fresh_for_test(cube_id: &str) {
    use coincube_core::miniscript::bitcoin::{hashes::Hash, BlockHash};
    let mut client = CoincubeClient::new();
    client.set_token("split-exit-fixture");
    arm(SplitIntent::new(
        cube_id.to_owned(),
        ChainId::BitcoinBlake2b,
        0,
        &client,
        ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            1,
            BlockHash::from_byte_array([3; 32]),
            Vec::new(),
        ),
        ScanDescriptor::parse(
            crate::services::foreign_scan::Branch::External,
            "wpkh(02c6047f9441ed7d6d3045406e95c07cd85aeb5c6b7a3c2e21b73cdb1e24ff3a64)",
        )
        .expect("fixture descriptor"),
        None,
    )
    .expect("fixture intent"));
}

pub fn clear() {
    if let Ok(mut slot) = cell().lock() {
        *slot = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::{hashes::Hash, BlockHash};

    fn intent(target: &str, client: &CoincubeClient, generation: u64) -> SplitIntent {
        SplitIntent::new(
            target.to_owned(),
            ChainId::BitcoinBlake2b,
            7,
            client,
            ScanReport::for_test(
                ChainId::BitcoinBlake2b,
                generation,
                BlockHash::from_byte_array([3; 32]),
                Vec::new(),
            ),
            ScanDescriptor::parse(
                crate::services::foreign_scan::Branch::External,
                "wpkh(02c6047f9441ed7d6d3045406e95c07cd85aeb5c6b7a3c2e21b73cdb1e24ff3a64)",
            )
            .unwrap(),
            None,
        )
        .unwrap()
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
