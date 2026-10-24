//! Closed wallet reads and session-only topology drafts. No signing or wallet engine.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};

pub mod capabilities;
pub mod pairing;
pub use capabilities::{Capabilities, ReadCapability, ReadPermissions};
pub mod proposal;

#[cfg(feature = "adapter")]
pub mod adapter;
#[cfg(any(unix, windows))]
pub mod ipc;

/// Empty arguments for listing the current grant's authorized wallet.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Empty {}
/// Opaque wallet identity returned by this session's authorized wallet list.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletInput {
    /// Client-scoped opaque identifier; never a descriptor, filesystem path or account name.
    #[schemars(length(min = 1, max = 64))]
    pub wallet_id: String,
}
/// Bounded request for the authorized wallet's confirmed history.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryInput {
    /// Client-scoped opaque identifier obtained from the authorized wallet list.
    #[schemars(length(min = 1, max = 64))]
    pub wallet_id: String,
    /// Maximum confirmed transactions, 1 through 50.
    #[schemars(range(min = 1, max = 50))]
    pub limit: u8,
}
/// Closed read-only dispatch vocabulary accepted by the desktop IPC boundary.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", content = "input", deny_unknown_fields)]
pub enum Request {
    /// List only the wallet included in the live grant.
    List(Empty),
    /// Read balance and synchronization metadata for the granted wallet.
    Summary(WalletInput),
    /// Read a bounded list of confirmed transactions for the granted wallet.
    History(HistoryInput),
    /// Read deterministic policy structure without exposing keys or descriptors.
    Policy(WalletInput),
    /// Prepare only a session-local topology draft; never install a wallet.
    PrepareProposal(proposal::PrepareInput),
    /// Assemble public account keys; requires a separate creation-preparation grant.
    PrepareCreation(proposal::CreationInput),
    /// Read the current preparation grant's topology draft.
    Proposal(proposal::ProposalInput),
}
impl Request {
    /// Return wallet scope and its explicitly named read capability.
    ///
    /// Listing has no caller-selected wallet, but still requires a live grant.
    pub fn scope(&self) -> Option<(&str, ReadCapability)> {
        match self {
            Self::List(_) => None,
            Self::Summary(v) => Some((&v.wallet_id, ReadCapability::Balances)),
            Self::History(v) => Some((&v.wallet_id, ReadCapability::ConfirmedHistory)),
            Self::Policy(v) => Some((&v.wallet_id, ReadCapability::PolicySummary)),
            Self::PrepareProposal(_) | Self::PrepareCreation(_) | Self::Proposal(_) => None,
        }
    }
    /// Validate wallet-ID byte length and the 1–50 history limit at runtime.
    pub fn valid(&self) -> bool {
        self.scope()
            .is_none_or(|(id, _)| !id.is_empty() && id.len() <= 64)
            && !matches!(self, Self::History(v) if v.limit == 0 || v.limit > 50)
            && match self {
                Self::PrepareProposal(v) => v.valid(),
                Self::PrepareCreation(v) => v.valid(),
                Self::Proposal(v) => v.valid(),
                _ => true,
            }
    }
}
/// Minimal public metadata for a wallet explicitly included in the grant.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WalletMetadata {
    /// Opaque identity valid only in the current authorized session.
    pub wallet_id: String,
    /// Generic wallet name; user-authored labels are not exported.
    pub display_name: String,
    /// Supported wallet category, currently `vault`.
    pub wallet_type: String,
}
/// Wallets visible to this grant, rather than a catalog of installed wallets.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WalletList {
    /// Only explicitly authorized wallets in the current session.
    pub wallets: Vec<WalletMetadata>,
}
/// Existing wallet-engine balance snapshot; amounts are integer satoshis.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Summary {
    /// Opaque identity of the authorized wallet.
    pub wallet_id: String,
    /// Bitcoin network reported by the active embedded daemon.
    pub network: String,
    /// Supported wallet category, currently `vault`.
    pub wallet_type: String,
    /// Confirmed coins excluding coins already being spent.
    pub confirmed_sats: u64,
    /// Unconfirmed coins excluding coins already being spent.
    pub unconfirmed_sats: u64,
    /// Coins eligible under the existing UI coin-selection predicate.
    pub available_sats: u64,
    /// Data is a daemon snapshot, not a guarantee of completeness.
    pub sync_state: String,
}
/// Minimized transaction projection using the existing UI's amount semantics.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Transaction {
    /// Public transaction identifier, without raw transaction or script data.
    pub txid: String,
    /// Incoming amount as represented by the existing wallet UI, in satoshis.
    pub incoming_sats: u64,
    /// Wallet spending to non-wallet outputs, in satoshis; zero for incoming payments.
    pub outgoing_sats: u64,
    /// Confirmation timestamp when available, in Unix seconds.
    pub timestamp: Option<u32>,
    /// Confirmation height when available.
    pub height: Option<i32>,
}
/// Bounded confirmed-history result without addresses, scripts or labels.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct History {
    /// Opaque identity of the authorized wallet.
    pub wallet_id: String,
    /// Whether this result excludes unconfirmed transactions; currently true.
    pub confirmed_only: bool,
    /// At most the requested limit of most recent confirmed transactions.
    pub transactions: Vec<Transaction>,
}
/// Deterministic signing-path structure, not proof of present spend eligibility.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PolicyPath {
    /// Required signatures for this path.
    pub threshold: usize,
    /// Policy key count, not independently verified people or devices.
    pub key_count: usize,
    /// Relative recovery delay in blocks; zero for the primary path.
    pub relative_delay_blocks: u16,
}
/// Public policy analysis without descriptors, extended keys or signing material.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Policy {
    /// Opaque identity of the authorized wallet.
    pub wallet_id: String,
    /// Whether these fields come from deterministic descriptor analysis.
    pub deterministic: bool,
    /// Script category, currently `p2tr` or `p2wsh`.
    pub script_type: String,
    /// Primary signing path without a recovery delay.
    pub primary: PolicyPath,
    /// Recovery paths and their relative block delays.
    pub recovery: Vec<PolicyPath>,
}
/// Closed result vocabulary crossing the authenticated desktop IPC boundary.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum Response {
    /// Authorized-wallet metadata.
    List(WalletList),
    /// Authorized-wallet balance snapshot.
    Summary(Summary),
    /// Authorized-wallet confirmed history.
    History(History),
    /// Authorized-wallet deterministic policy summary.
    Policy(Policy),
    /// Session-only topology proposal, not a descriptor or created wallet.
    Proposal(proposal::Proposal),
}
/// Sanitized boundary errors that never carry raw daemon or credential details.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Error {
    /// Missing, invalid, expired, revoked or out-of-scope authorization.
    AccessDenied,
    /// The wallet, session, transport or bounded read is unavailable.
    Unavailable,
    /// Arguments or a frame violate the closed request contract.
    InvalidInput,
    /// This session already holds a different proposal; no second draft was made.
    ProposalConflict,
}
impl Error {
    /// Return the stable machine-readable code exposed in MCP tool errors.
    pub fn code(self) -> &'static str {
        match self {
            Self::AccessDenied => "access_denied",
            Self::Unavailable => "wallet_unavailable",
            Self::InvalidInput => "invalid_input",
            Self::ProposalConflict => "proposal_conflict",
        }
    }
}
/// The desktop supplies this closed service; credentials never reach it.
/// The historical name includes read operations and bounded in-memory drafts,
/// not wallet mutation. IPC authorization distinguishes these scopes explicitly.
pub trait Reader: Send + Sync + 'static {
    /// Handle an already-authorized typed read or session-only draft request.
    ///
    /// The IPC server checks the grant before and after this future completes;
    /// implementations return only the minimized response types above.
    fn read(
        &self,
        request: Request,
    ) -> Pin<Box<dyn Future<Output = Result<Response, Error>> + Send + '_>>;
}
