//! "Start a claim as soon as this Cube is open."
//!
//! The Home card that starts a Bitcoin Blake2b claim is pressed *before* the
//! source Cube is unlocked — the claim needs its descriptor and its master
//! seed, and Home has neither. So the card records an intent, the ordinary
//! unlock runs, and the freshly built `App` consumes it.
//!
//! One Cube id, not a queue: a second press replaces the first, and opening
//! any other Cube clears it. It holds no secret — only which Cube the user
//! pressed the card on — and it is never permission: `App` re-checks
//! [`crate::app::features::claim_blake2b`] before the installer starts, and
//! `Installer::try_new_for_chain` re-checks the account gate after that.

use std::sync::{Mutex, OnceLock};

fn cell() -> &'static Mutex<Option<String>> {
    static INTENT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    INTENT.get_or_init(|| Mutex::new(None))
}

/// Record that the next open of `cube_id` should start a claim.
pub fn arm(cube_id: impl Into<String>) {
    if let Ok(mut slot) = cell().lock() {
        *slot = Some(cube_id.into());
    }
}

/// Consume the intent if it was armed for this Cube.
///
/// Takes on *any* Cube open, matching or not: an intent that survived the user
/// changing their mind and opening something else would fire on a later,
/// unrelated open.
pub fn take(cube_id: &str) -> bool {
    match cell().lock() {
        Ok(mut slot) => slot.take().is_some_and(|armed| armed == cube_id),
        Err(_) => false,
    }
}

/// Drop any armed intent. Called on lock and duress alongside the session.
pub fn clear() {
    if let Ok(mut slot) = cell().lock() {
        *slot = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One global cell, so the cases run in one test rather than racing.
    #[test]
    fn an_intent_fires_once_for_its_own_cube_and_never_for_another() {
        clear();
        assert!(!take("cube-a"), "nothing armed");

        arm("cube-a");
        assert!(take("cube-a"), "armed for this Cube");
        assert!(!take("cube-a"), "and only once");

        arm("cube-a");
        assert!(!take("cube-b"), "opening another Cube consumes it");
        assert!(!take("cube-a"), "without firing later");

        arm("cube-a");
        arm("cube-b");
        assert!(!take("cube-a"), "a second press replaces the first");
        clear();
    }
}
