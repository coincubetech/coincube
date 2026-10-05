//! The typed seed text of the single-step route (#568 B4b-3c). This module
//! is private to `split::unified`, so the `Zeroizing` buffer below is
//! private to this impl: everything else reaches the text only through
//! these methods, which the panel and the view use exactly as
//! `split_unified_holds_seeds_only_zeroized` pins (Reviewer-660661e D2,
//! decided by Robert: structural, not another grep rule).

use std::fmt;

use zeroize::Zeroizing;

/// Text typed into a `.secure(true)` seed input: zeroized when replaced or
/// dropped, never printed.
#[derive(Clone, Default)]
pub struct SeedText(Zeroizing<String>);

impl SeedText {
    /// The typed text, lent to the view's one `.secure(true)` input and to
    /// nothing else (`split_unified_holds_seeds_only_zeroized`).
    pub(in crate::app) fn expose_for_secure_input(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Move the text out, leaving the buffer empty: the seed set's input.
    pub(super) fn take(&mut self) -> Zeroizing<String> {
        std::mem::take(&mut self.0)
    }
    /// Empty the buffer; the old bytes are zeroized as they drop.
    pub(super) fn clear(&mut self) {
        self.0 = Zeroizing::default();
    }
}

impl From<String> for SeedText {
    fn from(text: String) -> Self {
        Self(Zeroizing::new(text))
    }
}

impl fmt::Debug for SeedText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SeedText(<redacted>)")
    }
}
