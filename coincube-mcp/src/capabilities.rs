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
    /// Implemented, bounded confirmed-history request limit.
    pub max_confirmed_transactions: u8,
    /// End-to-end proposal workflow through native consent; not a grant.
    /// False while only the underlying protocol/service is staged.
    pub wallet_proposals: bool,
    /// Whether an MCP call can create a wallet. Always false for this design.
    pub wallet_creation_via_mcp: bool,
    /// Whether the MCP surface exports public keys or descriptors.
    pub public_key_material: bool,
    /// Whether the MCP surface exports secrets or offers signing/broadcast.
    pub secrets_or_signing: bool,
}
impl Capabilities {
    /// Describe only operations actually implemented in this build.
    pub fn current() -> Self {
        Self {
            schema_version: 1,
            wallet_family: "bitcoin_vault_embedded".into(),
            reads_require_existing_vault: true,
            wallet_data_requires_consent: true,
            max_confirmed_transactions: 50,
            wallet_proposals: false,
            wallet_creation_via_mcp: false,
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
