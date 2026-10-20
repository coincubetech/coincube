//! Explicit, process-local opt-in for testing the paired BTCB2 workflow.
//! This changes availability only. Proofs, transaction reviews, device
//! approval and separate broadcast confirmations are still required.

use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Called at startup only when the user supplies `--enable-btcb2-split`.
/// Nothing is persisted or enabled for subsequent launches.
pub fn enable_for_this_process() {
    ENABLED.store(true, Ordering::SeqCst);
}

pub(crate) fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}
