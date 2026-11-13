use super::*;
use crate::services::coincube::network_status::{
    ForkActivation, NetworkObservation, RdtsFlagday, RdtsStatus,
};
use coincube_core::miniscript::bitcoin::hashes::Hash;

fn hash(n: u8) -> BlockHash {
    BlockHash::from_byte_array([n; 32])
}
fn policy() -> Policy {
    Policy {
        max_observation_age_seconds: 60,
    }
}
fn headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("x-cache", "BYPASS".parse().unwrap());
    h.insert(CACHE_CONTROL, "private, no-store".parse().unwrap());
    h
}
fn anchor() -> NetworkAnchorStatus {
    NetworkAnchorStatus {
        network: ChainId::BitcoinBlake2b,
        state: AnchorState::Available,
        anchor: Some(NetworkAnchor {
            tip_hash: hash(2),
            tip_height: 100,
            tip_median_time_past: 8_000,
            observed_at: 10_000,
            observation: NetworkObservation {
                tip_height: 100,
                fork: Some(ForkActivation {
                    height: 90,
                    active: true,
                }),
                rdts: RdtsStatus::Flagday {
                    flagday: RdtsFlagday {
                        height: 90,
                        expiry_time: 20_000,
                        active: true,
                    },
                },
            },
        }),
    }
}

#[test]
fn explicit_bypass_and_no_store_are_both_required() {
    for (cache, control, accepted) in [
        ("BYPASS", "no-store", true),
        ("BYPASS", "private, No-Store", true),
        ("HIT", "no-store", false),
        ("MISS", "no-store", false),
        ("BYPASS", "max-age=0", false),
        ("BYPASS", "no-store=false", false),
    ] {
        let mut h = HeaderMap::new();
        h.insert("x-cache", cache.parse().unwrap());
        h.insert(CACHE_CONTROL, control.parse().unwrap());
        assert_eq!(
            FreshRead::from_response(ChainId::Bitcoin, (), 10_000, &h).is_ok(),
            accepted
        );
    }
    assert_eq!(
        FreshRead::from_response(ChainId::Bitcoin, (), 10_000, &HeaderMap::new()).unwrap_err(),
        FailureKind::FreshnessUnverified
    );
    let mut ambiguous = headers();
    ambiguous.append("x-cache", "HIT".parse().unwrap());
    assert_eq!(
        FreshRead::from_response(ChainId::Bitcoin, (), 10_000, &ambiguous).unwrap_err(),
        FailureKind::FreshnessUnverified
    );
}

/// The ancestry anchor needs an active fork at or below a coherent, fresh tip
/// and nothing from RDTS. Ported from the removed fork-only anchor and anchor
/// projection tests, which checked the same rules through `fork_anchor` and
/// `project_anchor`.
#[test]
fn ancestry_anchor_needs_a_coherent_active_fork_but_not_rdts() {
    for rdts in [RdtsStatus::Absent, RdtsStatus::Unsupported] {
        let mut a = anchor();
        a.anchor.as_mut().unwrap().observation.rdts = rdts;
        let checked =
            checked_ancestry_anchor(a, ChainId::BitcoinBlake2b, policy(), 10_000).unwrap();
        assert_eq!(checked.tip_height, 100);
    }
    for change in 0..5 {
        let mut a = anchor();
        match change {
            0 => a.anchor = None,
            1 => a.anchor.as_mut().unwrap().observation.tip_height = 99,
            2 => a.anchor.as_mut().unwrap().observation.fork = None,
            3 => {
                a.anchor
                    .as_mut()
                    .unwrap()
                    .observation
                    .fork
                    .as_mut()
                    .unwrap()
                    .active = false
            }
            _ => a.anchor.as_mut().unwrap().tip_median_time_past = -1,
        }
        assert_eq!(
            checked_ancestry_anchor(a, ChainId::BitcoinBlake2b, policy(), 10_000)
                .unwrap_err()
                .kind,
            FailureKind::Malformed,
            "{}",
            change
        );
    }
    assert_eq!(
        checked_ancestry_anchor(anchor(), ChainId::BitcoinBlake2b, policy(), 10_061)
            .unwrap_err()
            .kind,
        FailureKind::Stale
    );
    assert_eq!(
        checked_ancestry_anchor(anchor(), ChainId::Bitcoin, policy(), 10_000)
            .unwrap_err()
            .kind,
        FailureKind::WrongChain
    );
}
