//! Bounded unsigned-transaction requests. The PSBT never crosses this boundary.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Maximum recipients in one preparation.
pub const MAX_RECIPIENTS: usize = 8;
/// Maximum engine construction attempts per paired client and unlocked session, including failures.
pub const MAX_DRAFTS: usize = 8;
/// Bitcoin's total supply, expressed in satoshis.
pub const MAX_SATS: u64 = 2_100_000_000_000_000;

/// A bare Bitcoin address and an exact amount; never a URL or payment request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Recipient {
    /// Bare address, checked for the active Regtest network by Tenshu.
    #[schemars(length(min = 14, max = 90))]
    pub address: String,
    /// Exact integer satoshis, with dust checked by the wallet engine.
    #[schemars(range(min = 1, max = 2_100_000_000_000_000_u64))]
    pub amount_sats: u64,
}

/// Requires a separately paired transaction-preparation scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrepareInput {
    /// Opaque identity from this pairing's authorized wallet list.
    #[schemars(length(min = 1, max = 64))]
    pub wallet_id: String,
    /// Retry identity; changed input with the same identity is rejected.
    #[schemars(length(min = 1, max = 64))]
    pub request_id: String,
    /// At most eight distinct bare addresses. No send-max or caller change address.
    #[schemars(length(min = 1, max = 8))]
    pub recipients: Vec<Recipient>,
    /// Explicit integer fee rate in satoshis per virtual byte.
    #[schemars(range(min = 1, max = 1000))]
    pub fee_rate_sat_vb: u64,
    /// Reject a construction whose total fee exceeds this ceiling.
    #[schemars(range(min = 1, max = 2_100_000_000_000_000_u64))]
    pub max_fee_sats: u64,
}
impl PrepareInput {
    /// Runtime bounds apply independently of the published JSON schema.
    pub fn valid(&self) -> bool {
        id_valid(&self.wallet_id)
            && id_valid(&self.request_id)
            && (1..=MAX_RECIPIENTS).contains(&self.recipients.len())
            && (1..=1000).contains(&self.fee_rate_sat_vb)
            && (1..=MAX_SATS).contains(&self.max_fee_sats)
            && self.recipients.iter().all(|r| {
                (14..=90).contains(&r.address.len())
                    && r.address.bytes().all(|b| b.is_ascii_alphanumeric())
                    && (1..=MAX_SATS).contains(&r.amount_sats)
            })
            && self
                .recipients
                .iter()
                .try_fold(self.max_fee_sats, |sum, r| sum.checked_add(r.amount_sats))
                .is_some_and(|sum| sum <= MAX_SATS)
    }
}
fn id_valid(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Read only a draft from the current transaction pairing and session.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftInput {
    /// Pairing-scoped wallet identity.
    #[schemars(length(min = 1, max = 64))]
    pub wallet_id: String,
    /// Opaque draft identity returned by preparation.
    #[schemars(length(min = 1, max = 64))]
    pub draft_id: String,
}
impl DraftInput {
    /// Reject unbounded or non-opaque selectors.
    pub fn valid(&self) -> bool {
        id_valid(&self.wallet_id) && id_valid(&self.draft_id)
    }
}

/// Minimized summary. No PSBT, input outpoints, change address or signing material.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Prepared {
    /// Pairing-scoped wallet identity.
    pub wallet_id: String,
    /// Session-local native review identity.
    pub draft_id: String,
    /// Always regtest for this experiment.
    pub network: String,
    /// Always unsigned_ready_for_native_review; not signed, saved or broadcast.
    pub status: String,
    /// Only the addresses and amounts supplied in the request.
    pub recipients: Vec<Recipient>,
    /// Sum of the requested recipient amounts.
    pub amount_sats: u64,
    /// Actual total fee of this unsigned construction.
    pub fee_sats: u64,
    /// Requested fee rate, not a claim about future confirmation time.
    pub fee_rate_sat_vb: u64,
    /// Total change amount, without revealing its address.
    pub change_sats: u64,
    /// Input count, without input identities.
    pub input_count: usize,
    /// Explicitly true: native review and user signing are still required.
    pub signing_required: bool,
}
