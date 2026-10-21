//! Read-only, deliberately closed protocol. No wallet engine or signing types.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};

#[cfg(feature = "adapter")]
pub mod adapter;
#[cfg(unix)]
pub mod ipc;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Empty {}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletInput {
    #[schemars(length(min = 1, max = 64))]
    pub wallet_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryInput {
    #[schemars(length(min = 1, max = 64))]
    pub wallet_id: String,
    /// Maximum confirmed transactions, 1 through 50.
    #[schemars(range(min = 1, max = 50))]
    pub limit: u8,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", content = "input", deny_unknown_fields)]
pub enum Request {
    List(Empty),
    Summary(WalletInput),
    History(HistoryInput),
    Policy(WalletInput),
}
impl Request {
    pub fn scope(&self) -> Option<(&str, usize)> {
        match self {
            Self::List(_) => None,
            Self::Summary(v) => Some((&v.wallet_id, 0)),
            Self::History(v) => Some((&v.wallet_id, 1)),
            Self::Policy(v) => Some((&v.wallet_id, 2)),
        }
    }
    pub fn valid(&self) -> bool {
        self.scope()
            .is_none_or(|(id, _)| !id.is_empty() && id.len() <= 64)
            && !matches!(self, Self::History(v) if v.limit == 0 || v.limit > 50)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletMetadata {
    pub wallet_id: String,
    pub display_name: String,
    pub wallet_type: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WalletList {
    pub wallets: Vec<WalletMetadata>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Summary {
    pub wallet_id: String,
    pub network: String,
    pub wallet_type: String,
    /// Confirmed coins excluding coins already being spent.
    pub confirmed_sats: u64,
    pub unconfirmed_sats: u64,
    /// Coins eligible under the existing UI coin-selection predicate.
    pub available_sats: u64,
    /// Data is a daemon snapshot, not a guarantee of completeness.
    pub sync_state: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Transaction {
    pub txid: String,
    pub incoming_sats: u64,
    pub outgoing_sats: u64,
    pub timestamp: Option<u32>,
    pub height: Option<i32>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct History {
    pub wallet_id: String,
    pub confirmed_only: bool,
    pub transactions: Vec<Transaction>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PolicyPath {
    pub threshold: usize,
    pub key_count: usize,
    pub relative_delay_blocks: u16,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Policy {
    pub wallet_id: String,
    pub deterministic: bool,
    pub script_type: String,
    pub primary: PolicyPath,
    pub recovery: Vec<PolicyPath>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum Response {
    List(WalletList),
    Summary(Summary),
    History(History),
    Policy(Policy),
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Error {
    AccessDenied,
    Unavailable,
    InvalidInput,
}
impl Error {
    pub fn code(self) -> &'static str {
        match self {
            Self::AccessDenied => "access_denied",
            Self::Unavailable => "wallet_unavailable",
            Self::InvalidInput => "invalid_input",
        }
    }
}
/// The desktop supplies this implementation; credentials never reach the wallet service.
pub trait Reader: Send + Sync + 'static {
    fn read(
        &self,
        request: Request,
    ) -> Pin<Box<dyn Future<Output = Result<Response, Error>> + Send + '_>>;
}
