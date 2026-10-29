//! Route-derived Settings navigation, kept inside page content.

use coincube_ui::component::tenshu::{self, choice_chips, segment, tabs};
use coincube_ui::icon;
use coincube_ui::widget::{Column, Container, Element, Row};
use iced::widget::Space;
use iced::Length;

use crate::app::cache::Cache;
use crate::app::features;
use crate::app::menu::{
    CubeSettingsOption, CubeSubMenu, Menu, SettingsOption, SparkSettingsOption, SparkSubMenu,
    VaultSubMenu,
};
use crate::app::view::message::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteScope {
    Cube,
    Vault,
    Spark,
}

fn route_scope(menu: &Menu) -> Option<RouteScope> {
    match menu {
        Menu::Cube(CubeSubMenu::Settings(_)) => Some(RouteScope::Cube),
        Menu::Vault(VaultSubMenu::Settings(_)) => Some(RouteScope::Vault),
        Menu::Spark(SparkSubMenu::Settings(_)) => Some(RouteScope::Spark),
        _ => None,
    }
}

#[derive(Clone)]
struct RouteChoice {
    id: &'static str,
    label: String,
    intent: Message,
    selected: bool,
    enabled: bool,
}

fn route_choices(menu: &Menu, cache: &Cache) -> Vec<RouteChoice> {
    let scope = match route_scope(menu) {
        Some(scope) => scope,
        None => return Vec::new(),
    };
    let mut choices = vec![RouteChoice {
        id: "tenshu.settings.route.scope.cube",
        label: if cache.cube_name.is_empty() {
            "Cube".to_owned()
        } else {
            format!("Cube · {}", cache.cube_name)
        },
        intent: Message::Menu(Menu::Cube(CubeSubMenu::Settings(
            CubeSettingsOption::General,
        ))),
        selected: scope == RouteScope::Cube,
        enabled: true,
    }];
    if cache.has_vault || cache.has_imported_bitcoin {
        choices.push(RouteChoice {
            id: "tenshu.settings.route.scope.vault",
            label: "Vault".to_owned(),
            intent: Message::Menu(Menu::Vault(VaultSubMenu::Settings(Some(
                SettingsOption::Wallet,
            )))),
            selected: scope == RouteScope::Vault,
            enabled: true,
        });
    }
    if features::spark(cache.chain()).is_available() {
        choices.push(RouteChoice {
            id: "tenshu.settings.route.scope.spark",
            label: "Spark".to_owned(),
            intent: Message::Menu(Menu::Spark(SparkSubMenu::Settings(Some(
                SparkSettingsOption::General,
            )))),
            selected: scope == RouteScope::Spark,
            enabled: true,
        });
    }
    choices
}

fn section_routes(menu: &Menu, cache: &Cache) -> Vec<RouteChoice> {
    match route_scope(menu) {
        Some(RouteScope::Cube) => {
            let mut sections = vec![
                (CubeSettingsOption::General, "Display & appearance", "display"),
                (CubeSettingsOption::Recovery, "Backup & protection", "backup"),
                (CubeSettingsOption::PaymentChecks, "Payment checks", "payment-checks"),
                (CubeSettingsOption::AiAccess, "AI access", "ai-access"),
                (CubeSettingsOption::DataExports, "Data & exports", "data"),
            ];
            if crate::feature_flags::CUBE_MEMBERS_UI_ENABLED {
                sections.push((CubeSettingsOption::Members, "Members", "members"));
            }
            // Download stats and About moved to App preferences. Their routes
            // stay reachable, so show a tab for one only while it is open.
            match menu {
                Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::Stats)) => {
                    sections.push((CubeSettingsOption::Stats, "Download stats", "stats"))
                }
                Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::About)) => {
                    sections.push((CubeSettingsOption::About, "About", "about"))
                }
                _ => {}
            }
            sections
                .into_iter()
                .map(|(section, label, id)| RouteChoice {
                    id: match id {
                        "display" => "tenshu.settings.route.cube.display",
                        "backup" => "tenshu.settings.route.cube.backup",
                        "payment-checks" => "tenshu.settings.route.cube.payment-checks",
                        "ai-access" => "tenshu.settings.route.cube.ai-access",
                        "data" => "tenshu.settings.route.cube.data",
                        "stats" => "tenshu.settings.route.cube.stats",
                        "about" => "tenshu.settings.route.cube.about",
                        _ => "tenshu.settings.route.cube.members",
                    },
                    label: label.to_owned(),
                    intent: Message::Menu(Menu::Cube(CubeSubMenu::Settings(section))),
                    // The avatar flow belongs to Cube identity on Display.
                    selected: matches!(menu, Menu::Cube(CubeSubMenu::Settings(current))
                        if *current == section
                            || (*current == CubeSettingsOption::Avatar
                                && section == CubeSettingsOption::General)),
                    enabled: true,
                })
                .collect()
        }
        Some(RouteScope::Vault) => [
            (SettingsOption::Wallet, "Wallet & signers", "wallet"),
            (SettingsOption::Node, "Node", "node"),
            (
                SettingsOption::LocalSigning,
"Pair with Keychain",
                "pair",
            ),
            (SettingsOption::Connections, "Connections", "connections"),
            (SettingsOption::ImportExport, "Data & exports", "data"),
        ]
        .iter()
        .copied()
        .filter(|(section, _, _)| {
            !cache.has_imported_bitcoin || *section != SettingsOption::ImportExport
        })
        .map(|(section, label, id)| RouteChoice {
            id: match id {
                "wallet" => "tenshu.settings.route.vault.wallet",
                "node" => "tenshu.settings.route.vault.node",
                "pair" => "tenshu.settings.route.vault.pair",
                "connections" => "tenshu.settings.route.vault.connections",
                _ => "tenshu.settings.route.vault.data",
            },
            label: label.to_owned(),
            intent: Message::Menu(Menu::Vault(VaultSubMenu::Settings(Some(section)))),
            selected: matches!(menu, Menu::Vault(VaultSubMenu::Settings(Some(current))) if *current == section)
                || (section == SettingsOption::Wallet
                    && matches!(menu, Menu::Vault(VaultSubMenu::Settings(None)))),
            enabled: section != SettingsOption::LocalSigning || !cache.has_imported_bitcoin,
        })
        .collect(),
        Some(RouteScope::Spark) => [
            (SparkSettingsOption::General, "Spark", "general"),
            (SparkSettingsOption::Connections, "Connections", "connections"),
        ]
        .iter()
        .copied()
        .map(|(section, label, id)| RouteChoice {
            id: if id == "general" {
                "tenshu.settings.route.spark.general"
            } else {
                "tenshu.settings.route.spark.connections"
            },
            label: label.to_owned(),
            intent: Message::Menu(Menu::Spark(SparkSubMenu::Settings(Some(section)))),
            // Lightning Address lives on the Spark tab.
            selected: match menu {
                Menu::Spark(SparkSubMenu::Settings(Some(current))) => {
                    *current == section
                        || (section == SparkSettingsOption::General
                            && *current == SparkSettingsOption::LightningAddress)
                }
                Menu::Spark(SparkSubMenu::Settings(None)) => {
                    section == SparkSettingsOption::General
                }
                _ => false,
            },
            enabled: true,
        })
        .collect(),
        None => Vec::new(),
    }
}

fn scope_icon(scope: &str) -> coincube_ui::widget::Text<'static> {
    match scope {
        "tenshu.settings.route.scope.vault" => icon::vault_icon(),
        "tenshu.settings.route.scope.spark" => icon::spark_icon(),
        _ => icon::cube_icon(),
    }
}

/// The network coin and "<name> · <network>" under a page title (`.idl`).
pub(crate) fn scope_line(cache: &Cache, name: &str) -> Element<'static, Message> {
    let network = crate::app::view::nav::shell::network_label(cache.chain());
    let light = matches!(
        cache.theme_mode,
        coincube_ui::theme::palette::ThemeMode::Light
    );
    let coin = iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(
        crate::home::launcher_coin::widget_source(cache.chain(), 16.0, 1.0, light).into_bytes(),
    ))
    .width(Length::Fixed(16.0))
    .height(Length::Fixed(16.0));
    Row::new()
        .spacing(6)
        .align_y(iced::Alignment::Center)
        .push(
            Container::new(coin)
                .width(Length::Fixed(18.0))
                .height(Length::Fixed(18.0))
                .center_x(Length::Fixed(18.0))
                .center_y(Length::Fixed(18.0)),
        )
        .push(tenshu::caption(format!("{} · {}", name, network)).size(13))
        .into()
}

/// The Settings heading, its scope line, the scope chips and the section tabs
/// (`.ph`, `.idl`, `.scopes`, `.tabs`). The tabs keep one row at the 1024px
/// minimum window, so there is no narrow-window substitute.
pub fn in_content_navigation(menu: &Menu, cache: &Cache) -> Option<Element<'static, Message>> {
    let scope = route_scope(menu)?;
    let scopes = route_choices(menu, cache);
    let sections = section_routes(menu, cache);
    let scope_name = scopes
        .iter()
        .find(|choice| choice.selected)
        .map(|choice| choice.label.clone())
        .unwrap_or_else(|| match scope {
            RouteScope::Cube => "Cube".to_owned(),
            RouteScope::Vault => "Vault".to_owned(),
            RouteScope::Spark => "Spark".to_owned(),
        });
    let scope_line = scope_line(cache, &scope_name);

    let scope_intents: Vec<Message> = scopes.iter().map(|c| c.intent.clone()).collect();
    let chips = choice_chips(
        scopes.iter().enumerate().map(|(index, choice)| {
            segment(choice.id, index, choice.label.clone())
                .icon(scope_icon(choice.id))
                .enabled(choice.enabled)
        }),
        scopes.iter().position(|choice| choice.selected),
        Message::FocusAction,
    )
    .on_select(move |index| scope_intents[index].clone());

    let section_intents: Vec<Message> = sections.iter().map(|c| c.intent.clone()).collect();
    let tab_row = tabs(
        sections.iter().enumerate().map(|(index, choice)| {
            segment(choice.id, index, choice.label.clone()).enabled(choice.enabled)
        }),
        sections.iter().position(|choice| choice.selected),
        Message::FocusAction,
    )
    .on_select(move |index| section_intents[index].clone());

    Some(
        Column::new()
            .width(Length::Fill)
            .push(tenshu::title("Settings").size(24))
            .push(Space::new().height(Length::Fixed(4.0)))
            .push(scope_line)
            .push(Space::new().height(Length::Fixed(20.0)))
            .push(chips)
            .push(Space::new().height(Length::Fixed(16.0)))
            .push(tab_row)
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_bitcoin_keeps_vault_scope_and_disables_phone_pairing() {
        let cache = Cache {
            has_vault: false,
            has_imported_bitcoin: true,
            ..Cache::default()
        };
        let menu = Menu::Vault(VaultSubMenu::Settings(Some(SettingsOption::Wallet)));
        assert!(route_choices(&menu, &cache)
            .iter()
            .any(|choice| choice.label == "Vault" && choice.enabled));
        assert!(section_routes(&menu, &cache)
            .iter()
            .any(|choice| choice.label.contains("Pair with Keychain") && !choice.enabled));
        assert!(!section_routes(&menu, &cache)
            .iter()
            .any(|choice| choice.label == "Data & exports"));
    }

    #[test]
    fn vault_settings_default_selects_wallet_and_signers() {
        let menu = Menu::Vault(VaultSubMenu::Settings(None));
        let sections = section_routes(&menu, &Cache::default());
        assert!(sections
            .iter()
            .any(|choice| choice.label == "Wallet & signers" && choice.selected));
        assert!(!sections
            .iter()
            .any(|choice| choice.label == "Node" && choice.selected));
    }

    #[test]
    fn cube_sections_keep_recipient_checks_separate() {
        let menu = Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::PaymentChecks));
        let sections = section_routes(&menu, &Cache::default());
        assert!(sections
            .iter()
            .any(|choice| choice.label == "Payment checks" && choice.selected));
        assert!(sections
            .iter()
            .any(|choice| choice.label == "Display & appearance" && !choice.selected));
    }

    #[test]
    fn cube_settings_exposes_ai_access_in_the_rendered_route_choices() {
        let menu = Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::General));
        let sections = section_routes(&menu, &Cache::default());
        assert!(sections.iter().any(|choice| {
            choice.id == "tenshu.settings.route.cube.ai-access"
                && choice.label == "AI access"
                && choice.enabled
                && !choice.selected
                && matches!(
                    choice.intent,
                    Message::Menu(Menu::Cube(CubeSubMenu::Settings(
                        CubeSettingsOption::AiAccess
                    )))
                )
        }));

        let ai_menu = Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::AiAccess));
        assert!(section_routes(&ai_menu, &Cache::default())
            .iter()
            .any(|choice| choice.label == "AI access" && choice.selected));
    }

    #[test]
    fn cube_data_exports_is_selected_only_on_its_own_route() {
        let data = Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::DataExports));
        let sections = section_routes(&data, &Cache::default());
        assert!(sections.iter().any(|choice| {
            choice.label == "Data & exports"
                && choice.selected
                && choice.enabled
                && matches!(
                    choice.intent,
                    Message::Menu(Menu::Cube(CubeSubMenu::Settings(
                        CubeSettingsOption::DataExports
                    )))
                )
        }));
        let backup = Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::Recovery));
        assert!(section_routes(&backup, &Cache::default())
            .iter()
            .any(|choice| choice.label == "Data & exports" && !choice.selected));
    }

    #[test]
    fn cube_settings_keeps_about_and_stats_reachable() {
        let cache = Cache::default();
        for (current, label) in [
            (CubeSettingsOption::About, "About"),
            (CubeSettingsOption::Stats, "Download stats"),
        ] {
            let menu = Menu::Cube(CubeSubMenu::Settings(current));
            let scopes = route_choices(&menu, &cache);
            assert!(!scopes.iter().any(|choice| choice.label == "This device"));
            assert!(scopes
                .iter()
                .any(|choice| choice.label == "Cube" && choice.selected));

            let sections = section_routes(&menu, &cache);
            assert!(sections.iter().any(|choice| {
                choice.label == label
                    && choice.selected
                    && choice.enabled
                    && matches!(choice.intent, Message::Menu(Menu::Cube(CubeSubMenu::Settings(route))) if route == current)
            }));
            assert!(sections
                .iter()
                .any(|choice| choice.label == "Data & exports"));
        }
    }

    #[test]
    fn scope_uses_the_open_cubes_name_without_adding_device_scope() {
        let cache = Cache {
            cube_name: "Everyday".to_owned(),
            ..Cache::default()
        };
        let choices = route_choices(
            &Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::General)),
            &cache,
        );
        assert!(choices
            .iter()
            .any(|choice| choice.label == "Cube · Everyday" && choice.selected));
        assert!(!choices.iter().any(|choice| choice.label == "This device"));
    }
}
