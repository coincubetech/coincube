//! The single text sidebar shared by every open-Cube panel.
//!
//! Keep routes in the existing `Menu` model while screen families migrate.
//! Settings and Tools each have one home here; old destination handlers stay
//! reachable without rendering a second or third rail.

use super::{marketplace, NavContext, SubItem};
use crate::{
    app::{
        features,
        menu::{
            CubeSettingsOption, CubeSubMenu, LiquidSubMenu, Menu, SettingsOption,
            SparkSettingsOption, SparkSubMenu, TopLevel, VaultSubMenu,
        },
        view::Message,
        ConnectionStatus,
    },
    chain::{ChainId, ChainIdExt},
};
use coincube_ui::{
    icon::{
        arrow_down, chevron_right, coins_outline_icon, cube_icon, home_icon,
        lightning_outline_icon, receipt_icon, recovery_icon, settings_icon, shop_icon, vault_icon,
    },
    theme,
    widget::{action::Action, Column, ColumnExt, Element, Row, Text},
};
use iced::{
    widget::{column, container, row, rule, scrollable, text, Space},
    Alignment, Length,
};

pub const FULL_WIDTH: f32 = 248.0;
pub const COMPACT_WIDTH: f32 = 208.0;
pub const TOP_BAR_HEIGHT: f32 = 56.0;
pub const LAUNCHER_RETURN_ID: &str = "tenshu.launcher.return";
pub const CUBE_SWITCHER_ID: &str = "tenshu.cube.switcher";
pub const CUBE_LOCK_ID: &str = "tenshu.cube.lock.ask";
pub const CUBE_LOCK_CANCEL_ID: &str = "tenshu.cube.lock.cancel";
pub const VAULT_TOOLS_ID: &str = "tenshu.vault.tools";
pub const SETTINGS_ID: &str = "tenshu.shell.settings";
pub const MIDDLE_SCROLL_ID: &str = "tenshu.shell.middle-scroll";

/// Innermost shell dismissal. The GUI owns Escape dispatch; this helper only
/// identifies the existing harmless intent for the currently open disclosure.
pub fn escape_intent(state: ShellState) -> Option<Message> {
    if state.lock_confirmation {
        Some(Message::CancelCubeLock)
    } else if state.switcher_open {
        Some(Message::ToggleCubeSwitcher)
    } else if state.tools_open {
        Some(Message::ToggleVaultTools)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ShellState {
    pub switcher_open: bool,
    pub lock_confirmation: bool,
    pub tools_open: bool,
    pub backup_attention: bool,
}

fn network_label(network: ChainId) -> &'static str {
    match network {
        ChainId::Bitcoin => "Bitcoin mainnet",
        ChainId::BitcoinBlake2b => "Bitcoin Blake2b mainnet",
        other => other.label(),
    }
}

fn short_cube_name(name: &str) -> String {
    // Names are persisted without a length limit and may contain line breaks.
    // Keep fixed sidebar regions to one short, predictable label.
    let mut chars = name.chars().map(|ch| {
        if ch.is_whitespace() || ch.is_control() {
            ' '
        } else {
            ch
        }
    });
    let prefix: String = chars.by_ref().take(16).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn muted<'a>(label: impl Into<String>, size: u32) -> Text<'a> {
    text(label.into())
        .size(size)
        .style(|t: &theme::Theme| iced::widget::text::Style {
            color: Some(t.colors.shell.muted),
        })
}

fn status<'a>(label: impl Into<String>, good: bool) -> Text<'a> {
    text(label.into())
        .size(13)
        .style(move |t: &theme::Theme| iced::widget::text::Style {
            color: Some(if good {
                t.colors.shell.good
            } else {
                t.colors.shell.muted
            }),
        })
}

/// The 56px bar contains only the Tenshu return action and status chips.
pub fn top_bar<'a>(ctx: &NavContext<'a>) -> Element<'a, Message> {
    let brand = Action::new(
        LAUNCHER_RETURN_ID,
        row![cube_icon().size(17), text("Tenshu").size(15)]
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .padding([7, 8])
    .style(theme::button::shell_brand)
    .on_press(Message::OpenLauncher);

    let account: Element<'a, Message> = if ctx.connect_authenticated {
        container(status("Signed in", true))
            .padding([7, 11])
            .style(theme::container::shell_chip)
            .into()
    } else {
        container(
            Action::new("tenshu.connect.sign-in", status("Signed out", false))
                .padding([6, 11])
                .style(theme::button::shell_nav)
                .on_press(Message::OpenConnectSignIn),
        )
        .style(theme::container::shell_chip)
        .into()
    };
    let account_name = if ctx.connect_authenticated {
        ctx.connect_email
            .map(|email| format!("Account: Signed in as {email}"))
            .unwrap_or_else(|| "Account: Signed in".to_owned())
    } else {
        "Account: Signed out · Sign in".to_owned()
    };
    let account = iced::widget::tooltip(
        account,
        container(muted(account_name, 12))
            .padding([6, 10])
            .style(theme::container::shell_menu),
        iced::widget::tooltip::Position::Bottom,
    );

    let (connection, healthy) = match ctx.connect_stream_status {
        ConnectionStatus::Connected => ("Connected", true),
        ConnectionStatus::Connecting => ("Checking…", false),
        ConnectionStatus::Error(_) => ("Reconnect required", false),
        ConnectionStatus::Inactive => ("Not checked", false),
    };
    let connection_chip = container(status(connection, healthy))
        .padding([7, 11])
        .style(theme::container::shell_chip);
    let connection = iced::widget::tooltip(
        connection_chip,
        container(muted(format!("Cube connection: {connection}"), 12))
            .padding([6, 10])
            .style(theme::container::shell_menu),
        iced::widget::tooltip::Position::Bottom,
    );

    container(
        row![brand, Space::new().width(Length::Fill), account, connection]
            .spacing(8)
            .align_y(Alignment::Center)
            .padding([0, 16]),
    )
    .height(Length::Fixed(TOP_BAR_HEIGHT))
    .width(Length::Fill)
    .style(theme::container::shell_top)
    .into()
}

fn nav_row<'a>(
    label: impl Into<String>,
    icon: Option<Text<'static>>,
    active: bool,
    message: Option<Message>,
    nested: bool,
    disabled_reason: Option<String>,
) -> Element<'a, Message> {
    let label = label.into();
    let mut body: Row<Message> = Row::new().spacing(10).align_y(Alignment::Center);
    if nested {
        body = body.push(Space::new().width(Length::Fixed(18.0)));
    }
    if let Some(icon) = icon {
        body = body.push(
            icon.size(17)
                .style(move |t: &theme::Theme| iced::widget::text::Style {
                    color: Some(if active {
                        t.colors.shell.accent_ink
                    } else {
                        t.colors.shell.muted
                    }),
                }),
        );
    }
    body = body.push(text(label.clone()).size(if nested { 13 } else { 14 }));
    let id = if nested {
        format!("tenshu.shell.sub.{label}.{message:?}")
    } else {
        match label.as_str() {
            "Lock Cube" => CUBE_LOCK_ID.to_owned(),
            "Tools" => VAULT_TOOLS_ID.to_owned(),
            "Settings" => SETTINGS_ID.to_owned(),
            "All Cubes" => "tenshu.launcher.all-cubes".to_owned(),
            _ => format!("tenshu.shell.nav.{label}"),
        }
    };
    let mut button = Action::new(id, body)
        .width(Length::Fill)
        .padding([9, 10])
        .style(if active {
            theme::button::shell_nav_active
        } else {
            theme::button::shell_nav
        });
    if let Some(message) = message {
        button = button.on_press(message);
    }
    let row: Element<'a, Message> = button.into();
    if let Some(reason) = disabled_reason {
        iced::widget::tooltip(
            row,
            container(muted(reason, 12))
                .padding([6, 10])
                .style(theme::container::shell_menu),
            iced::widget::tooltip::Position::Right,
        )
        .gap(6)
        .into()
    } else {
        row
    }
}

fn route<'a>(
    label: impl Into<String>,
    icon: Option<Text<'static>>,
    menu: &Menu,
    to: Menu,
    nested: bool,
) -> Element<'a, Message> {
    let active = menu == &to;
    nav_row(label, icon, active, Some(Message::Menu(to)), nested, None)
}

fn subitem<'a>(menu: &Menu, item: SubItem, label: Option<&str>) -> Element<'a, Message> {
    let active = item.disabled_reason.is_none() && (item.matches)(menu);
    let disabled_reason = item.disabled_reason;
    nav_row(
        label.unwrap_or(item.label),
        None,
        active,
        disabled_reason
            .is_none()
            .then_some(Message::Menu(item.route)),
        true,
        disabled_reason,
    )
}

fn section(label: &'static str) -> Element<'static, Message> {
    container(muted(label, 12))
        .padding(iced::Padding {
            top: 17.0,
            right: 10.0,
            bottom: 5.0,
            left: 10.0,
        })
        .into()
}

fn identity<'a>(ctx: &NavContext<'a>, state: ShellState) -> Element<'a, Message> {
    let avatar: Element<'a, Message> = if let Some(handle) = ctx.avatar {
        iced::widget::image(handle.clone())
            .width(Length::Fixed(36.0))
            .height(Length::Fixed(36.0))
            .into()
    } else {
        container(cube_icon().size(20))
            .width(Length::Fixed(36.0))
            .height(Length::Fixed(36.0))
            .center_x(Length::Fixed(36.0))
            .center_y(Length::Fixed(36.0))
            .style(theme::container::shell_menu)
            .into()
    };
    let title = if ctx.cube_name.is_empty() {
        "Cube".to_owned()
    } else {
        short_cube_name(ctx.cube_name)
    };
    let control = Action::new(
        CUBE_SWITCHER_ID,
        row![
            avatar,
            column![
                text(title)
                    .size(14)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                muted(network_label(ctx.network), 12)
            ]
            .spacing(1)
            .width(Length::Fill),
            if state.switcher_open {
                arrow_down()
            } else {
                chevron_right()
            },
        ]
        .spacing(9)
        .align_y(Alignment::Center),
    )
    .padding([8, 10])
    .width(Length::Fill)
    .style(theme::button::shell_nav)
    .on_press(Message::ToggleCubeSwitcher);
    iced::widget::tooltip(
        container(control).padding([4, 8]),
        container(muted(ctx.cube_name, 12))
            .padding([6, 10])
            .style(theme::container::shell_menu),
        iced::widget::tooltip::Position::Right,
    )
    .into()
}

fn switcher_menu<'a>(ctx: &NavContext<'a>, state: ShellState) -> Element<'a, Message> {
    let mut items: Column<Message> = column![nav_row(
        "All Cubes",
        Some(cube_icon()),
        false,
        Some(Message::OpenLauncher),
        false,
        None,
    )]
    .spacing(4);
    if state.lock_confirmation {
        let prompt = format!(
            "Lock {} and open the Launcher?",
            short_cube_name(ctx.cube_name)
        );
        let full_name = container(
            scrollable(
                muted(ctx.cube_name, 12).wrapping(iced::widget::text::Wrapping::WordOrGlyph),
            )
            .width(Length::Fixed(200.0))
            .height(Length::Fixed(120.0)),
        )
        .padding([6, 10])
        .style(theme::container::shell_menu);
        items = items
            .push(iced::widget::tooltip(
                muted(prompt, 13).wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                full_name,
                iced::widget::tooltip::Position::Right,
            ))
            .push(
                row![
                    container(
                        Action::new("tenshu.cube.lock.confirm", text("Lock").size(13))
                            .padding([6, 12])
                            .style(theme::button::primary)
                            .focus_radius(25.0)
                            .on_press(Message::LockCubeToLauncher)
                    )
                    .id("cube-lock-confirm"),
                    container(
                        Action::new(CUBE_LOCK_CANCEL_ID, text("Cancel").size(13))
                            .padding([6, 12])
                            .style(theme::button::shell_nav)
                            .on_press(Message::CancelCubeLock)
                    )
                    .id("cube-lock-cancel"),
                ]
                .spacing(8),
            );
    } else {
        items = items.push(nav_row(
            "Lock Cube",
            None,
            false,
            Some(Message::AskCubeLock),
            false,
            None,
        ));
    }
    container(items)
        .padding(7)
        .style(theme::container::shell_menu)
        .into()
}

fn activity_route(ctx: &NavContext<'_>) -> Menu {
    if ctx.has_vault || ctx.has_imported_bitcoin {
        Menu::Vault(VaultSubMenu::Transactions(None))
    } else if features::spark(ctx.network).is_available() {
        Menu::Spark(SparkSubMenu::Transactions(None))
    } else {
        Menu::Liquid(LiquidSubMenu::Transactions(None))
    }
}

fn wallet_children<'a>(
    menu: &Menu,
    ctx: &NavContext<'a>,
    group: TopLevel,
) -> Vec<Element<'a, Message>> {
    let items = match group {
        TopLevel::Spark => super::spark::items(ctx),
        TopLevel::Liquid => super::liquid::items(ctx),
        TopLevel::Vault => super::vault::items(ctx),
        TopLevel::Marketplace => marketplace::items(ctx),
        TopLevel::Cube => return Vec::new(),
    };
    items
        .into_iter()
        .filter(|item| {
            if group == TopLevel::Vault {
                !matches!(
                    &item.route,
                    Menu::Vault(
                        VaultSubMenu::Coins(_)
                            | VaultSubMenu::PSBTs(_)
                            | VaultSubMenu::Recovery
                            | VaultSubMenu::Claim
                            | VaultSubMenu::Settings(_)
                    )
                )
            } else if group == TopLevel::Spark {
                !matches!(&item.route, Menu::Spark(SparkSubMenu::Settings(_)))
            } else if group == TopLevel::Liquid {
                !matches!(&item.route, Menu::Liquid(LiquidSubMenu::Settings(_)))
            } else {
                true
            }
        })
        .map(|item| subitem(menu, item, None))
        .collect()
}

fn settings_children<'a>(menu: &Menu, ctx: &NavContext<'a>) -> Vec<Element<'a, Message>> {
    let mut items = Vec::new();
    items.push(route(
        "Cube",
        None,
        menu,
        Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::General)),
        true,
    ));
    if ctx.has_vault || ctx.has_imported_bitcoin {
        items.push(route(
            "Vault",
            None,
            menu,
            Menu::Vault(VaultSubMenu::Settings(Some(SettingsOption::Wallet))),
            true,
        ));
    }
    if !ctx.network.is_blake2b() {
        items.push(route(
            "Spark",
            None,
            menu,
            Menu::Spark(SparkSubMenu::Settings(Some(SparkSettingsOption::General))),
            true,
        ));
    }
    if !ctx.network.is_blake2b() && ctx.liquid_gate.show() {
        items.push(route(
            "Liquid",
            None,
            menu,
            Menu::Liquid(LiquidSubMenu::Settings(None)),
            true,
        ));
    }
    if let Some(subpages) = super::tertiary::items_for(menu, ctx) {
        for item in subpages {
            let label = match (&item.route, item.label) {
                (Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::Recovery)), _) => {
                    "Backup & protection"
                }
                (Menu::Vault(VaultSubMenu::Settings(Some(SettingsOption::Wallet))), _) => {
                    "Wallet & signers"
                }
                (Menu::Vault(VaultSubMenu::Settings(Some(SettingsOption::LocalSigning))), _) => {
                    "Pair with Keychain"
                }
                (Menu::Vault(VaultSubMenu::Settings(Some(SettingsOption::ImportExport))), _) => {
                    "Data & exports"
                }
                _ => item.label,
            };
            items.push(subitem(menu, item, Some(label)));
        }
    }
    items
}

/// One anchored sidebar, with only its middle group scrolling.
pub fn sidebar<'a>(menu: &Menu, ctx: &NavContext<'a>, state: ShellState) -> Element<'a, Message> {
    let overview = Menu::Cube(CubeSubMenu::Overview);
    let backup = Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::Recovery));
    let activity = activity_route(ctx);
    let mut top: Column<Message> = Column::new().spacing(2).padding([0, 10]);
    top = top
        .push(route("Overview", Some(home_icon()), menu, overview, false))
        .push(nav_row(
            "Activity",
            Some(receipt_icon()),
            matches!(
                menu,
                Menu::Vault(VaultSubMenu::Transactions(_))
                    | Menu::Spark(SparkSubMenu::Transactions(_))
                    | Menu::Liquid(LiquidSubMenu::Transactions(_))
            ),
            Some(Message::Menu(activity)),
            false,
            None,
        ))
        .push(route(
            "Backup & protection",
            Some(recovery_icon()),
            menu,
            backup,
            false,
        ));

    let mut middle: Column<Message> = Column::new().spacing(2).padding([0, 10]);
    middle = middle.push(section("WALLETS"));
    if !ctx.network.is_blake2b() {
        let spark_reason = features::spark(ctx.network).reason().map(str::to_owned);
        middle = middle.push(nav_row(
            "Spark",
            Some(lightning_outline_icon()),
            matches!(menu, Menu::Spark(_)),
            spark_reason
                .is_none()
                .then_some(Message::Menu(Menu::Spark(SparkSubMenu::Overview))),
            false,
            spark_reason,
        ));
        if matches!(menu, Menu::Spark(_)) {
            for item in wallet_children(menu, ctx, TopLevel::Spark) {
                middle = middle.push(item);
            }
        }
    }
    if ctx.has_vault || ctx.has_imported_bitcoin {
        middle = middle.push(nav_row(
            "Vault",
            Some(vault_icon()),
            matches!(menu, Menu::Vault(_)),
            Some(Message::Menu(Menu::Vault(VaultSubMenu::Overview))),
            false,
            None,
        ));
        if matches!(menu, Menu::Vault(_)) {
            for item in wallet_children(menu, ctx, TopLevel::Vault) {
                middle = middle.push(item);
            }
        }
    } else {
        middle = middle.push(nav_row(
            "Set up Vault",
            Some(vault_icon()),
            false,
            Some(Message::SetupVault),
            false,
            None,
        ));
    }
    if !ctx.network.is_blake2b() && ctx.liquid_gate.show() {
        let liquid_reason = features::liquid(ctx.network).reason().map(str::to_owned);
        middle = middle.push(nav_row(
            "Liquid",
            None,
            matches!(menu, Menu::Liquid(_)),
            liquid_reason
                .is_none()
                .then_some(Message::Menu(Menu::Liquid(LiquidSubMenu::Overview))),
            false,
            liquid_reason,
        ));
        if matches!(menu, Menu::Liquid(_)) {
            for item in wallet_children(menu, ctx, TopLevel::Liquid) {
                middle = middle.push(item);
            }
        }
    }
    let show_marketplace = (ctx.has_p2p && ctx.marketplace_flags.p2p_on())
        || (ctx.has_vault && ctx.marketplace_flags.buy_sell_on());
    if show_marketplace {
        middle = middle.push(nav_row(
            "Marketplace",
            Some(shop_icon()),
            matches!(menu, Menu::Marketplace(_)),
            Some(Message::Menu(super::primary::marketplace_landing_menu(ctx))),
            false,
            None,
        ));
        if matches!(menu, Menu::Marketplace(_)) {
            for item in wallet_children(menu, ctx, TopLevel::Marketplace) {
                middle = middle.push(item);
            }
            if let Some(subpages) = super::tertiary::items_for(menu, ctx) {
                for item in subpages {
                    middle = middle.push(subitem(menu, item, None));
                }
            }
        }
    }
    if ctx.has_vault || ctx.has_imported_bitcoin {
        middle = middle.push(section("VAULT"));
        middle = middle.push(nav_row(
            "Tools",
            Some(coins_outline_icon()),
            state.tools_open,
            Some(Message::ToggleVaultTools),
            false,
            None,
        ));
        if state.tools_open {
            let tools = super::vault::items(ctx);
            for item in tools.into_iter().filter(|item| {
                matches!(
                    &item.route,
                    Menu::Vault(
                        VaultSubMenu::Coins(_)
                            | VaultSubMenu::PSBTs(_)
                            | VaultSubMenu::Recovery
                            | VaultSubMenu::Claim
                    )
                )
            }) {
                let label = match &item.route {
                    Menu::Vault(VaultSubMenu::Coins(_)) => "Coin control",
                    Menu::Vault(VaultSubMenu::PSBTs(_)) => "PSBT proposals",
                    Menu::Vault(VaultSubMenu::Recovery) => "Recovery path",
                    Menu::Vault(VaultSubMenu::Claim) => item.label,
                    _ => item.label,
                };
                middle = middle.push(subitem(menu, item, Some(label)));
            }
        }
    }
    if matches!(
        menu,
        Menu::Cube(CubeSubMenu::Settings(_))
            | Menu::Vault(VaultSubMenu::Settings(_))
            | Menu::Spark(SparkSubMenu::Settings(_))
            | Menu::Liquid(LiquidSubMenu::Settings(_))
    ) {
        middle = middle.push(section("SETTINGS"));
        for item in settings_children(menu, ctx) {
            middle = middle.push(item);
        }
    }

    let settings_active = matches!(
        menu,
        Menu::Cube(CubeSubMenu::Settings(_))
            | Menu::Vault(VaultSubMenu::Settings(_))
            | Menu::Spark(SparkSubMenu::Settings(_))
            | Menu::Liquid(LiquidSubMenu::Settings(_))
    );
    let foot = column![
        rule::horizontal(1),
        container(nav_row(
            "Settings",
            Some(settings_icon()),
            settings_active,
            Some(Message::Menu(Menu::Cube(CubeSubMenu::Settings(
                CubeSettingsOption::General
            )))),
            false,
            None
        ))
        .id("cube-sidebar-settings"),
    ]
    .spacing(8)
    .padding([8, 10]);

    let mut sidebar: Column<Message> = Column::new()
        .push(identity(ctx, state))
        .width(Length::Fill)
        .height(Length::Fill);
    if state.switcher_open {
        sidebar = sidebar.push(container(switcher_menu(ctx, state)).padding([0, 8]));
    }
    sidebar = sidebar
        .push(top)
        .push(scrollable(middle).id(MIDDLE_SCROLL_ID).height(Length::Fill))
        .push_maybe(state.backup_attention.then(|| {
            nav_row(
                "Backup not confirmed · Review",
                None,
                false,
                Some(Message::Menu(Menu::Cube(CubeSubMenu::Settings(
                    CubeSettingsOption::Recovery,
                )))),
                false,
                None,
            )
        }))
        .push(foot);
    container(sidebar)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::container::shell_rail)
        .into()
}

#[cfg(test)]
mod keyboard_tests;
