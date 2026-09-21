//! Durable change-index allocation, shared by Claim preparation and ordinary spends.
use coincube_core::{chain::ChainId, descriptors::CoincubeDescriptor};
use miniscript::bitcoin::bip32::ChildNumber;

/// A committed allocation, not proof of funds, ownership of keys or spend permission.
/// Dropping this value never releases its index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeReservation {
    pub(crate) chain: ChainId,
    pub(crate) descriptor: CoincubeDescriptor,
    pub(crate) index: ChildNumber,
}

impl ChangeReservation {
    pub fn chain(&self) -> ChainId {
        self.chain
    }
    pub fn descriptor(&self) -> &CoincubeDescriptor {
        &self.descriptor
    }
    pub fn index(&self) -> ChildNumber {
        self.index
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationError {
    Storage,
    IdentityMismatch,
    Exhausted,
    Unsupported,
}
impl std::fmt::Display for ReservationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Storage => "Change reservation could not be durably stored",
            Self::IdentityMismatch => "Change reservation wallet identity mismatch",
            Self::Exhausted => "Change derivation index or lookahead exhausted",
            Self::Unsupported => "Database does not support durable change reservation",
        })
    }
}
impl std::error::Error for ReservationError {}
