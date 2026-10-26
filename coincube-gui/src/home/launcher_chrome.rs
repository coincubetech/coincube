//! The Launcher's top bar, dock and Key card, from P-Launcher's `.top`,
//! `.bottom` and `.keycard`. Sizes, weights and colours follow the
//! prototype's CSS; every control keeps the catalog's existing ids and
//! messages, so focus, the network gate and the quota stay authoritative.

use iced::{
    advanced::widget::Id,
    widget::{button as iced_button, container, mouse_area, svg, text, Space},
    Alignment, Border, Color, Length, Padding,
};

use coincube_ui::{
    font, theme,
    widget::{action::Action, Column, Container, Element, Row},
};

use super::{
    launcher_catalog::{self, CatalogMessage, Projection},
    launcher_coin, launcher_scene, Home, HomeSection, ViewMessage,
};
use crate::{
    app::state::preferences::{BalancePrivacy, BalancePrivacyChrome},
    chain::{ChainId, ChainIdExt},
    feature_flags,
};

/// `.top` and `.bottom`: 64px bars with a 24px gutter.
pub(super) const BAR_HEIGHT: f32 = 64.0;
const GUTTER: f32 = 24.0;
/// `.keycard`: 264px wide, 8px above the bar.
const KEY_CARD_WIDTH: f32 = 264.0;
/// `.keybtn { height: 32px }`.
const KEY_BUTTON_HEIGHT: f32 = 32.0;

const CUBE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linejoin="round"><path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/><path d="M4 7.5l8 4.5 8-4.5M12 12v9"/></svg>"##;
const EYE_OPEN: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="3"/></svg>"##;
const EYE_SHUT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M9.9 5.8A9.6 9.6 0 0 1 12 5.5c6 0 9.5 6.5 9.5 6.5a16 16 0 0 1-2.6 3.4M6.4 7.4C3.9 9.1 2.5 12 2.5 12S6 18.5 12 18.5c1.8 0 3.3-.5 4.6-1.2M9.9 9.9a3 3 0 0 0 4.2 4.2M4 4l16 16"/></svg>"##;
const PERSON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round"><circle cx="12" cy="8.5" r="3.5"/><path d="M5 20c.8-3.6 3.6-5.5 7-5.5s6.2 1.9 7 5.5"/></svg>"##;
const SLIDERS: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round"><path d="M4 7h10M18 7h2M4 17h4M12 17h8"/><circle cx="16" cy="7" r="2"/><circle cx="10" cy="17" r="2"/></svg>"##;
const PLUS: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round"><path d="M12 5v14M5 12h14"/></svg>"##;
const IMPORT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 4v11M7.5 10.5L12 15l4.5-4.5M5 19h14"/></svg>"##;
const KEY: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="15" r="4"/><path d="M11 12l8-8M16 7l2 2M14 9l2 2"/></svg>"##;
const SHIELD: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linejoin="round"><path d="M12 3l7 3v5.5c0 4.3-3 7.6-7 9.5-4-1.9-7-5.2-7-9.5V6z"/></svg>"##;
const IMPORTED: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18M10 7.5v9m0-9h3c2.4 0 2.4 4 0 4h-3m0 0h3.6c2.6 0 2.6 4.5 0 4.5H10M11.5 6v1.5m0 9V18"/></svg>"##;
const LOCK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M7 11h10v9H7zM9 11V8a3 3 0 0 1 6 0v3"/></svg>"##;
const PASSKEY: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M9 5a4 4 0 1 0 0 8a4 4 0 1 0 0-8M12 11.5l7 7m-3-3 2.5-2.5m-.5 5 2.5-2.5"/></svg>"##;
const CHEVRON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M7 10l5 5 5-5"/></svg>"##;
const TICK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>"##;
const CHECK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18M8.5 12.2l2.4 2.4 4.6-5.1"/></svg>"##;

#[derive(Debug, Clone, Copy)]
pub(super) enum Ink {
    Body,
    Muted,
    Accent,
    /// `--on-accent`, text and icons on the primary fill.
    OnAccent,
    /// Lays out like the bar's brand without drawing it, to align a menu.
    /// Text only: Iced's SVG tint ignores alpha.
    Clear,
}

/// `.quiet` mutes the bar text while a Cube's face is up.
fn body(quiet: bool) -> Ink {
    if quiet {
        Ink::Muted
    } else {
        Ink::Body
    }
}

fn ink(theme: &theme::Theme, tone: Ink) -> Color {
    let shell = theme.colors.shell;
    match tone {
        Ink::Body => shell.ink,
        Ink::Muted => shell.muted,
        Ink::Accent => shell.accent_ink,
        Ink::OnAccent => iced::color!(0x201407),
        Ink::Clear => Color::TRANSPARENT,
    }
}

/// A prototype stroke icon, tinted like `currentColor`.
fn glyph<'a, M: 'a>(source: &'static str, size: f32, tone: Ink) -> Element<'a, M> {
    svg(svg::Handle::from_memory(source.as_bytes()))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |theme: &theme::Theme, _| svg::Style {
            color: Some(ink(theme, tone)),
        })
        .into()
}

fn words<'a, M: 'a>(
    value: impl Into<String>,
    size: f32,
    weight: iced::Font,
    tone: Ink,
) -> Element<'a, M> {
    text(value.into())
        .size(size)
        .font(weight)
        .style(move |theme: &theme::Theme| text::Style {
            color: Some(ink(theme, tone)),
        })
        .into()
}

fn focus(id: Id) -> ViewMessage {
    ViewMessage::Catalog(CatalogMessage::Focus(id))
}

/// `.tbtn`: a 32px borderless bar control whose border appears on hover.
fn bar_button(
    theme: &theme::Theme,
    status: iced_button::Status,
    pressed: bool,
) -> iced_button::Style {
    let shell = theme.colors.shell;
    let hovered = matches!(
        status,
        iced_button::Status::Hovered | iced_button::Status::Pressed
    );
    iced_button::Style {
        background: pressed.then_some(iced::Background::Color(shell.selected)),
        text_color: shell.ink,
        border: Border {
            color: if pressed {
                shell.accent
            } else if hovered {
                shell.line
            } else {
                Color::TRANSPARENT
            },
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

/// `.btn` and `.btn.primary`: 36px dock actions, 8px radius.
fn dock_button(
    theme: &theme::Theme,
    status: iced_button::Status,
    primary: bool,
) -> iced_button::Style {
    let shell = theme.colors.shell;
    let hovered = matches!(
        status,
        iced_button::Status::Hovered | iced_button::Status::Pressed
    );
    let disabled = matches!(status, iced_button::Status::Disabled);
    let fade = |c: Color| {
        if disabled {
            Color { a: c.a * 0.45, ..c }
        } else {
            c
        }
    };
    if primary {
        let fill = if hovered {
            iced::color!(0xffa54d)
        } else {
            shell.accent
        };
        iced_button::Style {
            background: Some(iced::Background::Color(fade(fill))),
            text_color: fade(iced::color!(0x201407)),
            border: Border {
                color: fade(fill),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        }
    } else {
        iced_button::Style {
            background: None,
            text_color: fade(shell.ink),
            border: Border {
                color: fade(if hovered { shell.muted } else { shell.line }),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        }
    }
}

pub(super) fn network_name(chain: ChainId) -> &'static str {
    match chain {
        ChainId::Bitcoin => "Bitcoin mainnet",
        ChainId::BitcoinBlake2b => "Bitcoin Blake2b mainnet",
        other => other.label(),
    }
}

/// `.chip`: 32px pill; `.chip.on` while a network narrows the Cubes shown.
fn chip_style(on: bool) -> impl Fn(&theme::Theme, iced_button::Status) -> iced_button::Style {
    move |theme, status| {
        let shell = theme.colors.shell;
        let hovered = matches!(
            status,
            iced_button::Status::Hovered | iced_button::Status::Pressed
        );
        iced_button::Style {
            background: on.then_some(iced::Background::Color(shell.selected)),
            text_color: shell.ink,
            border: Border {
                color: if on {
                    shell.accent
                } else if hovered {
                    shell.muted
                } else {
                    shell.line
                },
                width: 1.0,
                radius: 16.0.into(),
            },
            ..Default::default()
        }
    }
}

/// The `.menu` row for one network.
pub(super) fn network_option_id(chain: ChainId) -> Id {
    Id::from(format!("tenshu.launcher.network.{chain:?}"))
}

fn connect_entry<'a>(home: &Home, quiet: bool) -> Element<'a, ViewMessage> {
    let account = &home.connect_account;
    let overview = ViewMessage::GoToSection(HomeSection::Connect(
        crate::app::menu::ConnectSubMenu::Overview,
    ));
    let dot = || words("·", 13.0, font::REGULAR, Ink::Muted);
    let (id, content, message): (Id, Row<'a, ViewMessage>, Option<ViewMessage>) = if account
        .is_authenticated()
    {
        let mut row = Row::new()
            .spacing(8)
            .align_y(Alignment::Center)
            .push(glyph(PERSON, 16.0, body(quiet)))
            .push(words("Connect", 13.0, font::REGULAR, body(quiet)));
        if let Some(user) = account.user.as_ref().filter(|u| !u.email.is_empty()) {
            row = row
                .push(dot())
                .push(words(user.email.clone(), 13.0, font::REGULAR, body(quiet)));
        }
        let state = if account.is_checking_session() {
            "Checking…"
        } else {
            "Signed in"
        };
        (
            launcher_catalog::connect_id(),
            row.push(words(state, 12.0, font::REGULAR, Ink::Muted)),
            Some(overview),
        )
    } else if account.is_checking_session() {
        (
            launcher_catalog::connect_id(),
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(glyph(PERSON, 16.0, body(quiet)))
                .push(words("Connect", 13.0, font::REGULAR, body(quiet)))
                .push(dot())
                .push(words("Checking…", 12.0, font::REGULAR, Ink::Muted)),
            None,
        )
    } else {
        // `.tbtn.out .who`: the sign-in call to action in accent ink.
        (
            launcher_catalog::sign_in_id(),
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(glyph(PERSON, 16.0, body(quiet)))
                .push(words("Connect", 13.0, font::REGULAR, body(quiet)))
                .push(dot())
                .push(words("Sign in", 13.0, font::SEMIBOLD, Ink::Accent)),
            Some(overview),
        )
    };
    Action::new(id, Container::new(content).center_y(Length::Fill), focus)
        .height(Length::Fixed(32.0))
        .padding([0.0, 10.0])
        .style(|theme, status| bar_button(theme, status, false))
        .on_press_maybe(message)
        .into()
}

/// `.tbtn.eye`: the device-wide hide-balances switch. Its state and intent
/// come from the GUI's accepted privacy receipt, as in an open Cube.
fn privacy_eye<'a>(
    chrome: Option<BalancePrivacyChrome>,
    quiet: bool,
) -> Option<Element<'a, ViewMessage>> {
    let chrome = chrome?;
    let hidden = chrome.privacy.masks_amounts();
    let pressed = matches!(
        chrome.privacy,
        BalancePrivacy::Hidden | BalancePrivacy::HiddenLocally
    );
    Some(
        Action::new(
            launcher_catalog::privacy_id(),
            Container::new(glyph(
                if hidden { EYE_SHUT } else { EYE_OPEN },
                16.0,
                body(quiet),
            ))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
            focus,
        )
        .width(Length::Fixed(32.0))
        .height(Length::Fixed(32.0))
        .style(move |theme, status| bar_button(theme, status, pressed))
        .on_press_maybe(chrome.intent().map(ViewMessage::ChangeDevicePreference))
        .into(),
    )
}

/// `.top`: brand, network chip and count; then balances, Connect and App
/// preferences. The theme lives in App preferences, as in the prototype.
pub(super) fn top_bar<'a>(
    home: &'a Home,
    projection: &Projection<'_>,
    quiet: bool,
) -> Element<'a, ViewMessage> {
    let count = count_text(
        projection.locals.len() + projection.remotes.len(),
        projection.local_total + projection.remote_total,
    );

    // The chip narrows by network only when there is more than one to show.
    let several = home.displayed_networks.len() > 1;
    let label = if several {
        format!("Network: {}", network_name(home.network))
    } else {
        "Network".to_string()
    };
    let chip = Action::new(
        launcher_catalog::network_id(),
        Container::new(
            Row::new()
                .spacing(6)
                .align_y(Alignment::Center)
                .push(words(
                    label,
                    13.0,
                    if several && !quiet {
                        font::MEDIUM
                    } else {
                        font::REGULAR
                    },
                    body(quiet),
                ))
                .push(glyph(CHEVRON, 14.0, body(quiet))),
        )
        .center_y(Length::Fill),
        focus,
    )
    .height(Length::Fixed(32.0))
    .padding([0.0, 12.0])
    .style(chip_style(several && !quiet))
    .on_press(ViewMessage::Catalog(CatalogMessage::NetworkMenu(
        !home.catalog.network_menu,
    )));
    let separator = Container::new(Space::new())
        .width(Length::Fixed(1.0))
        .height(Length::Fixed(20.0))
        .style(|theme: &theme::Theme| container::Style {
            background: Some(iced::Background::Color(theme.colors.shell.line)),
            ..Default::default()
        });
    let preferences = Action::new(
        launcher_catalog::settings_id(),
        Container::new(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(glyph(SLIDERS, 16.0, body(quiet)))
                .push(words("App preferences", 13.0, font::REGULAR, body(quiet))),
        )
        .center_y(Length::Fill),
        focus,
    )
    .height(Length::Fixed(32.0))
    .padding([0.0, 10.0])
    .style(|theme, status| bar_button(theme, status, false))
    .on_press(ViewMessage::OpenAppPreferences);
    Container::new(
        Row::new()
            .spacing(16)
            .align_y(Alignment::Center)
            .height(Length::Fill)
            .push(brand(body(quiet)))
            .push(separator)
            .push(chip)
            .push(words(count, 13.0, font::REGULAR, Ink::Muted))
            .push(Space::new().width(Length::Fill))
            .push(privacy_eye(home.balance_privacy, quiet))
            .push(connect_entry(home, quiet))
            .push(preferences),
    )
    .height(Length::Fixed(BAR_HEIGHT))
    .padding([0.0, GUTTER])
    .width(Length::Fill)
    .style(rail)
    .into()
}

fn rail(theme: &theme::Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(theme.colors.shell.rail)),
        text_color: Some(theme.colors.shell.ink),
        ..Default::default()
    }
}

fn dock_action<'a>(
    id: Id,
    icon: &'static str,
    label: &'static str,
    primary: bool,
    message: Option<ViewMessage>,
    quiet: bool,
) -> Element<'a, ViewMessage> {
    // `.quiet .dock .btn.primary` drops the fill; every label goes muted.
    let primary = primary && !quiet;
    let tone = if primary { Ink::OnAccent } else { body(quiet) };
    Action::new(
        id,
        Container::new(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(glyph(icon, 16.0, tone))
                .push(words(label, 14.0, font::MEDIUM, tone)),
        )
        .center_y(Length::Fill),
        focus,
    )
    .height(Length::Fixed(36.0))
    .padding([0.0, 16.0])
    .style(move |theme, status| dock_button(theme, status, primary))
    .on_press_maybe(message)
    .into()
}

fn small_link<'a>(id: Id, label: &'static str, message: ViewMessage) -> Element<'a, ViewMessage> {
    Action::new(id, words(label, 13.0, font::MEDIUM, Ink::Accent), focus)
        .padding([4.0, 6.0])
        .style(|theme, status| bar_button(theme, status, false))
        .on_press(message)
        .into()
}

/// `.bottom`: Create and Import on the left, Key on the right. The app's
/// gated Recover/Split entries and the quota copy share the same 64px bar.
pub(super) fn dock<'a>(
    home: &Home,
    projection: &Projection<'_>,
    quiet: bool,
) -> Element<'a, ViewMessage> {
    let at_limit = projection.at_limit();
    let create = dock_action(
        launcher_catalog::create_id(),
        PLUS,
        "Create Cube",
        true,
        (!at_limit).then_some(ViewMessage::ShowCreateCube(true)),
        quiet,
    );
    let import = dock_action(
        launcher_catalog::import_id(),
        IMPORT,
        "Import Bitcoin wallet",
        false,
        (!at_limit && projection.network == ChainId::Bitcoin)
            .then_some(ViewMessage::ShowImportBitcoin),
        quiet,
    );
    let recover = (home.connect_account.is_authenticated()
        && !home.network.is_blake2b()
        && feature_flags::RECOVER_VAULT_ENABLED)
        .then(|| {
            small_link(
                launcher_catalog::recover_vault_id(),
                "Recover a Vault",
                ViewMessage::GoToSection(HomeSection::RecoverVault),
            )
        });
    let split = (home.network == ChainId::Bitcoin
        && home
            .connect_chain_availability(ChainId::BitcoinBlake2b)
            .is_available())
    .then(|| {
        small_link(
            launcher_catalog::split_wallet_id(),
            "Split a Bitcoin wallet",
            ViewMessage::GoToSection(HomeSection::SplitWallet),
        )
    });
    let limit = at_limit.then(|| {
        text(limit_text(projection))
            .size(12)
            .style(|theme: &theme::Theme| text::Style {
                color: Some(theme.colors.shell.muted),
            })
            .wrapping(text::Wrapping::WordOrGlyph)
            .width(Length::Fill)
    });
    let key = (projection.local_total > 0).then(|| key_button(home.catalog.key_open()));
    Container::new(
        Row::new()
            .spacing(8)
            .align_y(Alignment::Center)
            .height(Length::Fill)
            .push(create)
            .push(import)
            .push(recover)
            .push(split)
            .push(Space::new().width(Length::Fixed(16.0)))
            .push(limit)
            .push((!at_limit).then(|| Space::new().width(Length::Fill)))
            .push(key),
    )
    .height(Length::Fixed(BAR_HEIGHT))
    .padding([0.0, GUTTER])
    .width(Length::Fill)
    .style(rail)
    .into()
}

/// `.keybtn`: hover peeks, a click pins, a second click or Escape shuts.
fn key_button<'a>(open: bool) -> Element<'a, ViewMessage> {
    let button = Action::new(
        launcher_catalog::key_id(),
        Container::new(
            Row::new()
                .spacing(6)
                .align_y(Alignment::Center)
                .push(glyph(KEY, 16.0, Ink::Body))
                .push(words("Key", 13.0, font::REGULAR, Ink::Body)),
        )
        .center_y(Length::Fill),
        focus,
    )
    .height(Length::Fixed(KEY_BUTTON_HEIGHT))
    .padding([0.0, 10.0])
    .style(move |theme: &theme::Theme, status| {
        let mut style = bar_button(theme, status, false);
        if open {
            style.background = Some(iced::Background::Color(theme.colors.shell.panel));
            style.border.color = theme.colors.shell.line;
        }
        style
    })
    .on_press(ViewMessage::Catalog(CatalogMessage::KeyToggle));
    mouse_area(button)
        .on_enter(ViewMessage::Catalog(CatalogMessage::KeyHover(true)))
        .on_exit(ViewMessage::Catalog(CatalogMessage::KeyHover(false)))
        .into()
}

fn coin<'a>(chain: ChainId, light: bool) -> Element<'a, ViewMessage> {
    svg(svg::Handle::from_memory(
        launcher_coin::widget_source(chain, 16.0, 1.5, light).into_bytes(),
    ))
    .width(Length::Fixed(16.0))
    .height(Length::Fixed(16.0))
    .into()
}

/// `.sw`: a 12px face swatch in a network family's edge colour.
fn swatch<'a>(chain: ChainId, light: bool) -> Element<'a, ViewMessage> {
    let edge = launcher_scene::edge(chain, light);
    let face = launcher_scene::Palette::new(light).top;
    Container::new(Space::new())
        .width(Length::Fixed(12.0))
        .height(Length::Fixed(12.0))
        .style(move |_: &theme::Theme| container::Style {
            background: Some(iced::Background::Color(Color::from_rgb8(
                face[0], face[1], face[2],
            ))),
            border: Border {
                color: Color::from_rgba8(edge[0], edge[1], edge[2], edge[3] as f32 / 255.0),
                width: 1.5,
                radius: 2.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn legend_row<'a>(
    marks: Vec<Element<'a, ViewMessage>>,
    label: &'static str,
) -> Element<'a, ViewMessage> {
    let mut row = Row::new().spacing(6).align_y(Alignment::Center);
    for mark in marks {
        row = row.push(mark);
    }
    row.push(words(label, 13.0, font::REGULAR, Ink::Body))
        .into()
}

/// `.keycard`: how to read a Cube, using the marks this app actually draws.
pub(super) fn key_card<'a>(home: &Home) -> Element<'a, ViewMessage> {
    let light = matches!(
        home.theme_mode,
        coincube_ui::theme::palette::ThemeMode::Light
    );
    let b2b = home.displayed_networks.iter().any(|c| c.is_blake2b());
    let mut edges = vec![swatch(ChainId::Bitcoin, light)];
    if b2b {
        edges.push(swatch(ChainId::BitcoinBlake2b, light));
    }
    edges.push(swatch(ChainId::Testnet4, light));
    let mut coins = vec![coin(ChainId::Bitcoin, light)];
    if b2b {
        coins.push(coin(ChainId::BitcoinBlake2b, light));
    }
    Container::new(
        Column::new()
            .spacing(10)
            .push(legend_row(coins, "Top: network"))
            .push(legend_row(
                vec![
                    glyph(SHIELD, 16.0, Ink::Body),
                    glyph(IMPORTED, 16.0, Ink::Body),
                ],
                "Left: wallets",
            ))
            .push(legend_row(
                vec![
                    glyph(LOCK, 16.0, Ink::Body),
                    glyph(PASSKEY, 16.0, Ink::Body),
                    glyph(CHECK, 16.0, Ink::Body),
                ],
                "Right: lock, backup",
            ))
            .push(legend_row(edges, "Edge: network family")),
    )
    .width(Length::Fixed(KEY_CARD_WIDTH))
    .padding(Padding {
        top: 12.0,
        right: 14.0,
        bottom: 12.0,
        left: 14.0,
    })
    .style(|theme: &theme::Theme| container::Style {
        background: Some(iced::Background::Color(theme.colors.shell.panel)),
        border: Border {
            color: theme.colors.shell.line,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    })
    .into()
}

/// `.brand`: the cube mark and the product name.
fn brand<'a, M: 'a>(tone: Ink) -> Element<'a, M> {
    // Iced's SVG tint drops alpha, so the undrawn copy lays out a space.
    let mark: Element<'a, M> = match tone {
        Ink::Clear => Space::new()
            .width(Length::Fixed(20.0))
            .height(Length::Fixed(20.0))
            .into(),
        tone => glyph(CUBE, 20.0, tone),
    };
    Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(mark)
        .push(words("Tenshu", 15.0, font::SEMIBOLD, tone))
        .into()
}

/// `.menu`: 272px under the chip, one `.mi` row per listed network and the
/// filter note. Only the shown network's Cubes are loaded, so only its row
/// carries a count. A click anywhere else closes it.
pub(super) fn network_layer<'a>(home: &Home, shown: usize) -> Option<Element<'a, ViewMessage>> {
    if !home.catalog.network_menu {
        return None;
    }
    let close = ViewMessage::Catalog(CatalogMessage::NetworkMenu(false));
    let mut rows = Column::new().spacing(0);
    // As on the chip, a lone network is not a filter: no tick, no weight.
    let several = home.displayed_networks.len() > 1;
    for chain in &home.displayed_networks {
        let current = *chain == home.network;
        let on = current && several;
        let row = Row::new()
            .spacing(8)
            .align_y(Alignment::Center)
            .push(
                Container::new(on.then(|| glyph(TICK, 14.0, Ink::Body)))
                    .width(Length::Fixed(14.0))
                    .height(Length::Fixed(14.0)),
            )
            .push(words(
                network_name(*chain),
                13.0,
                if on { font::SEMIBOLD } else { font::REGULAR },
                Ink::Body,
            ))
            .push(Space::new().width(Length::Fill))
            .push(current.then(|| words(shown.to_string(), 12.0, font::REGULAR, Ink::Muted)));
        rows = rows.push(
            Action::new(
                network_option_id(*chain),
                Container::new(row).center_y(Length::Fill),
                focus,
            )
            .width(Length::Fill)
            .height(Length::Fixed(34.0))
            .padding([0.0, 10.0])
            .style(|theme: &theme::Theme, status| {
                let shell = theme.colors.shell;
                iced_button::Style {
                    background: matches!(
                        status,
                        iced_button::Status::Hovered | iced_button::Status::Pressed
                    )
                    .then_some(iced::Background::Color(shell.selected)),
                    text_color: shell.ink,
                    border: Border {
                        radius: 6.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            })
            .on_press(if current {
                close.clone()
            } else {
                ViewMessage::SelectNetwork(*chain)
            }),
        );
    }
    let note = Column::new()
        .padding(Padding {
            top: 6.0,
            right: 6.0,
            bottom: 4.0,
            left: 6.0,
        })
        .push(
            Container::new(Space::new())
                .width(Length::Fill)
                .height(Length::Fixed(1.0))
                .style(|theme: &theme::Theme| container::Style {
                    background: Some(iced::Background::Color(theme.colors.shell.line)),
                    ..Default::default()
                }),
        )
        .push(Space::new().height(Length::Fixed(8.0)))
        .push(words(
            "Filters change what is shown, never a Cube's network.",
            12.0,
            font::REGULAR,
            Ink::Muted,
        ));
    let menu = Container::new(rows.push(note))
        .width(Length::Fixed(272.0))
        .padding(6)
        .style(|theme: &theme::Theme| container::Style {
            background: Some(iced::Background::Color(theme.colors.shell.panel)),
            border: Border {
                color: theme.colors.shell.line,
                width: 1.0,
                radius: 10.0.into(),
            },
            // `0 18px 40px -12px rgba(0,0,0,.5)`; Iced has no spread.
            shadow: iced::Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
                offset: iced::Vector::new(0.0, 18.0),
                blur_radius: 40.0,
            },
            ..Default::default()
        });
    // `.menu { top: 40px }` from the chip, which sits 16px down the bar. An
    // undrawn copy of the brand and separator puts it under the chip.
    let placed = Column::new()
        .push(Space::new().height(Length::Fixed((BAR_HEIGHT - 32.0) / 2.0 + 40.0)))
        .push(
            Row::new()
                .spacing(16)
                .push(brand(Ink::Clear))
                .push(Space::new().width(Length::Fixed(1.0)))
                .push(mouse_area(menu)),
        )
        .padding([0.0, GUTTER]);
    Some(
        iced::widget::Stack::new()
            .width(Length::Fill)
            .height(Length::Fill)
            .push(mouse_area(Space::new().width(Length::Fill).height(Length::Fill)).on_press(close))
            .push(placed)
            .into(),
    )
}

/// The Key card's layer: bottom-right, 8px above the dock.
pub(super) fn key_layer<'a>(home: &Home) -> Option<Element<'a, ViewMessage>> {
    home.catalog.key_open().then(|| {
        // `bottom: calc(100% + 8px)` of the 32px button centred in the dock.
        // The 8px gap stays inside the hover area so a peek survives the
        // pointer's way up from the button to the card.
        let button_top = (BAR_HEIGHT + KEY_BUTTON_HEIGHT) / 2.0;
        Container::new(
            mouse_area(Container::new(key_card(home)).padding(Padding {
                bottom: 8.0,
                ..Padding::ZERO
            }))
            .on_enter(ViewMessage::Catalog(CatalogMessage::KeyHover(true)))
            .on_exit(ViewMessage::Catalog(CatalogMessage::KeyHover(false))),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::alignment::Horizontal::Right)
        .align_y(iced::alignment::Vertical::Bottom)
        .padding(Padding {
            top: 0.0,
            right: GUTTER,
            bottom: button_top,
            left: 0.0,
        })
        .into()
    })
}

/// What the PIN and passkey screens need to redraw the Launcher's bars
/// exactly as they stood when the Cube was opened from the floor.
#[derive(Debug, Clone, Default)]
pub(crate) struct ChromeSnapshot {
    chip: String,
    count: String,
    /// `Some(hidden)` while the balances eye is shown.
    eye: Option<bool>,
    connect: Vec<(String, Ink, f32, bool)>,
    create_label: &'static str,
    import_label: &'static str,
    links: Vec<&'static str>,
    limit: Option<String>,
    key: bool,
}

pub(super) fn snapshot(home: &Home, projection: &Projection<'_>) -> ChromeSnapshot {
    let shown = projection.locals.len() + projection.remotes.len();
    let total = projection.local_total + projection.remote_total;
    let account = &home.connect_account;
    let mut connect = vec![("Connect".to_string(), Ink::Muted, 13.0, false)];
    let dot = || ("·".to_string(), Ink::Muted, 13.0, false);
    if account.is_authenticated() {
        if let Some(user) = account.user.as_ref().filter(|u| !u.email.is_empty()) {
            connect.push(dot());
            connect.push((user.email.clone(), Ink::Muted, 13.0, false));
        }
        connect.push((
            if account.is_checking_session() {
                "Checking…"
            } else {
                "Signed in"
            }
            .to_string(),
            Ink::Muted,
            12.0,
            false,
        ));
    } else if account.is_checking_session() {
        connect.push(dot());
        connect.push(("Checking…".to_string(), Ink::Muted, 12.0, false));
    } else {
        connect.push(dot());
        connect.push(("Sign in".to_string(), Ink::Accent, 13.0, true));
    }
    let at_limit = projection.at_limit();
    let mut links = Vec::new();
    if account.is_authenticated()
        && !home.network.is_blake2b()
        && feature_flags::RECOVER_VAULT_ENABLED
    {
        links.push("Recover a Vault");
    }
    if home.network == ChainId::Bitcoin
        && home
            .connect_chain_availability(ChainId::BitcoinBlake2b)
            .is_available()
    {
        links.push("Split a Bitcoin wallet");
    }
    ChromeSnapshot {
        chip: if home.displayed_networks.len() > 1 {
            format!("Network: {}", network_name(home.network))
        } else {
            "Network".to_string()
        },
        count: count_text(shown, total),
        eye: home
            .balance_privacy
            .map(|chrome| chrome.privacy.masks_amounts()),
        connect,
        create_label: "Create Cube",
        import_label: "Import Bitcoin wallet",
        links,
        limit: at_limit.then(|| limit_text(projection)),
        key: projection.local_total > 0,
    }
}

fn count_text(shown: usize, total: usize) -> String {
    if shown == total {
        format!("{total} {}", if total == 1 { "Cube" } else { "Cubes" })
    } else {
        format!("{shown} of {total} Cubes · {} hidden", total - shown)
    }
}

fn limit_text(projection: &Projection<'_>) -> String {
    if projection.has_local_list {
        format!(
            "Cube limit reached ({}/{}) on the {} plan. Upgrade your Connect account to create more.",
            projection.total_count, projection.cube_limit, projection.tier,
        )
    } else {
        format!(
            "Cube limit reached ({}/{}) on the {} plan. Upgrade your Connect account or delete a remote Cube to create one here.",
            projection.total_count, projection.cube_limit, projection.tier,
        )
    }
}

/// A control drawn in its resting state, for bars that take no input.
fn resting<'a, M: 'a>(
    content: impl Into<Element<'a, M>>,
    height: f32,
    padding: f32,
    style: impl Fn(&theme::Theme) -> iced_button::Style + 'a,
) -> Element<'a, M> {
    Container::new(Container::new(content).center_y(Length::Fill))
        .height(Length::Fixed(height))
        .padding([0.0, padding])
        .style(move |theme: &theme::Theme| {
            let button = style(theme);
            container::Style {
                background: button.background,
                border: button.border,
                text_color: Some(button.text_color),
                ..Default::default()
            }
        })
        .into()
}

/// The `.top.quiet` bar under a Cube that is being unlocked.
pub(crate) fn quiet_top_bar<'a, M: 'a>(chrome: &ChromeSnapshot) -> Element<'a, M> {
    let quiet = true;
    let chip = resting(
        Row::new()
            .spacing(6)
            .align_y(Alignment::Center)
            .push(words(chrome.chip.clone(), 13.0, font::REGULAR, body(quiet)))
            .push(glyph(CHEVRON, 14.0, body(quiet))),
        32.0,
        12.0,
        |theme| chip_style(false)(theme, iced_button::Status::Active),
    );
    let separator = Container::new(Space::new())
        .width(Length::Fixed(1.0))
        .height(Length::Fixed(20.0))
        .style(|theme: &theme::Theme| container::Style {
            background: Some(iced::Background::Color(theme.colors.shell.line)),
            ..Default::default()
        });
    let eye = chrome.eye.map(|hidden| {
        Container::new(glyph(
            if hidden { EYE_SHUT } else { EYE_OPEN },
            16.0,
            body(quiet),
        ))
        .width(Length::Fixed(32.0))
        .height(Length::Fixed(32.0))
        .center_x(Length::Fixed(32.0))
        .center_y(Length::Fixed(32.0))
    });
    let mut connect =
        Row::new()
            .spacing(8)
            .align_y(Alignment::Center)
            .push(glyph(PERSON, 16.0, body(quiet)));
    for (value, tone, size, strong) in &chrome.connect {
        connect = connect.push(words(
            value.clone(),
            *size,
            if *strong {
                font::SEMIBOLD
            } else {
                font::REGULAR
            },
            *tone,
        ));
    }
    let resting_bar = |theme: &theme::Theme| bar_button(theme, iced_button::Status::Active, false);
    let preferences = Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(glyph(SLIDERS, 16.0, body(quiet)))
        .push(words("App preferences", 13.0, font::REGULAR, body(quiet)));
    Container::new(
        Row::new()
            .spacing(16)
            .align_y(Alignment::Center)
            .height(Length::Fill)
            .push(brand(body(quiet)))
            .push(separator)
            .push(chip)
            .push(words(chrome.count.clone(), 13.0, font::REGULAR, Ink::Muted))
            .push(Space::new().width(Length::Fill))
            .push(eye)
            .push(resting(connect, 32.0, 10.0, resting_bar))
            .push(resting(preferences, 32.0, 10.0, resting_bar)),
    )
    .height(Length::Fixed(BAR_HEIGHT))
    .padding([0.0, GUTTER])
    .width(Length::Fill)
    .style(rail)
    .into()
}

/// The `.bottom.quiet` dock under a Cube that is being unlocked.
pub(crate) fn quiet_dock<'a, M: 'a>(chrome: &ChromeSnapshot) -> Element<'a, M> {
    let quiet = true;
    let action = |icon: &'static str, label: &'static str| {
        resting(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(glyph(icon, 16.0, body(quiet)))
                .push(words(label, 14.0, font::MEDIUM, body(quiet))),
            36.0,
            16.0,
            |theme| dock_button(theme, iced_button::Status::Active, false),
        )
    };
    let mut row = Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .height(Length::Fill)
        .push(action(PLUS, chrome.create_label))
        .push(action(IMPORT, chrome.import_label));
    for link in &chrome.links {
        row = row.push(
            Container::new(words(*link, 13.0, font::MEDIUM, Ink::Accent)).padding([4.0, 6.0]),
        );
    }
    row = row.push(Space::new().width(Length::Fixed(16.0)));
    row = match &chrome.limit {
        Some(limit) => row.push(
            text(limit.clone())
                .size(12)
                .style(|theme: &theme::Theme| text::Style {
                    color: Some(theme.colors.shell.muted),
                })
                .wrapping(text::Wrapping::WordOrGlyph)
                .width(Length::Fill),
        ),
        None => row.push(Space::new().width(Length::Fill)),
    };
    // `.keybtn` is outside `.quiet`'s selectors, so it keeps its ink.
    let key = chrome.key.then(|| {
        resting(
            Row::new()
                .spacing(6)
                .align_y(Alignment::Center)
                .push(glyph(KEY, 16.0, Ink::Body))
                .push(words("Key", 13.0, font::REGULAR, Ink::Body)),
            KEY_BUTTON_HEIGHT,
            10.0,
            |theme| bar_button(theme, iced_button::Status::Active, false),
        )
    });
    Container::new(row.push(key))
        .height(Length::Fixed(BAR_HEIGHT))
        .padding([0.0, GUTTER])
        .width(Length::Fill)
        .style(rail)
        .into()
}
