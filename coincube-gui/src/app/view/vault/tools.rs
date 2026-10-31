//! The Vault's Tools page (P-Advanced): a pinned head with the Tools title,
//! its scope line and Back to Vault, then one tab per tool, kept inside page
//! content above whichever tool is open (the tabs move onto the head's rule
//! in plan W6). Each tab is its own route, so the tool
//! panels keep their state and deep links (Refresh coins, a PSBT) still land
//! on the right tab.

use coincube_ui::component::tenshu::{segment, tabs, PageHead};
use coincube_ui::icon;
use coincube_ui::widget::{Column, Element, Text};
use iced::widget::Space;
use iced::Length;

use crate::app::cache::Cache;
use crate::app::menu::{Menu, VaultSubMenu};
use crate::app::view::{message::Message, nav};

/// Whether `menu` is one of the Vault's tools.
fn is_tool(menu: &Menu) -> bool {
    matches!(
        menu,
        Menu::Vault(
            VaultSubMenu::Coins(_)
                | VaultSubMenu::PSBTs(_)
                | VaultSubMenu::Recovery
                | VaultSubMenu::SplitCoins
        )
    )
}

/// Whether two tool routes open the same tab, whatever coins or PSBT they
/// carry.
fn same_tab(a: &Menu, b: &Menu) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
        && match (a, b) {
            (Menu::Vault(a), Menu::Vault(b)) => {
                std::mem::discriminant(a) == std::mem::discriminant(b)
            }
            _ => false,
        }
}

fn tab_id(route: &Menu) -> &'static str {
    match route {
        Menu::Vault(VaultSubMenu::Coins(_)) => "tenshu.vault.tools.tab.coins",
        Menu::Vault(VaultSubMenu::PSBTs(_)) => "tenshu.vault.tools.tab.psbts",
        Menu::Vault(VaultSubMenu::Recovery) => "tenshu.vault.tools.tab.recovery",
        _ => "tenshu.vault.tools.tab.split",
    }
}

fn tab_icon(route: &Menu) -> Text<'static> {
    match route {
        Menu::Vault(VaultSubMenu::Coins(_)) => icon::coins_outline_icon(),
        Menu::Vault(VaultSubMenu::PSBTs(_)) => icon::receipt_icon(),
        Menu::Vault(VaultSubMenu::Recovery) => icon::recovery_icon(),
        _ => icon::claim_icon(),
    }
}

/// The Tools head for a Vault tool route, `None` elsewhere.
pub fn page_head(menu: &Menu, cache: &Cache) -> Option<PageHead<'static, Message>> {
    is_tool(menu).then(|| {
        coincube_ui::component::tenshu::page_head("Tools", Message::FocusAction)
            .context(
                crate::app::view::cube_coin(cache),
                crate::app::view::network_line(cache, crate::app::view::cube_display_name(cache)),
            )
            .back("Vault", Message::Menu(Menu::Vault(VaultSubMenu::Overview)))
    })
}

/// The tab bar for a Vault tool route, `None` elsewhere.
pub fn in_content_navigation(menu: &Menu, cache: &Cache) -> Option<Element<'static, Message>> {
    if !is_tool(menu) {
        return None;
    }
    let tools = nav::vault::tools(&nav::NavContext::from_cache(cache));

    let intents: Vec<Message> = tools
        .iter()
        .map(|(_, route)| Message::Menu(route.clone()))
        .collect();
    let tab_row = tabs(
        tools.iter().enumerate().map(|(index, (label, route))| {
            segment(tab_id(route), index, *label).icon(tab_icon(route))
        }),
        tools.iter().position(|(_, route)| same_tab(route, menu)),
        Message::FocusAction,
    )
    .on_select(move |index| intents[index].clone());

    Some(
        Column::new()
            .width(Length::Fill)
            .push(tab_row)
            .push(Space::new().height(Length::Fixed(20.0)))
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_frame_shows_only_on_tool_routes() {
        let cache = Cache {
            has_vault: true,
            ..Cache::default()
        };
        for menu in [
            Menu::Vault(VaultSubMenu::Coins(None)),
            Menu::Vault(VaultSubMenu::PSBTs(None)),
            Menu::Vault(VaultSubMenu::Recovery),
            Menu::Vault(VaultSubMenu::SplitCoins),
        ] {
            assert!(in_content_navigation(&menu, &cache).is_some(), "{:?}", menu);
            assert!(page_head(&menu, &cache).is_some(), "{:?}", menu);
        }
        for menu in [
            Menu::Vault(VaultSubMenu::Overview),
            Menu::Vault(VaultSubMenu::Send),
            Menu::Vault(VaultSubMenu::Settings(None)),
        ] {
            assert!(in_content_navigation(&menu, &cache).is_none(), "{:?}", menu);
            assert!(page_head(&menu, &cache).is_none(), "{:?}", menu);
        }
    }

    #[test]
    fn tools_frame_mounts_title_back_link_and_one_tab_per_tool() {
        use crate::app::view::shared::landing::capture;
        let cache = Cache {
            has_vault: true,
            cube_name: "Everyday".to_owned(),
            ..Cache::default()
        };
        let menu = Menu::Vault(VaultSubMenu::PSBTs(None));
        let head = page_head(&menu, &cache).expect("PSBT proposals is a tool route");
        let frame = in_content_navigation(&menu, &cache).expect("PSBT proposals is a tool route");
        let mounted = capture::inspect(
            "vault-tools-frame",
            Column::new()
                .push(head.view(crate::app::view::page_gutter))
                .push(frame)
                .into(),
            (1024, 768),
            "dark",
            coincube_ui::theme::Theme::dark(),
        );
        assert!(mounted.contains("Tools"));
        assert!(mounted.contains("Everyday"));
        assert!(mounted.contains("Back to Vault"));
        assert!(mounted.has_focus(coincube_ui::component::tenshu::page_head::BACK_ID.into()));
        for (label, route) in nav::vault::tools(&nav::NavContext::from_cache(&cache)) {
            assert!(mounted.contains(label), "missing tab {:?}", label);
            assert!(mounted.has_focus(tab_id(&route).into()));
        }
    }

    #[test]
    fn a_tool_route_with_a_selection_still_selects_its_tab() {
        let tools = nav::vault::tools(&nav::NavContext::from_cache(&Cache {
            has_vault: true,
            ..Cache::default()
        }));
        let menu = Menu::Vault(VaultSubMenu::Coins(Some(Vec::new())));
        assert_eq!(
            tools.iter().position(|(_, route)| same_tab(route, &menu)),
            Some(0)
        );
        assert!(!same_tab(
            &Menu::Vault(VaultSubMenu::Recovery),
            &Menu::Vault(VaultSubMenu::SplitCoins)
        ));
    }
}
