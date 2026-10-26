//! Route-derived Settings navigation, kept inside page content.

use coincube_ui::component::text::*;
use coincube_ui::font;
use coincube_ui::theme;
use coincube_ui::widget::{action::Action, Element};
use iced::widget::{pick_list, responsive, rule, Column, Row};
use iced::{Length, Size};

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
                (CubeSettingsOption::Stats, "Download stats", "stats"),
                (CubeSettingsOption::About, "About", "about"),
            ];
            if crate::feature_flags::CUBE_MEMBERS_UI_ENABLED {
                sections.push((CubeSettingsOption::Members, "Members", "members"));
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
                    selected: matches!(menu, Menu::Cube(CubeSubMenu::Settings(current)) if *current == section),
                    enabled: true,
                })
                .collect()
        }
        Some(RouteScope::Vault) => [
            (SettingsOption::Wallet, "Wallet & signers", "wallet"),
            (SettingsOption::Node, "Node", "node"),
            (
                SettingsOption::LocalSigning,
                if cache.has_imported_bitcoin {
                    "Pair with Keychain · Unavailable for imported wallets"
                } else {
                    "Pair with Keychain"
                },
                "pair",
            ),
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
            (
                SparkSettingsOption::LightningAddress,
                "Lightning Address",
                "lightning-address",
            ),
        ]
        .iter()
        .copied()
        .map(|(section, label, id)| RouteChoice {
            id: if id == "general" {
                "tenshu.settings.route.spark.general"
            } else {
                "tenshu.settings.route.spark.lightning-address"
            },
            label: label.to_owned(),
            intent: Message::Menu(Menu::Spark(SparkSubMenu::Settings(Some(section)))),
            selected: matches!(menu, Menu::Spark(SparkSubMenu::Settings(Some(current))) if *current == section)
                || (section == SparkSettingsOption::General
                    && matches!(menu, Menu::Spark(SparkSubMenu::Settings(None)))),
            enabled: true,
        })
        .collect(),
        None => Vec::new(),
    }
}

/// The Settings scope and section choices live in page content on every route.
pub fn in_content_navigation(menu: &Menu, cache: &Cache) -> Option<Element<'static, Message>> {
    let scope = route_scope(menu)?;
    let scopes = route_choices(menu, cache);
    let sections = section_routes(menu, cache);
    let context = match scope {
        RouteScope::Cube => "Applies to this Cube.",
        RouteScope::Vault => "Applies to this Vault in the current Cube.",
        RouteScope::Spark => "Applies to this Cube's Spark wallet.",
    };
    Some(
        responsive(move |size: Size| {
            let mut content = Column::new()
                .spacing(12)
                .width(Length::Fill)
                .push(text("Settings").size(H2_SIZE).font(font::SEMIBOLD))
                .push(text(context).size(P2_SIZE));
            if size.width < 760.0 {
                content = content
                    .push(compact_choice("Scope", scopes.clone()))
                    .push(compact_choice("Section", sections.clone()));
            } else {
                content = content
                    .push(route_choice_row(scopes.clone()))
                    .push(route_choice_row(sections.clone()));
            }
            content.push(rule::horizontal(1)).into()
        })
        .height(Length::Shrink)
        .into(),
    )
}

fn compact_choice(label: &'static str, choices: Vec<RouteChoice>) -> Element<'static, Message> {
    let selected = choices
        .iter()
        .find(|choice| choice.selected)
        .map(|choice| choice.label.clone());
    let labels: Vec<String> = choices
        .iter()
        .filter(|choice| choice.enabled)
        .map(|choice| choice.label.clone())
        .collect();
    let fallback = choices
        .iter()
        .find(|choice| choice.selected)
        .or_else(|| choices.first())
        .map(|choice| choice.intent.clone());
    Row::new()
        .spacing(12)
        .align_y(iced::Alignment::Center)
        .push(text(label).size(P2_SIZE).width(Length::Fixed(64.0)))
        .push(
            pick_list(labels, selected, move |selected| {
                choices
                    .iter()
                    .find(|choice| choice.label == selected && choice.enabled)
                    .map(|choice| choice.intent.clone())
                    .or_else(|| fallback.clone())
                    .expect("settings choice list is nonempty")
            })
            .width(Length::Fill)
            .style(theme::pick_list::primary),
        )
        .into()
}

fn route_choice_row(choices: Vec<RouteChoice>) -> Element<'static, Message> {
    let mut row = Row::new().spacing(8);
    for choice in choices {
        let action = Action::new(
            choice.id,
            text(choice.label).size(P2_SIZE).font(font::MEDIUM),
            Message::FocusAction,
        )
        .padding([8, 16])
        .style(if choice.selected {
            theme::button::primary
        } else {
            theme::button::secondary
        });
        row = row.push(if choice.enabled {
            action.on_press(choice.intent)
        } else {
            action
        });
    }
    row.wrap().vertical_spacing(8).into()
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
