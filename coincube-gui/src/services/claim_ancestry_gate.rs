//! Final production gate for Bitcoin-only-input Claim ancestry.
//!
//! The proof, signing preparation, reorg handling and two-chain harness remain
//! compiled and testable while this gate is closed. Opening it authorizes both
//! GUI signer dispatch and creation of new durable completion metadata; those
//! two capabilities must move together so a signed ancestry Claim cannot finish
//! without restart-safe completion tracking.
//!
//! Keep this closed until issue #547 records owner acceptance of the live
//! two-chain flow and its independent final review. `InputProofUnsupported` in
//! the generic core assessment remains a separate fail-closed boundary: only
//! the typed, freshly verified ancestry path may reach this gate.

/// User-facing refusal retained while production ancestry is awaiting approval.
pub(crate) const CLOSED_MESSAGE: &str = "Bitcoin-only input signing is not available yet.";

// This is the only production switch for ancestry signer dispatch and durable
// completion. Changing it requires the acceptance evidence documented above.
const ENABLED: bool = false;

/// An unforgeable authorization token for production ancestry side effects.
pub(crate) struct Authorization(());

pub(crate) fn authorization() -> Option<Authorization> {
    ENABLED.then_some(Authorization(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_ancestry_stays_closed_pending_acceptance() {
        assert!(authorization().is_none());
        assert_eq!(
            CLOSED_MESSAGE,
            "Bitcoin-only input signing is not available yet."
        );
    }
}
