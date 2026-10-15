//! Reserved production gate for the paired BTCB2 ancestry migration.
//!
//! The old one-sided Claim signer and completion route stay closed regardless
//! of this switch. The paired signer and durable two-chain submission paths
//! are staged behind this gate. Keep them unavailable until the live wizard
//! and device acceptance checks pass; unit coverage alone is insufficient.
//!
//! Keep this closed until issue #547 records owner acceptance of the live
//! two-chain flow and its independent final review. `InputProofUnsupported` in
//! the generic core assessment remains a separate fail-closed boundary: only
//! the typed, freshly verified ancestry path may reach this gate.

/// User-facing refusal retained while production ancestry is awaiting approval.
pub(crate) const CLOSED_MESSAGE: &str = "Bitcoin-only input signing is not available yet.";
/// No new claim may use the old 90-byte OP_RETURN construction. The replacement
/// needs independently verified Bitcoin-only and BTCB2-only funding coins.
pub(crate) const NEW_CLAIM_MESSAGE: &str = "Claim will require confirmed Bitcoin-only and Bitcoin Blake2b-only funding coins in this Vault. This transaction cannot be signed until both funding coins can be verified.";

// Paired signer entry and both paired submission reviews consume this token.
// Changing it requires the acceptance evidence documented above.
const ENABLED: bool = false;

// The disposable regtest child is a separate test-binary process. It may
// exercise the whole paired signing route without making a production build
// or an ordinary unit-test run capable of opening the launch gate.
#[cfg(all(test, feature = "regtest-harness"))]
static REGTEST_ACCEPTANCE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(all(test, feature = "regtest-harness"))]
pub(crate) fn enable_disposable_regtest_acceptance() {
    assert_eq!(
        std::env::var("CLAIM_GUI_REGTEST_CHILD").as_deref(),
        Ok("1"),
        "only the disposable two-chain child may open the test gate"
    );
    REGTEST_ACCEPTANCE.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// An unforgeable authorization token for production ancestry side effects.
#[allow(dead_code)] // Paired signer and submission remain gated in production.
pub(crate) struct Authorization(());

#[allow(dead_code)] // Paired signer and submission remain gated in production.
pub(crate) fn authorization() -> Option<Authorization> {
    if ENABLED {
        return Some(Authorization(()));
    }
    #[cfg(all(test, feature = "regtest-harness"))]
    if REGTEST_ACCEPTANCE.load(std::sync::atomic::Ordering::SeqCst) {
        return Some(Authorization(()));
    }
    None
}

/// Historical one-sided discovery is test-only. The paired deposit check uses
/// its own two-chain collector and cannot be opened by this switch.
///
/// Tests may open discovery on a specific client; that never grants
/// [`authorization`].
pub(crate) fn discovery_open(client: &crate::services::coincube::CoincubeClient) -> bool {
    #[cfg(test)]
    if client.ancestry_discovery_opened_for_test() {
        return true;
    }
    #[cfg(not(test))]
    let _ = client;
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_ancestry_stays_closed_pending_acceptance() {
        assert!(authorization().is_none());
        let mut client = crate::services::coincube::CoincubeClient::for_test("http://127.0.0.1:1");
        assert!(!discovery_open(&client));
        client.open_ancestry_discovery_for_test();
        assert!(discovery_open(&client));
        assert!(
            authorization().is_none(),
            "opening discovery never authorizes"
        );
        assert_eq!(
            CLOSED_MESSAGE,
            "Bitcoin-only input signing is not available yet."
        );
    }
}
