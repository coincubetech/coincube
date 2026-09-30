//! Preserve the poison mechanism through coordinator transport dispatch.
use super::*;
use coincube_core::{claim_finalize::VerifiedAncestryTransfer, miniscript::bitcoin::Amount};

#[derive(Clone)]
pub(super) enum VerifiedStep1 {
    OpReturn(Arc<VerifiedPoisonTransfer>),
    Ancestry(Arc<VerifiedAncestryTransfer>),
}
impl VerifiedStep1 {
    pub(super) fn transaction(&self) -> &Transaction {
        match self {
            Self::OpReturn(v) => v.transaction(),
            Self::Ancestry(v) => v.transaction(),
        }
    }
    pub(super) fn fee(&self) -> Amount {
        match self {
            Self::OpReturn(v) => v.fee(),
            Self::Ancestry(v) => v.fee(),
        }
    }
    pub(super) fn vsize(&self) -> usize {
        match self {
            Self::OpReturn(v) => v.vsize(),
            Self::Ancestry(v) => v.vsize(),
        }
    }
    pub(super) fn gate(&self, deadline: Instant) -> (SubmissionGate, SubmissionRevoker) {
        match self {
            Self::OpReturn(v) => SubmissionGate::new(v, deadline),
            Self::Ancestry(v) => SubmissionGate::for_ancestry(v, deadline),
        }
    }
}
