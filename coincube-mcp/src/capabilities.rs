//! Public build metadata, never a view of installed wallets or live permission.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Stable names for the three existing wallet-read permissions.
///
/// New operations must receive separate permissions; they cannot consume an
/// unused index or reinterpret an existing read grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadCapability {
    /// Read balances and synchronization state.
    Balances,
    /// Read bounded confirmed transaction history.
    ConfirmedHistory,
    /// Read deterministic signing-path structure.
    PolicySummary,
}

/// Explicit read permissions owned by the trusted desktop, not tool arguments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadPermissions {
    /// Balance/sync reads are allowed.
    pub balances: bool,
    /// Confirmed-history reads are allowed.
    pub confirmed_history: bool,
    /// Policy-summary reads are allowed.
    pub policy_summary: bool,
}
impl ReadPermissions {
    /// Preserve the legacy desktop selection order without adding authority.
    pub fn from_legacy(selected: [bool; 3]) -> Self {
        Self {
            balances: selected[0],
            confirmed_history: selected[1],
            policy_summary: selected[2],
        }
    }

    /// Check one explicitly named permission.
    pub fn permits(self, capability: ReadCapability) -> bool {
        match capability {
            ReadCapability::Balances => self.balances,
            ReadCapability::ConfirmedHistory => self.confirmed_history,
            ReadCapability::PolicySummary => self.policy_summary,
        }
    }
}

/// Non-sensitive description of this build's implemented MCP surface.
///
/// This is static metadata. It does not report whether a Cube exists, is
/// unlocked, has a Vault or has consented to a particular operation.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// Version of this capability document, not the negotiated MCP version.
    pub schema_version: u32,
    /// Supported wallet/backend family, not an installed-wallet inventory.
    pub wallet_family: String,
    /// Wallet-data reads need a currently authorized existing Vault.
    pub reads_require_existing_vault: bool,
    /// Every wallet-data operation needs explicit live desktop consent.
    pub wallet_data_requires_consent: bool,
    /// Desktop can persist explicitly scoped client pairing across unlocks.
    /// This is implementation metadata, never current client authorization.
    pub persistent_client_pairing: bool,
    /// Implemented, bounded confirmed-history request limit.
    pub max_confirmed_transactions: u8,
    /// End-to-end proposal workflow through native consent; not a grant.
    /// False while only the underlying protocol/service is staged.
    pub wallet_proposals: bool,
    /// Static first-demo constraints, absent in ordinary builds. Never consent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_support: Option<ProposalSupport>,
    /// Whether an MCP call can create a wallet. Always false for this design.
    pub wallet_creation_via_mcp: bool,
    /// Unsigned native transaction preparation is implemented in this build.
    pub transaction_preparation: bool,
    /// Explicit first-slice limits, not current client permission.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction_support: Option<TransactionSupport>,
    /// Whether the MCP surface exports public keys or descriptors.
    pub public_key_material: bool,
    /// Whether the MCP surface exports secrets or offers signing/broadcast.
    pub secrets_or_signing: bool,
}
/// Non-sensitive protocol limits and human checkpoints for the first demo.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProposalSupport {
    /// Only an isolated Bitcoin Regtest Cube with no Vault/import is eligible.
    pub network: String,
    /// Fixed supported primary threshold.
    pub primary_threshold: u8,
    /// Fixed supported primary key count; not verified independent owners.
    pub primary_key_count: u8,
    /// The assistant must discuss and explicitly select recovery, never omit it.
    pub recovery_required: bool,
    /// Maximum keys in the explicitly chosen recovery path.
    pub max_recovery_keys: u8,
    /// Relative positive delay in blocks, not a guaranteed calendar duration.
    pub max_relative_delay_blocks: u16,
    /// Whether every proposal requires manual public-key entry in Tenshu.
    /// False when separately consented public-key assembly is supported.
    pub native_key_selection_required: bool,
    /// A separately paired setup client can supply public account-key assignments.
    pub public_key_assembly: bool,
    /// Compilation never verifies physical devices, custody or backup possession.
    pub manual_key_verification_required: bool,
    /// Actual descriptor review and creation approval happen only inside Tenshu.
    pub native_creation_authorization_required: bool,
    /// No automated assertion of key ownership or a successful backup rehearsal.
    pub key_or_backup_verification: String,
    /// The owner's Cube Recovery Kit encryption key is never a Vault signer.
    pub owner_recovery_key_is_vault_signer: bool,
}
/// Regtest unsigned-payment constraints; signing and broadcasting stay native.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TransactionSupport {
    /// Only Bitcoin Regtest is eligible.
    pub network: String,
    /// Exact integer satoshis only; no implicit fiat conversion.
    pub amount_unit: String,
    /// Bounded memory per paired client and unlock.
    pub max_drafts_per_session: usize,
    /// Bounded recipient count; self-send is excluded.
    pub max_recipients: usize,
    /// Caller must supply a total-fee ceiling.
    pub explicit_fee_limit_required: bool,
    /// Complete unsigned PSBT stays in Tenshu.
    pub psbt_export_via_mcp: bool,
    /// Native review and user signing are required.
    pub native_signing_required: bool,
}
impl Capabilities {
    /// Describe only operations actually implemented in this build.
    pub fn current() -> Self {
        Self {
            schema_version: 1,
            wallet_family: "bitcoin_vault_embedded".into(),
            reads_require_existing_vault: true,
            wallet_data_requires_consent: true,
            persistent_client_pairing: true,
            max_confirmed_transactions: 50,
            wallet_proposals: cfg!(feature = "native-proposals"),
            proposal_support: cfg!(feature = "native-proposals").then(|| ProposalSupport {
                network: "regtest".into(),
                primary_threshold: 3,
                primary_key_count: 5,
                recovery_required: true,
                max_recovery_keys: 5,
                max_relative_delay_blocks: 65535,
                native_key_selection_required: false,
                public_key_assembly: true,
                manual_key_verification_required: true,
                native_creation_authorization_required: true,
                key_or_backup_verification: "manual_required".into(),
                owner_recovery_key_is_vault_signer: false,
            }),
            wallet_creation_via_mcp: false,
            transaction_preparation: cfg!(feature = "native-transactions"),
            transaction_support: cfg!(feature = "native-transactions").then(|| {
                TransactionSupport {
                    network: "regtest".into(),
                    amount_unit: "sats".into(),
                    max_drafts_per_session: crate::transaction::MAX_DRAFTS,
                    max_recipients: crate::transaction::MAX_RECIPIENTS,
                    explicit_fee_limit_required: true,
                    psbt_export_via_mcp: false,
                    native_signing_required: true,
                }
            }),
            public_key_material: false,
            secrets_or_signing: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_permission_selections_preserve_all_eight_combinations() {
        for bits in 0..8 {
            let selected = [bits & 1 != 0, bits & 2 != 0, bits & 4 != 0];
            let permissions = ReadPermissions::from_legacy(selected);
            assert_eq!(permissions.permits(ReadCapability::Balances), selected[0]);
            assert_eq!(
                permissions.permits(ReadCapability::ConfirmedHistory),
                selected[1]
            );
            assert_eq!(
                permissions.permits(ReadCapability::PolicySummary),
                selected[2]
            );
        }
        assert_eq!(
            ReadPermissions::default(),
            ReadPermissions::from_legacy([false; 3])
        );
    }
}
