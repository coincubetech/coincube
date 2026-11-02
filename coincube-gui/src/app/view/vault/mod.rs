pub(crate) mod label;

pub mod coins;
pub mod export;
pub mod fiat;
pub mod hw;
pub mod overview;
pub mod psbt;
pub mod psbts;
pub mod receive;
pub mod recovery;
pub mod settings;
pub mod spend;
pub mod split_coins;
pub mod tools;
pub mod transactions;
pub mod warning;

/// The pinned head of a Vault sub-page such as Send or Receive: `title`, the
/// wallet line, then Back to Vault as the last action (W4).
pub fn sub_page_head<'a>(
    cache: &crate::app::cache::Cache,
    title: &'a str,
) -> coincube_ui::component::tenshu::PageHead<'a, crate::app::view::Message> {
    crate::app::view::vault_page_head(cache, title).back(
        "Vault",
        crate::app::view::Message::Menu(crate::app::menu::Menu::Vault(
            crate::app::menu::VaultSubMenu::Overview,
        )),
    )
}

#[cfg(test)]
mod sub_page_head_tests {
    use crate::app::menu::{Menu, SparkSubMenu, VaultSubMenu};
    use crate::app::state::preferences::BalancePrivacy;
    use crate::app::view::{nav::shell::capture_tests, shared::landing::capture, Message};

    /// Each wallet's Send and Receive heads end with "Back to ‹wallet›",
    /// which opens that wallet's page (W4).
    #[test]
    fn sub_page_heads_go_back_to_their_wallet() {
        let cache = capture_tests::cache(false, BalancePrivacy::Shown);
        for (wallet, head, overview) in [
            (
                "Vault",
                super::sub_page_head(&cache, "Send"),
                Menu::Vault(VaultSubMenu::Overview),
            ),
            (
                "Spark",
                crate::app::view::spark::sub_page_head(&cache, "Receive"),
                Menu::Spark(SparkSubMenu::Overview),
            ),
        ] {
            let emitted = capture::activate(
                iced::widget::Column::new()
                    .push(head.view(crate::app::view::page_gutter))
                    .into(),
                coincube_ui::component::tenshu::page_head::BACK_ID.into(),
                (1440, 900),
                false,
            );
            assert!(
                emitted
                    .iter()
                    .any(|message| matches!(message, Message::Menu(m) if *m == overview)),
                "Back to {}: {:?}",
                wallet,
                emitted
            );
        }
    }
}
