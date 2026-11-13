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
        with_back(
            menu,
            cache,
            coincube_ui::component::tenshu::page_head("Tools", Message::FocusAction).context(
                crate::app::view::cube_coin(cache),
                crate::app::view::network_line(cache, &crate::app::view::cube_display_name(cache)),
            ),
        )
    })
}

/// `head` on a Vault tool route: the Tools head's Back to Vault, so a tool's
/// own head (Recovery's compose and review) keeps the way back, and the Tools
/// tabs on the head's rule (P-Advanced, plan W6) unless the tool's own steps
/// already sit there, in which case [`in_content_navigation`] keeps the tabs
/// in the body.
pub fn with_back<'a>(
    menu: &Menu,
    cache: &Cache,
    head: PageHead<'a, Message>,
) -> PageHead<'a, Message> {
    if !is_tool(menu) {
        return head;
    }
    let head = head.back("Vault", Message::Menu(Menu::Vault(VaultSubMenu::Overview)));
    if head.has_rule() {
        head
    } else {
        head.rule(tab_row(menu, cache))
    }
}

/// The Tools tabs in the body, for a tool route whose own head's rule holds
/// its steps (`own_steps`), so the tabs couldn't go there. `None` elsewhere.
pub fn in_content_navigation(
    menu: &Menu,
    cache: &Cache,
    own_steps: bool,
) -> Option<Element<'static, Message>> {
    if !is_tool(menu) || !own_steps {
        return None;
    }
    Some(
        Column::new()
            .width(Length::Fill)
            .push(tab_row(menu, cache))
            .push(Space::new().height(Length::Fixed(20.0)))
            .into(),
    )
}

/// The Tools tabs, the current tool selected.
fn tab_row(menu: &Menu, cache: &Cache) -> Element<'static, Message> {
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
    tab_row.into()
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
            let head = page_head(&menu, &cache).expect("a tool route has the Tools head");
            assert!(
                head.has_rule(),
                "the tabs sit on the head's rule: {:?}",
                menu
            );
            assert!(
                in_content_navigation(&menu, &cache, false).is_none(),
                "no second tab row: {:?}",
                menu
            );
            // A tool whose own head holds its steps keeps the tabs in the body.
            assert!(
                in_content_navigation(&menu, &cache, true).is_some(),
                "{:?}",
                menu
            );
        }
        for menu in [
            Menu::Vault(VaultSubMenu::Overview),
            Menu::Vault(VaultSubMenu::Send),
            Menu::Vault(VaultSubMenu::Settings(None)),
        ] {
            assert!(
                in_content_navigation(&menu, &cache, true).is_none(),
                "{:?}",
                menu
            );
            assert!(page_head(&menu, &cache).is_none(), "{:?}", menu);
        }
    }

    #[test]
    fn the_tools_head_mounts_title_back_and_one_tab_per_tool_on_its_rule() {
        use crate::app::view::shared::landing::capture;
        let cache = Cache {
            has_vault: true,
            cube_name: "Everyday".to_owned(),
            ..Cache::default()
        };
        let menu = Menu::Vault(VaultSubMenu::PSBTs(None));
        let head = page_head(&menu, &cache).expect("PSBT proposals is a tool route");
        let mounted = capture::inspect(
            "vault-tools-frame",
            head.view(crate::app::view::page_gutter),
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

    /// P-Advanced: the Tools tabs sit on the head's rule, once; a tool whose
    /// own head holds its steps (Recovery) keeps them in the body instead.
    #[test]
    fn tools_tabs_show_once_on_the_page() {
        use crate::app::view::shared::landing::capture;
        let cache = Cache {
            has_vault: true,
            cube_name: "Everyday".to_owned(),
            ..Cache::default()
        };
        let tools = nav::vault::tools(&nav::NavContext::from_cache(&cache));
        let (label, _) = tools[0];
        let count = |probe: &capture::Inspection| {
            probe.text.iter().filter(|(text, _)| text == label).count()
        };
        let menu = Menu::Vault(VaultSubMenu::PSBTs(None));
        let routed = capture::inspect_full_window(
            "vault-tools-routed",
            crate::app::view::dashboard(&menu, &cache, Column::new()),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
        );
        assert_eq!(count(&routed), 1, "one tab row, in the head");
        let recovery = Menu::Vault(VaultSubMenu::Recovery);
        let stepped = capture::inspect_full_window(
            "vault-tools-own-steps",
            crate::app::view::dashboard_headed(
                &recovery,
                &cache,
                crate::app::view::vault_page_head(&cache, "Recovery").rule(
                    coincube_ui::component::tenshu::steps(&["Details", "Review"], 0),
                ),
                Column::new(),
            ),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
        );
        assert_eq!(count(&stepped), 1, "the tabs move to the body");
        assert!(stepped.contains("Details") && stepped.contains("Back to Vault"));
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
