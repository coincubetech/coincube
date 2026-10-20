//! Split signing modules and compatibility exports for the shared public
//! hardware account source. Existing Split callers keep their current paths.

pub mod bind;
pub mod flow;
pub mod policy;
pub mod sign;

pub use crate::services::public_hardware_source::{
    account_from_device, parse_account, read_account, view, HardwareAccount, HardwareMessage,
    HardwareSource, HardwareSourceError, HardwareStatus, Purpose, PURPOSES,
};

#[cfg(test)]
pub(crate) use crate::services::public_hardware_source::tests;
