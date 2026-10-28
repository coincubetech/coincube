//! The single text sidebar shared by every open-Cube panel.
//!
//! Keep routes in the existing `Menu` model while screen families migrate.
//! Settings and Tools each have one home here; old destination handlers stay
//! reachable without rendering a second or third rail.

use super::{marketplace, NavContext, SubItem};
use crate::{
    app::{
        features,
        menu::{CubeSettingsOption, CubeSubMenu, LiquidSubMenu, Menu, SparkSubMenu, VaultSubMenu},
        state::preferences::{BalancePrivacy, BalancePrivacyChrome},
        view::{shared::glyph, Message},
        ConnectionStatus,
    },
    chain::{ChainId, ChainIdExt},
};
use coincube_ui::{
    font,
    icon::{cube_icon, eye_icon, eye_slash_icon, settings_icon, shop_icon},
    theme,
    widget::{action::Action, floating::Floating, Column, ColumnExt, Element, Row, Text},
};
use iced::{
    widget::{column, container, row, rule, scrollable, text, Space},
    Alignment, Length,
};

/// Where a rejected device is re-registered (Cube Settings → About).
pub const DEVICE_REJECTED_ROUTE: Menu =
    Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::About));

pub const FULL_WIDTH: f32 = 248.0;
pub const COMPACT_WIDTH: f32 = 208.0;
pub const TOP_BAR_HEIGHT: f32 = 56.0;
pub const LAUNCHER_RETURN_ID: &str = "tenshu.launcher.return";
pub const CUBE_SWITCHER_ID: &str = "tenshu.cube.switcher";
pub const CUBE_LOCK_ID: &str = "tenshu.cube.lock.ask";
pub const CUBE_LOCK_CANCEL_ID: &str = "tenshu.cube.lock.cancel";
pub const SETTINGS_ID: &str = "tenshu.shell.settings";
pub const CONNECT_REREGISTER_ID: &str = "tenshu.connect.reregister";
pub const MIDDLE_SCROLL_ID: &str = "tenshu.shell.middle-scroll";
pub const BALANCE_PRIVACY_ID: &str = "tenshu.balance-privacy.toggle";
pub const APP_PREFERENCES_ID: &str = "tenshu.shell.app-preferences";

/// Innermost shell dismissal. The GUI owns Escape dispatch; this helper only
/// identifies the existing harmless intent for the currently open disclosure.
pub fn escape_intent(state: ShellState) -> Option<Message> {
    if state.lock_confirmation {
        Some(Message::CancelCubeLock)
    } else if state.switcher_open {
        Some(Message::ToggleCubeSwitcher)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ShellState {
    pub switcher_open: bool,
    pub lock_confirmation: bool,
    pub backup_attention: bool,
}

pub(crate) fn network_label(network: ChainId) -> &'static str {
    match network {
        ChainId::Bitcoin => "Bitcoin mainnet",
        ChainId::BitcoinBlake2b => "Bitcoin Blake2b mainnet",
        other => other.label(),
    }
}

/// The Launcher's coin for the Cube's chain, beside its network name.
fn network_coin<'a>(ctx: &NavContext<'a>) -> Element<'a, Message> {
    let light = matches!(
        ctx.theme_mode,
        coincube_ui::theme::palette::ThemeMode::Light
    );
    iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(
        crate::home::launcher_coin::widget_source(ctx.network, 14.0, 1.0, light).into_bytes(),
    ))
    .width(Length::Fixed(14.0))
    .height(Length::Fixed(14.0))
    .into()
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

/// Legacy entry point for callers without the GUI privacy owner. Cube routes
/// mount `top_bar_with_privacy` from the shared dashboard shell.
pub fn top_bar<'a>(ctx: &NavContext<'a>) -> Element<'a, Message> {
    top_bar_with_privacy(ctx, None)
}

pub fn balance_privacy_label(chrome: BalancePrivacyChrome) -> &'static str {
    match (chrome.busy, chrome.privacy, chrome.intent().is_some()) {
        (true, _, _) => "Saving balance privacy…",
        (false, BalancePrivacy::Unresolved, true) => "Balances hidden for now. Show balances",
        (false, BalancePrivacy::Unresolved, false) => "Balances hidden until your choice loads",
        (false, BalancePrivacy::Hidden, true) => "Show balances",
        (false, BalancePrivacy::Hidden, false) => "Balances hidden",
        (false, BalancePrivacy::HiddenLocally, true) => "Balances hidden now. Retry Hide",
        (false, BalancePrivacy::HiddenLocally, false) => "Balances hidden now",
        (false, BalancePrivacy::Shown, true) => "Hide balances",
        (false, BalancePrivacy::Shown, false) => "Balances shown",
    }
}

pub fn top_bar_with_privacy<'a>(
    ctx: &NavContext<'a>,
    privacy: Option<BalancePrivacyChrome>,
) -> Element<'a, Message> {
    let brand = Action::new(
        LAUNCHER_RETURN_ID,
        row![cube_icon().size(17), text("Tenshu").size(15)]
            .spacing(8)
            .align_y(Alignment::Center),
        Message::FocusAction,
    )
    .padding([7, 8])
    .style(theme::button::shell_brand)
    .on_press(Message::OpenLauncher);

    let account: Element<'a, Message> = if ctx.connect_authenticated {
        container(
            Action::new(
                "tenshu.connect.account",
                status("Signed in", true),
                Message::FocusAction,
            )
            .padding([6, 11])
            .style(theme::button::shell_nav)
            .on_press(Message::OpenConnectSignIn),
        )
        .style(theme::container::shell_chip)
        .into()
    } else {
        container(
            Action::new(
                "tenshu.connect.sign-in",
                status("Signed out", false),
                Message::FocusAction,
            )
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
        ConnectionStatus::DeviceRejected => ("Device not recognized", false),
        ConnectionStatus::Inactive => ("Not checked", false),
    };
    // A rejected device has one fix, re-registering it, and its only button
    // is on the About page. The chip is the way there.
    let connection_chip: Element<'a, Message> =
        if *ctx.connect_stream_status == ConnectionStatus::DeviceRejected {
            container(
                Action::new(
                    CONNECT_REREGISTER_ID,
                    status(connection, healthy),
                    Message::FocusAction,
                )
                .padding([6, 11])
                .style(theme::button::shell_nav)
                .on_press(Message::Menu(DEVICE_REJECTED_ROUTE)),
            )
            .style(theme::container::shell_chip)
            .into()
        } else {
            container(status(connection, healthy))
                .padding([7, 11])
                .style(theme::container::shell_chip)
                .into()
        };
    let connection = iced::widget::tooltip(
        connection_chip,
        container(muted(
            match ctx.connect_stream_status {
                ConnectionStatus::DeviceRejected => {
                    format!("{} Click to re-register.", crate::app::DEVICE_REJECTED_HINT)
                }
                _ => format!("Cube connection: {connection}"),
            },
            12,
        ))
        .padding([6, 10])
        .style(theme::container::shell_menu),
        iced::widget::tooltip::Position::Bottom,
    );

    let eye = privacy.map(|chrome| {
        let glyph = if chrome.privacy.masks_amounts() {
            eye_slash_icon()
        } else {
            eye_icon()
        };
        let action = Action::new(BALANCE_PRIVACY_ID, glyph.size(16), Message::FocusAction)
            .padding([7, 9])
            .style(theme::button::shell_nav)
            .on_press_maybe(chrome.intent().map(Message::ChangeDevicePreference));
        iced::widget::tooltip(
            action,
            container(muted(balance_privacy_label(chrome), 12))
                .padding([6, 10])
                .style(theme::container::shell_menu),
            iced::widget::tooltip::Position::Bottom,
        )
    });
    let preferences = iced::widget::tooltip(
        Action::new(
            APP_PREFERENCES_ID,
            settings_icon().size(16),
            Message::FocusAction,
        )
        .padding([7, 9])
        .style(theme::button::shell_nav)
        .on_press(Message::OpenAppPreferences),
        container(muted("App preferences", 12))
            .padding([6, 10])
            .style(theme::container::shell_menu),
        iced::widget::tooltip::Position::Bottom,
    );
    container(
        row![brand, Space::new().width(Length::Fill), account, connection]
            .push(eye)
            .push(preferences)
            .spacing(8)
            .align_y(Alignment::Center)
            .padding([0, 16]),
    )
    .height(Length::Fixed(TOP_BAR_HEIGHT))
    .align_y(iced::alignment::Vertical::Center)
    .width(Length::Fill)
    .style(theme::container::shell_top)
    .into()
}

/// A rail icon: the canvas's stroke glyph, or a font icon where the canvas
/// draws none (Marketplace).
enum NavIcon {
    Glyph(&'static str),
    Font(Text<'static>),
}

fn nav_row<'a>(
    label: impl Into<String>,
    icon: Option<NavIcon>,
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
    // `.ni .ic` is muted; `.ni.on .ic` takes the accent ink.
    let tint = if active { glyph::accent } else { glyph::muted };
    match icon {
        Some(NavIcon::Glyph(source)) => body = body.push(glyph::glyph(source, 18.0, tint)),
        Some(NavIcon::Font(icon)) => {
            body = body.push(icon.size(17).width(Length::Fixed(18.0)).style(
                move |t: &theme::Theme| iced::widget::text::Style {
                    color: Some(tint(t)),
                },
            ))
        }
        None => {}
    }
    // `.ni.on { font-weight: 600 }`.
    body = body.push(
        text(label.clone())
            .size(if nested { 13 } else { 14 })
            .font(if active {
                font::SEMIBOLD
            } else {
                font::REGULAR
            }),
    );
    let id = if nested {
        format!("tenshu.shell.sub.{label}.{message:?}")
    } else {
        match label.as_str() {
            "Lock Cube" => CUBE_LOCK_ID.to_owned(),
            "Settings" => SETTINGS_ID.to_owned(),
            "All Cubes" => "tenshu.launcher.all-cubes".to_owned(),
            _ => format!("tenshu.shell.nav.{label}"),
        }
    };
    let mut button = Action::new(id, body, Message::FocusAction)
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
    icon: Option<NavIcon>,
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

/// `.gh`: a 13px semibold muted group heading.
fn section(label: &'static str) -> Element<'static, Message> {
    container(muted(label, 13).font(font::SEMIBOLD))
        .padding(iced::Padding {
            top: 22.0,
            right: 10.0,
            bottom: 6.0,
            left: 10.0,
        })
        .into()
}

fn identity<'a>(ctx: &NavContext<'a>) -> Element<'a, Message> {
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
                // `.net { white-space: nowrap }`: the coin and the name stay on one line.
                row![
                    network_coin(ctx),
                    muted(network_label(ctx.network), 12)
                        .wrapping(iced::widget::text::Wrapping::None)
                ]
                .spacing(5)
                .align_y(Alignment::Center)
            ]
            .spacing(1)
            .width(Length::Fill),
            // `.side .cv`: one muted 16px chevron; the open menu shows the state.
            glyph::glyph(glyph::CHEVRON_DOWN, 16.0, glyph::muted),
        ]
        .spacing(9)
        .align_y(Alignment::Center),
        Message::FocusAction,
    )
    .padding([8, 10])
    .width(Length::Fill)
    .style(theme::button::shell_nav)
    .on_press(Message::ToggleCubeSwitcher);
    iced::widget::tooltip(
        container(control).padding([4, 8]),
        container(muted(ctx.cube_name, 12).wrapping(iced::widget::text::Wrapping::WordOrGlyph))
            .max_width(240)
            .max_height(120)
            .clip(true)
            .padding([6, 10])
            .style(theme::container::shell_menu),
        iced::widget::tooltip::Position::Right,
    )
    .into()
}

fn switcher_menu<'a>(ctx: &NavContext<'a>, state: ShellState) -> Element<'a, Message> {
    let mut items: Column<Message> = column![nav_row(
        "All Cubes",
        Some(NavIcon::Glyph(glyph::GRID)),
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
                        Action::new(
                            "tenshu.cube.lock.confirm",
                            text("Lock").size(13),
                            Message::FocusAction
                        )
                        .padding([6, 12])
                        .style(theme::button::primary)
                        .on_press(Message::LockCubeToLauncher)
                    )
                    .id("cube-lock-confirm"),
                    container(
                        Action::new(
                            CUBE_LOCK_CANCEL_ID,
                            text("Cancel").size(13),
                            Message::FocusAction
                        )
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
            Some(NavIcon::Glyph(glyph::LOCK)),
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

fn activity_route(_ctx: &NavContext<'_>) -> Option<Menu> {
    Some(Menu::Cube(CubeSubMenu::Activity))
}

/// Marketplace is the one sidebar entry with children (P2P, Buy/Sell), as
/// on the canvas. Wallets have none: Send, Receive and their history are
/// reached from each wallet's own page.
fn marketplace_children<'a>(menu: &Menu, ctx: &NavContext<'a>) -> Vec<Element<'a, Message>> {
    marketplace::items(ctx)
        .into_iter()
        .map(|item| subitem(menu, item, None))
        .collect()
}

pub const BACKUP_ATTENTION_ID: &str = "tenshu.shell.backup-attention";

/// `.attn`: a bordered panel card above the foot, a warning mark and
/// "Backup not confirmed" in the warning ink, with "Review" in the accent.
fn backup_attention<'a>() -> Element<'a, Message> {
    let body = row![
        glyph::glyph(glyph::WARNING, 16.0, glyph::warn),
        text("Backup not confirmed")
            .size(12.5)
            .font(font::MEDIUM)
            .style(|t: &theme::Theme| iced::widget::text::Style {
                color: Some(t.colors.shell.warn),
            }),
        Space::new().width(Length::Fill),
        text("Review")
            .size(12.5)
            .font(font::SEMIBOLD)
            .style(|t: &theme::Theme| iced::widget::text::Style {
                color: Some(t.colors.shell.accent_ink),
            }),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    container(
        Action::new(BACKUP_ATTENTION_ID, body, Message::FocusAction)
            .width(Length::Fill)
            .padding([9, 8])
            .style(|t: &theme::Theme, status| {
                let shell = t.colors.shell;
                let hovered = matches!(
                    status,
                    iced::widget::button::Status::Hovered | iced::widget::button::Status::Pressed
                );
                iced::widget::button::Style {
                    background: Some(iced::Background::Color(if hovered {
                        shell.selected
                    } else {
                        shell.panel
                    })),
                    text_color: shell.ink,
                    border: iced::Border {
                        color: shell.line,
                        width: 1.0,
                        radius: 8.0.into(),
                    },
                    ..Default::default()
                }
            })
            .on_press(Message::Menu(Menu::Cube(CubeSubMenu::Settings(
                CubeSettingsOption::Recovery,
            )))),
    )
    .padding(iced::Padding {
        top: 0.0,
        right: 16.0,
        bottom: 8.0,
        left: 16.0,
    })
    .into()
}

/// One anchored sidebar, with only its middle group scrolling.
pub fn sidebar<'a>(menu: &Menu, ctx: &NavContext<'a>, state: ShellState) -> Element<'a, Message> {
    let overview = Menu::Cube(CubeSubMenu::Overview);
    let backup = Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::Recovery));
    let activity = activity_route(ctx);
    let activity_unavailable = false;
    // A focused row paints 4px beyond its hit bounds. Later selected rows
    // have opaque fills, so they need this clearance to keep the ring visible.
    let mut top: Column<Message> = Column::new().spacing(4).padding([0, 10]);
    top = top
        .push(route(
            "Overview",
            Some(NavIcon::Glyph(glyph::GRID)),
            menu,
            overview,
            false,
        ))
        .push(nav_row(
            "Activity",
            Some(NavIcon::Glyph(glyph::CLOCK)),
            matches!(menu, Menu::Cube(CubeSubMenu::Activity)),
            activity.map(Message::Menu),
            false,
            activity_unavailable.then(|| "No wallet with activity on this network".to_owned()),
        ))
        .push(route(
            "Backup & protection",
            Some(NavIcon::Glyph(glyph::SHIELD_CHECK)),
            menu,
            backup,
            false,
        ));

    // The scroll viewport clips drawing outside its content. Reserve more
    // than the focus ring's 4px outer extent at its first and last actions.
    let mut middle: Column<Message> = Column::new().spacing(4).padding([6, 10]);
    middle = middle.push(section("Wallets"));
    if !ctx.network.is_blake2b() {
        let spark_reason = features::spark(ctx.network).reason().map(str::to_owned);
        middle = middle.push(nav_row(
            "Spark",
            Some(NavIcon::Glyph(glyph::BOLT)),
            matches!(menu, Menu::Spark(_)),
            spark_reason
                .is_none()
                .then_some(Message::Menu(Menu::Spark(SparkSubMenu::Overview))),
            false,
            spark_reason,
        ));
    }
    if ctx.has_vault || ctx.has_imported_bitcoin {
        middle = middle.push(nav_row(
            "Vault",
            Some(NavIcon::Glyph(glyph::SHIELD)),
            matches!(menu, Menu::Vault(_)),
            Some(Message::Menu(Menu::Vault(VaultSubMenu::Overview))),
            false,
            None,
        ));
    } else {
        middle = middle.push(nav_row(
            "Set up Vault",
            Some(NavIcon::Glyph(glyph::SHIELD)),
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
            Some(NavIcon::Glyph(glyph::DROPLET)),
            matches!(menu, Menu::Liquid(_)),
            liquid_reason
                .is_none()
                .then_some(Message::Menu(Menu::Liquid(LiquidSubMenu::Overview))),
            false,
            liquid_reason,
        ));
    }
    let show_marketplace = (ctx.has_p2p && ctx.marketplace_flags.p2p_on())
        || (ctx.has_vault && ctx.marketplace_flags.buy_sell_on());
    if show_marketplace {
        middle = middle.push(nav_row(
            "Marketplace",
            Some(NavIcon::Font(shop_icon())),
            matches!(menu, Menu::Marketplace(_)),
            Some(Message::Menu(super::primary::marketplace_landing_menu(ctx))),
            false,
            None,
        ));
        if matches!(menu, Menu::Marketplace(_)) {
            for item in marketplace_children(menu, ctx) {
                middle = middle.push(item);
            }
            if let Some(subpages) = super::tertiary::items_for(menu, ctx) {
                for item in subpages {
                    middle = middle.push(subitem(menu, item, None));
                }
            }
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
            Some(NavIcon::Glyph(glyph::GEAR)),
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
        .push(identity(ctx))
        .width(Length::Fill)
        .height(Length::Fill);
    // The menu covers the rows below it rather than pushing them down.
    if state.switcher_open {
        sidebar = sidebar.push(Floating::new(
            container(switcher_menu(ctx, state)).padding([0, 8]),
        ));
    }
    sidebar = sidebar
        .push(top)
        .push(scrollable(middle).id(MIDDLE_SCROLL_ID).height(Length::Fill))
        .push_maybe(state.backup_attention.then(backup_attention))
        .push(foot);
    container(sidebar)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::container::shell_rail)
        .into()
}

/// What the runtime does with one event, for tests that drive the shell
/// without it: overlays (the switcher menu) first, the base only if they
/// leave the event uncaptured, and with no cursor where they cover it.
#[cfg(test)]
pub(crate) fn test_send(
    element: &mut Element<'_, Message>,
    tree: &mut iced::advanced::widget::Tree,
    node: &iced::advanced::layout::Node,
    renderer: &iced::Renderer,
    event: &iced::Event,
    cursor: iced::mouse::Cursor,
    size: iced::Size,
) -> Vec<Message> {
    use iced::advanced::{clipboard::Null, Layout, Shell};
    let mut messages = Vec::new();
    let viewport = iced::Rectangle::with_size(size);
    let mut base_cursor = cursor;
    if let Some(mut overlay) = element.as_widget_mut().overlay(
        tree,
        Layout::new(node),
        renderer,
        &viewport,
        iced::Vector::ZERO,
    ) {
        let overlay = overlay.as_overlay_mut();
        let overlay_node = overlay.layout(renderer, size);
        let mut shell = Shell::new(&mut messages);
        overlay.update(
            event,
            Layout::new(&overlay_node),
            cursor,
            renderer,
            &mut Null,
            &mut shell,
        );
        if shell.is_event_captured() {
            return messages;
        }
        if overlay.mouse_interaction(Layout::new(&overlay_node), cursor, renderer)
            != iced::mouse::Interaction::None
        {
            base_cursor = iced::mouse::Cursor::Unavailable;
        }
    }
    element.as_widget_mut().update(
        tree,
        event,
        Layout::new(node),
        base_cursor,
        renderer,
        &mut Null,
        &mut Shell::new(&mut messages),
        &viewport,
    );
    messages
}

/// Draws the shell and then its overlays on top, as the runtime does.
#[cfg(test)]
pub(crate) fn test_draw(
    element: &mut Element<'_, Message>,
    tree: &mut iced::advanced::widget::Tree,
    node: &iced::advanced::layout::Node,
    renderer: &mut iced::Renderer,
    theme: &theme::Theme,
    size: iced::Size,
) {
    use iced::advanced::{renderer::Style, Layout, Renderer as _};
    let viewport = iced::Rectangle::with_size(size);
    let style = Style {
        text_color: theme.colors.shell.ink,
    };
    element.as_widget().draw(
        tree,
        renderer,
        theme,
        &style,
        Layout::new(node),
        iced::mouse::Cursor::Unavailable,
        &viewport,
    );
    if let Some(mut overlay) = element.as_widget_mut().overlay(
        tree,
        Layout::new(node),
        renderer,
        &viewport,
        iced::Vector::ZERO,
    ) {
        let overlay = overlay.as_overlay_mut();
        let overlay_node = overlay.layout(renderer, size);
        renderer.with_layer(viewport, |renderer| {
            overlay.draw(
                renderer,
                theme,
                &style,
                Layout::new(&overlay_node),
                iced::mouse::Cursor::Unavailable,
            );
        });
    }
}

#[cfg(test)]
mod keyboard_tests;

#[cfg(test)]
mod privacy_eye_tests {
    use super::*;
    use crate::app::settings::preferences::PreferenceChange;

    #[test]
    fn eye_copy_matches_persisted_intent_and_failed_hide_latch() {
        let chrome = |privacy| BalancePrivacyChrome {
            privacy,
            busy: false,
            interactive: true,
        };
        let shown = chrome(BalancePrivacy::Shown);
        assert_eq!(balance_privacy_label(shown), "Hide balances");
        assert_eq!(shown.intent(), Some(PreferenceChange::HideBalances(true)));

        let hidden = chrome(BalancePrivacy::Hidden);
        assert_eq!(balance_privacy_label(hidden), "Show balances");
        assert_eq!(hidden.intent(), Some(PreferenceChange::HideBalances(false)));

        let failed_hide = chrome(BalancePrivacy::HiddenLocally);
        assert_eq!(
            balance_privacy_label(failed_hide),
            "Balances hidden now. Retry Hide"
        );
        assert_eq!(
            failed_hide.intent(),
            Some(PreferenceChange::HideBalances(true))
        );

        let busy = BalancePrivacyChrome {
            busy: true,
            ..shown
        };
        assert_eq!(busy.intent(), None);
    }
}
