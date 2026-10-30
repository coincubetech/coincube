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
//!
//! An explicit `--enable-btcb2-split` launch enables paired live testing for
//! that process only. Owner authorization for this local testing was given
//! on 2026-10-19; it is not evidence of completed hardware acceptance.

// Paired signer entry and both paired submission reviews consume this token.
// Changing it requires the acceptance evidence documented above.
const ENABLED: bool = false;

/// An unforgeable authorization token for production ancestry side effects.
#[allow(dead_code)] // Paired signer and submission remain gated in production.
pub(crate) struct Authorization(());

#[allow(dead_code)] // Paired signer and submission remain gated in production.
pub(crate) fn authorization() -> Option<Authorization> {
    if ENABLED || crate::live_split::enabled() {
        return Some(Authorization(()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_ancestry_stays_closed_pending_acceptance() {
        assert!(authorization().is_none());
    }
}
