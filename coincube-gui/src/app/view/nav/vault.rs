use super::{NavContext, SubItem};
use crate::app::menu::{Menu, VaultSubMenu};
use coincube_ui::icon::{
    coins_outline_icon, home_icon, receipt_icon, receive_icon, recovery_icon, send_icon,
    settings_icon,
};

/// Secondary-rail items for the Vault wallet section.
pub fn items(ctx: &NavContext) -> Vec<SubItem> {
    let mut items = vec![
        SubItem::new(
            "Overview",
            home_icon,
            Menu::Vault(VaultSubMenu::Overview),
            |m| matches!(m, Menu::Vault(VaultSubMenu::Overview)),
        ),
        SubItem::new("Send", send_icon, Menu::Vault(VaultSubMenu::Send), |m| {
            matches!(m, Menu::Vault(VaultSubMenu::Send))
        }),
        SubItem::new(
            "Receive",
            receive_icon,
            Menu::Vault(VaultSubMenu::Receive),
            |m| matches!(m, Menu::Vault(VaultSubMenu::Receive)),
        ),
        SubItem::new(
            "Coins",
            coins_outline_icon,
            Menu::Vault(VaultSubMenu::Coins(None)),
            |m| matches!(m, Menu::Vault(VaultSubMenu::Coins(_))),
        ),
        SubItem::new(
            "Transactions",
            receipt_icon,
            Menu::Vault(VaultSubMenu::Transactions(None)),
            |m| matches!(m, Menu::Vault(VaultSubMenu::Transactions(_))),
        ),
        SubItem::new(
            "PSBTs",
            receipt_icon,
            Menu::Vault(VaultSubMenu::PSBTs(None)),
            |m| matches!(m, Menu::Vault(VaultSubMenu::PSBTs(_))),
        ),
        SubItem::new(
            "Recovery",
            recovery_icon,
            Menu::Vault(VaultSubMenu::Recovery),
            |m| matches!(m, Menu::Vault(VaultSubMenu::Recovery)),
        ),
        SubItem::new(
            "Settings",
            settings_icon,
            Menu::Vault(VaultSubMenu::Settings(Some(
                crate::app::menu::SettingsOption::Node,
            ))),
            |m| matches!(m, Menu::Vault(VaultSubMenu::Settings(_))),
        ),
    ];
    // Bitcoin Blake2b claim entry. Hidden rather than disabled when the
    // account has no fork grant: an account without it has no BTCB2 at all,
    // which is not a per-Cube state the user can act on. Inserted before
    // Settings so the rail keeps Settings last.
    if crate::app::features::claim_blake2b(ctx.claim_source_cube()).is_available() {
        let settings_at = items.len().saturating_sub(1);
        items.insert(
            settings_at,
            SubItem::new(
                "Claim BTCB2",
                recovery_icon,
                Menu::Vault(VaultSubMenu::Claim),
                |m| matches!(m, Menu::Vault(VaultSubMenu::Claim)),
            ),
        );
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(
        status: &'a crate::app::ConnectionStatus,
        network: crate::chain::ChainId,
        btcb2_server_enabled: bool,
        btcb2_already_claimed: bool,
    ) -> NavContext<'a> {
        NavContext {
            has_vault: true,
            has_p2p: false,
            network,
            p2p_test_coordinator: false,
            marketplace_flags: crate::app::features::MarketplaceServerFlags::OFF,
            liquid_gate: crate::app::features::LiquidGate::HIDDEN,
            cube_name: "test cube",
            lightning_address: None,
            avatar: None,
            theme_mode: coincube_ui::theme::palette::ThemeMode::default(),
            connect_authenticated: btcb2_server_enabled,
            connect_stream_status: status,
            btcb2_server_enabled,
            btcb2_already_claimed,
        }
    }

    fn labels(items: &[SubItem]) -> Vec<&str> {
        items.iter().map(|i| i.label).collect()
    }

    /// With the account grant the claim entry appears, immediately before
    /// Settings — and without it the rail is byte-for-byte what it was.
    #[test]
    fn the_claim_entry_appears_only_with_the_account_grant_and_keeps_settings_last() {
        let status = crate::app::ConnectionStatus::default();
        let granted = items(&ctx(&status, crate::chain::ChainId::Bitcoin, true, false));
        assert_eq!(
            labels(&granted),
            [
                "Overview",
                "Send",
                "Receive",
                "Coins",
                "Transactions",
                "PSBTs",
                "Recovery",
                "Claim BTCB2",
                "Settings",
            ]
        );

        // Flag off: no BTCB2 surface anywhere on the rail.
        let ungranted = items(&ctx(&status, crate::chain::ChainId::Bitcoin, false, false));
        assert_eq!(
            labels(&ungranted),
            [
                "Overview",
                "Send",
                "Receive",
                "Coins",
                "Transactions",
                "PSBTs",
                "Recovery",
                "Settings",
            ]
        );

        // Already claimed: the item stays — the claim itself (step 1, the
        // poison self-transfer) happens after the target exists, and the App
        // routes the item to the step-1 panel instead of the installer
        // (`features::claim_entry`).
        assert_eq!(
            labels(&items(&ctx(
                &status,
                crate::chain::ChainId::Bitcoin,
                true,
                true
            ))),
            labels(&granted)
        );
        // A non-Bitcoin Cube: no claim surface, answered by
        // `features::claim_blake2b`.
        assert_eq!(
            labels(&items(&ctx(
                &status,
                crate::chain::ChainId::BitcoinBlake2b,
                true,
                false
            ))),
            labels(&ungranted)
        );
    }
}
