//! Chain-observation types shared by the input-ancestry engine and Split
//! coins. Nothing here authenticates a server, verifies chain work or
//! inclusion, signs or broadcasts.
//!
//! The Claim observation assessment (the OP_RETURN poison plan, its RDTS
//! deployment checks and the confirmation gate between the two sides) was
//! removed with the paired Claim and Split flows; Split coins makes one-sided
//! splits (company-brain decision 2026-10-29, "split protection in Coin
//! control").

use miniscript::bitcoin::BlockHash;
use serde::{Deserialize, Serialize};

/// Depth at which a split counts as having separated the coins it spent.
pub const MIN_CONFIRMATIONS: u64 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRef {
    pub height: u64,
    pub hash: BlockHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// The oldest an observation may be, in seconds. Always positive.
    pub max_observation_age_seconds: i64,
}
