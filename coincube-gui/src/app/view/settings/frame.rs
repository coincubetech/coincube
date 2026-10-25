//! Route-derived Settings navigation, kept inside page content.

use coincube_ui::component::text::*;
use coincube_ui::font;
use coincube_ui::theme;
use coincube_ui::widget::{action::Action, Element};
use iced::widget::{rule, Column, Row};
use iced::Length;

use crate::app::cache::Cache;
use crate::app::features;
use crate::app::menu::{
    CubeSettingsOption, CubeSubMenu, LiquidSubMenu, Menu, SettingsOption, SparkSettingsOption,
    SparkSubMenu, VaultSubMenu,
};
use crate::app::view::message::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteScope {
    Cube,
    Device,
    Vault,
    Spark,
    Liquid,
}

fn route_scope(menu: &Menu) -> Option<RouteScope> {
    match menu {
        Menu::Cube(CubeSubMenu::Settings(
            CubeSettingsOption::Stats | CubeSettingsOption::About,
        )) => Some(RouteScope::Device),
        Menu::Cube(CubeSubMenu::Settings(_)) => Some(RouteScope::Cube),
        Menu::Vault(VaultSubMenu::Settings(_)) => Some(RouteScope::Vault),
        Menu::Spark(SparkSubMenu::Settings(_)) => Some(RouteScope::Spark),
        Menu::Liquid(LiquidSubMenu::Settings(_)) => Some(RouteScope::Liquid),
        _ => None,
    }
}

struct RouteChoice {
    id: &'static str,
    label: &'static str,
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
        label: "Cube",
        intent: Message::Menu(Menu::Cube(CubeSubMenu::Settings(
            CubeSettingsOption::General,
        ))),
        selected: scope == RouteScope::Cube,
        enabled: true,
    }];
    choices.push(RouteChoice {
        id: "tenshu.settings.route.scope.device",
        label: "This device",
        intent: Message::Menu(Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::About))),
        selected: scope == RouteScope::Device,
        enabled: true,
    });
    if cache.has_vault || cache.has_imported_bitcoin {
        choices.push(RouteChoice {
            id: "tenshu.settings.route.scope.vault",
            label: "Vault",
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
            label: "Spark",
            intent: Message::Menu(Menu::Spark(SparkSubMenu::Settings(Some(
                SparkSettingsOption::General,
            )))),
            selected: scope == RouteScope::Spark,
            enabled: true,
        });
    }
    if features::liquid_wallet_usable(cache.chain(), cache.liquid_gate) {
        choices.push(RouteChoice {
            id: "tenshu.settings.route.scope.liquid",
            label: "Liquid",
            intent: Message::Menu(Menu::Liquid(LiquidSubMenu::Settings(None))),
            selected: scope == RouteScope::Liquid,
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
                        _ => "tenshu.settings.route.cube.members",
                    },
                    label,
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
            label,
            intent: Message::Menu(Menu::Vault(VaultSubMenu::Settings(Some(section)))),
            selected: matches!(menu, Menu::Vault(VaultSubMenu::Settings(Some(current))) if *current == section)
                || (section == SettingsOption::Node
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
            label,
            intent: Message::Menu(Menu::Spark(SparkSubMenu::Settings(Some(section)))),
            selected: matches!(menu, Menu::Spark(SparkSubMenu::Settings(Some(current))) if *current == section)
                || (section == SparkSettingsOption::General
                    && matches!(menu, Menu::Spark(SparkSubMenu::Settings(None)))),
            enabled: true,
        })
        .collect(),
        Some(RouteScope::Liquid) => vec![RouteChoice {
            id: "tenshu.settings.route.liquid.backup",
            label: "Open Backup & protection",
            intent: Message::Menu(Menu::Cube(CubeSubMenu::Settings(
                CubeSettingsOption::Recovery,
            ))),
            selected: false,
            enabled: true,
        }],
        // This base still renders device information through the existing
        // About and Download Stats routes.
        Some(RouteScope::Device) => vec![
            RouteChoice {
                id: "tenshu.settings.route.device.about",
                label: "About",
                intent: Message::Menu(Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::About))),
                selected: matches!(menu, Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::About))),
                enabled: true,
            },
            RouteChoice {
                id: "tenshu.settings.route.device.stats",
                label: "Download stats",
                intent: Message::Menu(Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::Stats))),
                selected: matches!(menu, Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::Stats))),
                enabled: true,
            },
        ],
        None => Vec::new(),
    }
}

/// The Settings scope and section choices live in page content on every route.
pub fn in_content_navigation(menu: &Menu, cache: &Cache) -> Option<Element<'static, Message>> {
    route_scope(menu)?;
    let scope_row = route_choice_row(route_choices(menu, cache));
    let mut content = Column::new()
        .spacing(12)
        .width(Length::Fill)
        .push(text("Settings").size(H2_SIZE).font(font::SEMIBOLD))
        .push(scope_row);
    content = content.push(route_choice_row(section_routes(menu, cache)));
    Some(content.push(rule::horizontal(1)).into())
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
        .focus_radius(17.0)
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
    fn vault_settings_default_selects_node() {
        let menu = Menu::Vault(VaultSubMenu::Settings(None));
        let sections = section_routes(&menu, &Cache::default());
        assert!(sections
            .iter()
            .any(|choice| choice.label == "Node" && choice.selected));
        assert!(!sections
            .iter()
            .any(|choice| choice.label == "Wallet & signers" && choice.selected));
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
}
