//! Keep ancestry and OP_RETURN constructions distinct during fork admission.
use super::*;
use coincube_core::{descriptors::CoincubeDescriptor, miniscript::bitcoin::psbt::Psbt};

pub(super) enum Source<'a> {
    OpReturn(&'a PoisonSelfTransfer),
    Ancestry(&'a AncestrySelfTransfer),
}
impl<'a> From<&'a PoisonSelfTransfer> for Source<'a> {
    fn from(value: &'a PoisonSelfTransfer) -> Self {
        Self::OpReturn(value)
    }
}
impl<'a> From<&'a AncestrySelfTransfer> for Source<'a> {
    fn from(value: &'a AncestrySelfTransfer) -> Self {
        Self::Ancestry(value)
    }
}
impl Source<'_> {
    pub(super) fn chain(&self) -> ChainId {
        match self {
            Self::OpReturn(s) => s.chain(),
            Self::Ancestry(s) => s.chain(),
        }
    }
    pub(super) fn descriptor(&self) -> &CoincubeDescriptor {
        match self {
            Self::OpReturn(s) => s.descriptor(),
            Self::Ancestry(s) => s.descriptor(),
        }
    }
    pub(super) fn psbt(&self) -> &Psbt {
        match self {
            Self::OpReturn(s) => s.psbt(),
            Self::Ancestry(s) => s.psbt(),
        }
    }
    pub(super) fn revalidate(
        &self,
        controller: &mut Controller,
        context: &Context,
    ) -> Result<(), Error> {
        match self {
            Self::OpReturn(s) => controller.revalidate_construction(context, s)?,
            Self::Ancestry(s) => controller.revalidate_ancestry_construction(context, s)?,
        }
        Ok(())
    }
}
