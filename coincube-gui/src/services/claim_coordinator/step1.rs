//! Preserve the poison mechanism through coordinator transport dispatch.
use super::*;
use coincube_core::{
    claim_finalize::VerifiedAncestryTransfer, foreign_split::VerifiedSplitStep1,
    miniscript::bitcoin::Amount,
};

#[derive(Clone)]
pub(super) enum VerifiedStep1 {
    OpReturn(Arc<VerifiedPoisonTransfer>),
    Ancestry(Arc<VerifiedAncestryTransfer>),
    /// Split (#568) step 1: a foreign wallet's signed step 1, submitted only
    /// through the Split production's daemonless Connect route.
    Split(Arc<VerifiedSplitStep1>),
}
impl VerifiedStep1 {
    pub(super) fn transaction(&self) -> &Transaction {
        match self {
            Self::OpReturn(v) => v.transaction(),
            Self::Ancestry(v) => v.transaction(),
            Self::Split(v) => v.transaction(),
        }
    }
    pub(super) fn fee(&self) -> Amount {
        match self {
            Self::OpReturn(v) => v.fee(),
            Self::Ancestry(v) => v.fee(),
            Self::Split(v) => v.fee(),
        }
    }
    pub(super) fn vsize(&self) -> usize {
        match self {
            Self::OpReturn(v) => v.vsize(),
            Self::Ancestry(v) => v.vsize(),
            Self::Split(v) => v.vsize(),
        }
    }
    pub(super) fn gate(&self, deadline: Instant) -> (SubmissionGate, SubmissionRevoker) {
        match self {
            Self::OpReturn(v) => SubmissionGate::new(v, deadline),
            Self::Ancestry(v) => SubmissionGate::for_ancestry(v, deadline),
            Self::Split(v) => SubmissionGate::for_split_step1(v, deadline),
        }
    }
}
