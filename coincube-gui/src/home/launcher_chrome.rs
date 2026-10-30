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
    component::tenshu::top_bar::{self, APP_PREFERENCES, EYE_OPEN, EYE_SHUT, SLIDERS},
    font, theme,
    widget::{action::Action, Column, Container, Element, Row},
};

use super::{
    launcher_activity::{self, ActivityMessage},
    launcher_catalog::{self, CatalogMessage, KeyPart, NetworkFilter, Projection},
    launcher_coin, launcher_scene, Home, HomeSection, ViewMessage,
};
use crate::{
    app::settings::preferences::{NetworkActivitySource, PreferenceChange},
    app::state::preferences::BalancePrivacyChrome,
    chain::{ChainId, ChainIdExt},
    feature_flags,
};

/// `.top` and `.bottom`: 64px bars with a 24px gutter, as the shared top bar.
pub(super) const BAR_HEIGHT: f32 = top_bar::HEIGHT;
const GUTTER: f32 = top_bar::GUTTER;
/// `.keycard`: 264px wide, 8px above the bar.
const KEY_CARD_WIDTH: f32 = 264.0;
/// `.keybtn { height: 32px }`.
const KEY_BUTTON_HEIGHT: f32 = 32.0;
const ACTIVITY_MENU_WIDTH: f32 = 300.0;
/// About the Key button's width, so the activity menu opens over its own
/// button rather than the Key's.
const KEY_BUTTON_WIDTH: f32 = 72.0;

const PERSON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round"><circle cx="12" cy="8.5" r="3.5"/><path d="M5 20c.8-3.6 3.6-5.5 7-5.5s6.2 1.9 7 5.5"/></svg>"##;
const PLUS: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round"><path d="M12 5v14M5 12h14"/></svg>"##;
const IMPORT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 4v11M7.5 10.5L12 15l4.5-4.5M5 19h14"/></svg>"##;
const KEY: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="15" r="4"/><path d="M11 12l8-8M16 7l2 2M14 9l2 2"/></svg>"##;
const SHIELD: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linejoin="round"><path d="M12 3l7 3v5.5c0 4.3-3 7.6-7 9.5-4-1.9-7-5.2-7-9.5V6z"/></svg>"##;
const IMPORTED: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18M10 7.5v9m0-9h3c2.4 0 2.4 4 0 4h-3m0 0h3.6c2.6 0 2.6 4.5 0 4.5H10M11.5 6v1.5m0 9V18"/></svg>"##;
/// Spark's asterisk from `coincube-ui`'s `spark_badge.svg`, filled and fitted
/// to the 24-unit grid like the face mark.
const SPARK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path transform="translate(3 3.4) scale(0.2675)" fill="#fff" fill-rule="evenodd" d="M39.7159 25.248L40.8727 0.570312H26.4219L27.5787 25.2483L4.46555 16.5221L0 30.2656L23.8282 36.7915L8.38717 56.0763L20.0781 64.5703L33.6483 43.9245L47.2179 64.5695L58.9089 56.0755L43.4679 36.7909L67.2937 30.2657L62.8281 16.5221L39.7159 25.248Z"/></svg>"##;
const LOCK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M7 11h10v9H7zM9 11V8a3 3 0 0 1 6 0v3"/></svg>"##;
const PASSKEY: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M9 5a4 4 0 1 0 0 8a4 4 0 1 0 0-8M12 11.5l7 7m-3-3 2.5-2.5m-.5 5 2.5-2.5"/></svg>"##;
const CHEVRON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M7 10l5 5 5-5"/></svg>"##;
const TICK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>"##;
const ATTENTION: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round"><path d="M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18M12 7.5v5.5M12 16.2v.3"/></svg>"##;
const QUESTION: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round"><path d="M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18M9.6 9.6a2.4 2.4 0 1 1 3.4 2.2c-.7.3-1 .8-1 1.5v.4M12 16.3v.3"/></svg>"##;

#[derive(Debug, Clone, Copy)]
pub(super) enum Ink {
    Body,
    Muted,
    Accent,
    /// `--warn`: a backup that needs attention.
    Warn,
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
        Ink::Warn => shell.warn,
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
    top_bar::button_style(theme, status, pressed)
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

/// A network's full name (`CH.full`), for a Cube's face and its unlock.
pub(super) fn network_name(chain: ChainId) -> &'static str {
    match chain {
        ChainId::Bitcoin => "Bitcoin mainnet",
        ChainId::BitcoinBlake2b => "Bitcoin Blake2b mainnet",
        ChainId::Testnet => "Bitcoin Testnet",
        ChainId::Testnet4 => "Bitcoin Testnet4",
        ChainId::Signet => "Bitcoin Signet",
        ChainId::Regtest => "Bitcoin Regtest",
        ChainId::BitcoinBlake2bTestnet4 => "Bitcoin Blake2b Testnet4",
    }
}

/// A network's menu name (`CH.menu`), for the Network chip and menu.
pub(super) fn network_menu_name(chain: ChainId) -> &'static str {
    match chain {
        ChainId::Bitcoin => "Bitcoin mainnet",
        ChainId::BitcoinBlake2b => "Bitcoin Blake2b mainnet",
        ChainId::BitcoinBlake2bTestnet4 => "Blake2b Testnet4",
        other => other.label(),
    }
}

/// `.chip`: 32px, square-cornered; `.chip.on` while a network narrows the Cubes shown.
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
                radius: 8.0.into(),
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
    let eye = crate::app::view::nav::shell::balance_privacy_eye(
        launcher_catalog::privacy_id(),
        chrome?,
        focus,
        ViewMessage::ChangeDevicePreference,
    );
    Some(eye.muted(quiet).into())
}

/// One `.chip`: a 32px filter toggle, `.chip.on` while it applies.
fn filter_chip<'a>(
    id: Id,
    label: String,
    on: bool,
    quiet: bool,
    menu: bool,
    message: ViewMessage,
) -> Element<'a, ViewMessage> {
    let on = on && !quiet;
    let mut content = Row::new().spacing(6).align_y(Alignment::Center).push(words(
        label,
        13.0,
        if on { font::MEDIUM } else { font::REGULAR },
        body(quiet),
    ));
    if menu {
        content = content.push(glyph(CHEVRON, 14.0, body(quiet)));
    }
    Action::new(id, Container::new(content).center_y(Length::Fill), focus)
        .height(Length::Fixed(32.0))
        .padding([0.0, 12.0])
        .style(chip_style(on))
        .on_press(message)
        .into()
}

/// The chips that narrow the Launcher by network, as on the canvas: "All
/// networks" when more than one network is listed, "Live" and "Test" in
/// Developer mode, then the Network menu.
fn filter_chips<'a>(home: &Home, filter: NetworkFilter, quiet: bool) -> Row<'a, ViewMessage> {
    let pick = |filter| ViewMessage::Catalog(CatalogMessage::Filter(filter));
    let mut chips = Row::new().spacing(8).align_y(Alignment::Center);
    for (filter_kind, label, id) in group_chips(home) {
        chips = chips.push(filter_chip(
            launcher_catalog::filter_id(id),
            label.to_string(),
            filter == filter_kind,
            quiet,
            false,
            pick(filter_kind),
        ));
    }
    let (label, on) = match filter {
        NetworkFilter::Network(chain) => (format!("Network: {}", network_menu_name(chain)), true),
        _ => ("Network".to_string(), false),
    };
    chips.push(filter_chip(
        launcher_catalog::network_id(),
        label,
        on,
        quiet,
        true,
        ViewMessage::Catalog(CatalogMessage::NetworkMenu(!home.catalog.network_menu)),
    ))
}

/// The Network menu, placed under the Network chip: undrawn copies of the
/// All / Live / Test chips take their widths and their 8px gaps.
fn under_network_chip<'a>(home: &Home, menu: Element<'a, ViewMessage>) -> Row<'a, ViewMessage> {
    let mut row = Row::new().spacing(8);
    for (_, label, _) in group_chips(home) {
        row = row.push(
            Container::new(words(label, 13.0, font::REGULAR, Ink::Clear))
                .height(Length::Fixed(32.0))
                .padding([0.0, 12.0]),
        );
    }
    row.push(menu)
}

/// The All / Live / Test chips this Home shows.
fn group_chips(home: &Home) -> Vec<(NetworkFilter, &'static str, &'static str)> {
    let mut chips = Vec::new();
    if home.displayed_networks.len() > 1 {
        chips.push((NetworkFilter::All, "All networks", "all"));
    }
    if home.developer_mode() {
        chips.push((NetworkFilter::Live, "Live networks", "live"));
        chips.push((NetworkFilter::Test, "Test networks", "test"));
    }
    chips
}

/// `.linkbtn` "Clear filters": 13px ink with the accent underline.
fn clear_filters<'a>(id: Id) -> Element<'a, ViewMessage> {
    Action::new(
        id,
        iced::widget::Stack::new()
            .push(
                Column::new()
                    .push(words("Clear filters", 13.0, font::REGULAR, Ink::Body))
                    .push(Space::new().height(Length::Fixed(3.0))),
            )
            .push(
                Column::new()
                    .height(Length::Fill)
                    .push(Space::new().height(Length::Fill))
                    .push(
                        Container::new(Space::new())
                            .width(Length::Fill)
                            .height(Length::Fixed(1.0))
                            .style(|theme: &theme::Theme| container::Style {
                                background: Some(iced::Background::Color(
                                    theme.colors.shell.accent,
                                )),
                                ..Default::default()
                            }),
                    ),
            ),
        focus,
    )
    .padding([2.0, 4.0])
    .style(|_theme: &theme::Theme, _status| iced_button::Style::default())
    .on_press(ViewMessage::Catalog(CatalogMessage::Filter(
        NetworkFilter::All,
    )))
    .into()
}

/// `.vsep`: the 1px rule between the brand and the filters.
fn separator<'a, M: 'a>() -> Element<'a, M> {
    Container::new(Space::new())
        .width(Length::Fixed(1.0))
        .height(Length::Fixed(20.0))
        .style(|theme: &theme::Theme| container::Style {
            background: Some(iced::Background::Color(theme.colors.shell.line)),
            ..Default::default()
        })
        .into()
}

/// `.top` on the shared bar: brand, network chips and count; then balances,
/// Connect and App preferences. The theme lives in App preferences, as in the
/// prototype.
pub(super) fn top_bar<'a>(
    home: &'a Home,
    projection: &Projection<'_>,
    quiet: bool,
) -> Element<'a, ViewMessage> {
    let count = count_text(
        projection.locals.len(),
        projection.local_total,
        projection.connect_only(home.catalog.mode),
    );
    let preferences = top_bar::preferences(
        launcher_catalog::settings_id(),
        focus,
        ViewMessage::OpenAppPreferences,
    )
    .pressed(home.app_preferences_open)
    .muted(quiet);
    top_bar::top_bar(top_bar::brand_mark(quiet), preferences)
        .lead(separator())
        .lead(filter_chips(home, projection.filter, quiet))
        .lead(words(count, 13.0, font::REGULAR, Ink::Muted))
        .lead(
            (projection.filter != NetworkFilter::All)
                .then(|| clear_filters(launcher_catalog::clear_filters_id())),
        )
        .eye(privacy_eye(home.balance_privacy, quiet))
        .chip(connect_entry(home, quiet))
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
    let quota = text(quota_text(projection))
        .size(12)
        .style(|theme: &theme::Theme| text::Style {
            color: Some(theme.colors.shell.muted),
        })
        .wrapping(text::Wrapping::WordOrGlyph)
        .width(Length::Fill);
    let key = (projection.filtered_total > 0).then(|| key_button(home.catalog.key_open()));
    let target = dock_target(home, projection)
        .map(|caption| words(caption, 12.0, font::REGULAR, Ink::Muted));
    Container::new(
        Row::new()
            .spacing(8)
            .align_y(Alignment::Center)
            .height(Length::Fill)
            .push(create)
            .push(import)
            .push(target)
            .push(recover)
            .push(Space::new().width(Length::Fixed(16.0)))
            .push(quota)
            .push(activity_button(home))
            .push(key),
    )
    .height(Length::Fixed(BAR_HEIGHT))
    .padding([0.0, GUTTER])
    .width(Length::Fill)
    .style(rail)
    .into()
}

fn activity_id() -> Id {
    Id::new("tenshu.launcher.activity")
}

fn activity_option_id(source: NetworkActivitySource) -> Id {
    Id::new(match source {
        NetworkActivitySource::Automatic => "tenshu.launcher.activity.automatic",
        NetworkActivitySource::LocalNode => "tenshu.launcher.activity.local",
        NetworkActivitySource::Connect => "tenshu.launcher.activity.connect",
    })
}

/// The network activity's source selector: the shown block and where it came
/// from, or why nothing shows. A click opens the source menu above it.
fn activity_button<'a>(home: &Home) -> Option<Element<'a, ViewMessage>> {
    if !home.activity_wanted() {
        return None;
    }
    let chain = home.activity_chain();
    let light = matches!(
        home.theme_mode,
        coincube_ui::theme::palette::ThemeMode::Light
    );
    let (label, dot) = match home.catalog.activity.current(chain) {
        Some(Ok(snapshot)) => (
            format!(
                "Block {} · {}",
                launcher_activity::block_label(snapshot.tip),
                snapshot.via.label()
            ),
            Some(launcher_scene::edge(chain, light)),
        ),
        Some(Err(why)) => (why.label().to_string(), None),
        None => ("Reading network activity".to_string(), None),
    };
    let open = home.catalog.activity.menu;
    let dot = Container::new(Space::new())
        .width(Length::Fixed(8.0))
        .height(Length::Fixed(8.0))
        .style(move |theme: &theme::Theme| container::Style {
            background: dot.map(|c| {
                iced::Background::Color(Color::from_rgba8(
                    c[0],
                    c[1],
                    c[2],
                    f32::from(c[3]) / 255.0,
                ))
            }),
            border: Border {
                color: theme.colors.shell.line,
                width: if dot.is_some() { 0.0 } else { 1.0 },
                radius: 2.0.into(),
            },
            ..Default::default()
        });
    Some(
        Action::new(
            activity_id(),
            Container::new(
                Row::new()
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(dot)
                    .push(words(label, 13.0, font::REGULAR, Ink::Muted))
                    .push(glyph(CHEVRON, 14.0, Ink::Muted)),
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
        .on_press(ViewMessage::Catalog(CatalogMessage::Activity(
            ActivityMessage::Menu(!open),
        )))
        .into(),
    )
}

/// The activity source menu, above its dock button. It follows the Network
/// filter; picking a source saves the device preference.
pub(super) fn activity_layer<'a>(home: &Home) -> Option<Element<'a, ViewMessage>> {
    if !home.catalog.activity.menu || !home.activity_wanted() {
        return None;
    }
    let close = ViewMessage::Catalog(CatalogMessage::Activity(ActivityMessage::Menu(false)));
    let current = home.catalog.activity.source;
    let option = |source: NetworkActivitySource, label: &'static str| {
        let on = source == current;
        Action::new(
            activity_option_id(source),
            Container::new(
                Row::new()
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(
                        Container::new(on.then(|| glyph(TICK, 14.0, Ink::Body)))
                            .width(Length::Fixed(14.0))
                            .height(Length::Fixed(14.0)),
                    )
                    .push(words(
                        label,
                        13.0,
                        if on { font::SEMIBOLD } else { font::REGULAR },
                        Ink::Body,
                    )),
            )
            .center_y(Length::Fill),
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
        .on_press(ViewMessage::ChangeDevicePreference(
            PreferenceChange::NetworkActivitySource(source),
        ))
    };
    let rule = Container::new(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|theme: &theme::Theme| container::Style {
            background: Some(iced::Background::Color(theme.colors.shell.line)),
            ..Default::default()
        });
    let menu = Container::new(
        Column::new()
            .spacing(0)
            .push(
                Container::new(words(
                    format!(
                        "Network activity · {}",
                        network_menu_name(home.activity_chain())
                    ),
                    12.0,
                    font::REGULAR,
                    Ink::Muted,
                ))
                .padding([6.0, 10.0]),
            )
            .push(option(NetworkActivitySource::Automatic, "Automatic"))
            .push(option(NetworkActivitySource::LocalNode, "Local node"))
            .push(option(
                NetworkActivitySource::Connect,
                launcher_activity::public_source(home.activity_chain())
                    .map_or("COINCUBE | Connect", |(via, _)| via.label()),
            ))
            .push(
                Column::new()
                    .padding(Padding {
                        top: 6.0,
                        right: 6.0,
                        bottom: 4.0,
                        left: 6.0,
                    })
                    .spacing(8)
                    .push(rule)
                    .push(
                        text(
                            "Follows the Network filter. Automatic reads a local node that is \
                             already running, else COINCUBE | Connect, or mempool.space for the \
                             test networks Connect does not serve. The Launcher never starts a \
                             node.",
                        )
                        .size(12)
                        .style(|theme: &theme::Theme| text::Style {
                            color: Some(theme.colors.shell.muted),
                        })
                        .wrapping(text::Wrapping::WordOrGlyph),
                    )
                    .push(small_link(
                        Id::new("tenshu.launcher.activity.preferences"),
                        "Turn off in App preferences",
                        ViewMessage::OpenAppPreferences,
                    )),
            ),
    )
    .width(Length::Fixed(ACTIVITY_MENU_WIDTH))
    .padding(6)
    .style(|theme: &theme::Theme| container::Style {
        background: Some(iced::Background::Color(theme.colors.shell.panel)),
        border: Border {
            color: theme.colors.shell.line,
            width: 1.0,
            radius: 10.0.into(),
        },
        shadow: iced::Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
            offset: iced::Vector::new(0.0, 18.0),
            blur_radius: 40.0,
        },
        ..Default::default()
    });
    // `bottom: calc(100% + 8px)` of its 32px button, like the Key card.
    let button_top = (BAR_HEIGHT + KEY_BUTTON_HEIGHT) / 2.0 + 8.0;
    let placed = Container::new(iced::widget::opaque(menu))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::alignment::Horizontal::Right)
        .align_y(iced::alignment::Vertical::Bottom)
        .padding(Padding {
            top: 0.0,
            right: GUTTER + KEY_BUTTON_WIDTH + 8.0,
            bottom: button_top,
            left: 0.0,
        });
    Some(
        iced::widget::Stack::new()
            .width(Length::Fill)
            .height(Length::Fill)
            .push(mouse_area(Space::new().width(Length::Fill).height(Length::Fill)).on_press(close))
            .push(placed)
            .into(),
    )
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
        .on_enter(ViewMessage::Catalog(CatalogMessage::KeyHover(
            KeyPart::Button,
            true,
        )))
        .on_exit(ViewMessage::Catalog(CatalogMessage::KeyHover(
            KeyPart::Button,
            false,
        )))
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
                    glyph(SPARK, 16.0, Ink::Body),
                ],
                "Left: wallets",
            ))
            .push(legend_row(
                vec![
                    glyph(LOCK, 16.0, Ink::Body),
                    glyph(PASSKEY, 16.0, Ink::Body),
                ],
                "Right: PIN or passkey lock",
            ))
            .push(legend_row(
                vec![glyph(ATTENTION, 16.0, Ink::Warn)],
                "Backup not confirmed",
            ))
            .push(legend_row(
                vec![glyph(QUESTION, 16.0, Ink::Muted)],
                "Backup status unknown",
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

/// An undrawn copy of the bar's brand (`top_bar::brand_mark`), to place a
/// menu under a control after it. Iced's SVG tint drops alpha, so the cube's
/// place is a space.
fn brand_footprint<'a, M: 'a>() -> Element<'a, M> {
    Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(
            Space::new()
                .width(Length::Fixed(20.0))
                .height(Length::Fixed(20.0)),
        )
        .push(words("Tenshu", 15.0, font::SEMIBOLD, Ink::Clear))
        .into()
}

/// `.menu`: 272px under the chip, one `.mi` row per listed network and the
/// filter note. Only the shown network's Cubes are loaded, so only its row
/// carries a count. A click outside it closes it.
pub(super) fn network_layer<'a>(
    home: &Home,
    projection: &Projection<'_>,
) -> Option<Element<'a, ViewMessage>> {
    if !home.catalog.network_menu {
        return None;
    }
    let close = ViewMessage::Catalog(CatalogMessage::NetworkMenu(false));
    let mut rows = Column::new().spacing(0);
    for (chain, count) in &projection.network_counts {
        let chain = *chain;
        let on = projection.filter == NetworkFilter::Network(chain);
        let row = Row::new()
            .spacing(8)
            .align_y(Alignment::Center)
            .push(
                Container::new(on.then(|| glyph(TICK, 14.0, Ink::Body)))
                    .width(Length::Fixed(14.0))
                    .height(Length::Fixed(14.0)),
            )
            .push(words(
                network_menu_name(chain),
                13.0,
                if on { font::SEMIBOLD } else { font::REGULAR },
                Ink::Body,
            ))
            .push(Space::new().width(Length::Fill))
            .push(words(
                if projection.unread.contains(&chain) {
                    "Unavailable".to_string()
                } else {
                    count.to_string()
                },
                12.0,
                font::REGULAR,
                Ink::Muted,
            ));
        rows = rows.push(
            Action::new(
                network_option_id(chain),
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
            // Retry is possible while the filtered network is not yet current.
            .on_press(if on && chain == home.network {
                close.clone()
            } else {
                ViewMessage::Catalog(CatalogMessage::Filter(NetworkFilter::Network(chain)))
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
                .push(brand_footprint())
                .push(Space::new().width(Length::Fixed(1.0)))
                // Presses on the menu's own padding and note stay in it.
                .push(under_network_chip(home, iced::widget::opaque(menu))),
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
            .on_enter(ViewMessage::Catalog(CatalogMessage::KeyHover(
                KeyPart::Card,
                true,
            )))
            .on_exit(ViewMessage::Catalog(CatalogMessage::KeyHover(
                KeyPart::Card,
                false,
            ))),
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
    /// The filter chips' labels, the Network chip last.
    chips: Vec<String>,
    /// Whether "Clear filters" shows.
    filtered: bool,
    count: String,
    /// `Some(hidden)` while the balances eye is shown.
    eye: Option<bool>,
    connect: Vec<(String, Ink, f32, bool)>,
    links: Vec<&'static str>,
    /// The dock's "New Cubes: <network>" caption, when it shows.
    pub(super) target: Option<String>,
    /// The dock's per-network plan usage line.
    pub(super) quota: String,
    key: bool,
}

pub(super) fn snapshot(home: &Home, projection: &Projection<'_>) -> ChromeSnapshot {
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
    let mut links = Vec::new();
    if account.is_authenticated()
        && !home.network.is_blake2b()
        && feature_flags::RECOVER_VAULT_ENABLED
    {
        links.push("Recover a Vault");
    }
    ChromeSnapshot {
        chips: group_chips(home)
            .into_iter()
            .map(|(_, label, _)| label.to_string())
            .chain(std::iter::once(match projection.filter {
                NetworkFilter::Network(chain) => format!("Network: {}", network_menu_name(chain)),
                _ => "Network".to_string(),
            }))
            .collect(),
        filtered: projection.filter != NetworkFilter::All,
        count: count_text(
            projection.locals.len(),
            projection.local_total,
            projection.connect_only(home.catalog.mode),
        ),
        eye: home
            .balance_privacy
            .map(|chrome| chrome.privacy.masks_amounts()),
        connect,
        links,
        target: dock_target(home, projection),
        quota: quota_text(projection),
        key: projection.filtered_total > 0,
    }
}

/// Create and Import act on the current network: name it once more than one
/// network is listed, since the filter alone may not show which.
fn dock_target(home: &Home, projection: &Projection<'_>) -> Option<String> {
    (projection.network_counts.len() > 1)
        .then(|| format!("New Cubes: {}", network_name(home.network)))
}

/// P-Launcher's `countText`: this device's Cubes, then the ghosts beside
/// them, which are Connect's and never counted as this device's.
fn count_text(shown: usize, total: usize, in_connect: usize) -> String {
    let connect = if in_connect > 0 {
        format!(" · {in_connect} in Connect only")
    } else {
        String::new()
    };
    if total == 0 && in_connect > 0 {
        format!("No Cubes on this device{connect}")
    } else if shown == total {
        format!(
            "{total} {}{connect}",
            if total == 1 { "Cube" } else { "Cubes" }
        )
    } else {
        format!(
            "{shown} of {total} Cubes{connect} · {} hidden",
            total - shown
        )
    }
}

/// The plan's per-network limit and each network's use of it. Cubes that are
/// registered in Connect but not on this device are called out, because they
/// fill a slot without appearing in any list here.
fn quota_text(projection: &Projection<'_>) -> String {
    let limit = projection.cube_limit;
    let networks = projection
        .usage
        .iter()
        .map(|usage| {
            let name = network_name(usage.chain);
            let Some(total) = usage.total() else {
                // This device's share is unknown: say what Connect holds
                // rather than print a ratio that may be short.
                return if usage.remote > 0 {
                    format!(
                        "{name} {} in Connect, this device not counted",
                        usage.remote
                    )
                } else {
                    format!("{name} not counted")
                };
            };
            let mut entry = format!("{name} {total}/{limit}");
            if total >= limit {
                entry.push_str(" full");
            }
            if usage.remote > 0 {
                entry.push_str(&format!(" ({} only in Connect)", usage.remote));
            }
            entry
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let plan = format!("{} plan: {limit} per network · {networks}", projection.tier);
    if projection.at_limit() {
        format!(
            "{plan}. Upgrade, or delete a {} Cube, to add one.",
            network_name(projection.network),
        )
    } else {
        plan
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
    // `.quiet .chip.on` drops the fill and the weight, so every chip rests.
    let last = chrome.chips.len().saturating_sub(1);
    let mut chips = Row::new().spacing(8).align_y(Alignment::Center);
    for (index, label) in chrome.chips.iter().enumerate() {
        let mut content = Row::new().spacing(6).align_y(Alignment::Center).push(words(
            label.clone(),
            13.0,
            font::REGULAR,
            body(quiet),
        ));
        if index == last {
            content = content.push(glyph(CHEVRON, 14.0, body(quiet)));
        }
        chips = chips.push(resting(content, 32.0, 12.0, |theme| {
            chip_style(false)(theme, iced_button::Status::Active)
        }));
    }
    let eye = chrome.eye.map(|hidden| {
        if hidden {
            top_bar::resting_icon(EYE_SHUT, "Balances hidden")
        } else {
            top_bar::resting_icon(EYE_OPEN, "Balances shown")
        }
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
    top_bar::top_bar(
        top_bar::brand_mark(quiet),
        top_bar::resting_icon(SLIDERS, APP_PREFERENCES),
    )
    .lead(separator())
    .lead(chips)
    .lead(words(chrome.count.clone(), 13.0, font::REGULAR, Ink::Muted))
    .lead(chrome.filtered.then(|| {
        Container::new(words("Clear filters", 13.0, font::REGULAR, Ink::Muted)).padding([2.0, 4.0])
    }))
    .eye(eye)
    .chip(resting(connect, 32.0, 10.0, resting_bar))
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
        .push(action(PLUS, "Create Cube"))
        .push(action(IMPORT, "Import Bitcoin wallet"));
    if let Some(target) = &chrome.target {
        row = row.push(words(target.clone(), 12.0, font::REGULAR, Ink::Muted));
    }
    for link in &chrome.links {
        row = row.push(
            Container::new(words(*link, 13.0, font::MEDIUM, Ink::Accent)).padding([4.0, 6.0]),
        );
    }
    row = row.push(Space::new().width(Length::Fixed(16.0)));
    row = row.push(
        text(chrome.quota.clone())
            .size(12)
            .style(|theme: &theme::Theme| text::Style {
                color: Some(theme.colors.shell.muted),
            })
            .wrapping(text::Wrapping::WordOrGlyph)
            .width(Length::Fill),
    );
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

#[cfg(test)]
mod tests {
    use super::count_text;

    #[test]
    fn the_count_names_this_devices_cubes_and_the_ghosts_apart() {
        assert_eq!(count_text(2, 2, 0), "2 Cubes");
        assert_eq!(count_text(1, 1, 2), "1 Cube · 2 in Connect only");
        assert_eq!(
            count_text(0, 0, 2),
            "No Cubes on this device · 2 in Connect only"
        );
        assert_eq!(
            count_text(1, 3, 1),
            "1 of 3 Cubes · 1 in Connect only · 2 hidden"
        );
    }
}

/// The Launcher's bars on the shared top bar. The capture test writes frames
/// beside the Cube's (`app::view::nav::shell::capture_tests`), opt-in only:
/// `TOP_BAR_CAPTURE_DIR=<dir> cargo test -p coincube-gui --lib top_bar_capture
/// -- --ignored`. Every Cube is a synthetic fixture.
#[cfg(test)]
mod top_bar_tests {
    use std::path::PathBuf;

    use iced::{
        advanced::{
            layout,
            renderer::Headless,
            widget::{operation::Focusable, Id, Operation, Tree},
            Layout,
        },
        widget::rule,
        Rectangle, Size,
    };

    use super::super::{launcher_catalog, Home, State};
    use crate::app::view::nav::shell::capture_tests::{directory, renderer, save, themes, WINDOWS};
    use crate::{app::settings::CubeSettings, chain::ChainId, dir::CoincubeDirectory};

    fn home(light: bool) -> Home {
        let mut home = Home::new(
            CoincubeDirectory::new(PathBuf::new()),
            Some(coincube_core::miniscript::bitcoin::Network::Bitcoin),
        )
        .0;
        home.network = ChainId::Bitcoin;
        let cubes = ["Everyday", "Archive", "Household 2024", "Travel fund"]
            .iter()
            .enumerate()
            .map(|(i, name)| {
                CubeSettings::new_with_raw_id(format!("cube-{i}"), (*name).into(), ChainId::Bitcoin)
            })
            .collect();
        home.state = State::Cubes {
            cubes,
            create_cube: false,
            source: ChainId::Bitcoin,
        };
        home.advisory_notice = None;
        home.catalog.mode = launcher_catalog::LauncherMode::Cubes;
        home.balance_privacy = Some(crate::app::state::preferences::BalancePrivacyChrome {
            privacy: crate::app::state::preferences::BalancePrivacy::Shown,
            busy: false,
            interactive: true,
        });
        home.theme_mode = if light {
            coincube_ui::theme::palette::ThemeMode::Light
        } else {
            coincube_ui::theme::palette::ThemeMode::Dark
        };
        home
    }

    #[derive(Default)]
    struct Probe {
        focus: Vec<(Id, Rectangle)>,
        text: Vec<(String, Rectangle)>,
    }

    impl Operation for Probe {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn focusable(&mut self, id: Option<&Id>, bounds: Rectangle, _: &mut dyn Focusable) {
            if let Some(id) = id {
                self.focus.push((id.clone(), bounds));
            }
        }
        fn text(&mut self, _: Option<&Id>, bounds: Rectangle, value: &str) {
            self.text.push((value.to_owned(), bounds));
        }
    }

    fn probe<M>(mut element: coincube_ui::widget::Element<'_, M>) -> (Rectangle, Probe) {
        for bytes in coincube_ui::font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            coincube_ui::font::REGULAR,
            iced::Pixels(14.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(1024.0, 400.0)),
        );
        let mut probe = Probe::default();
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut probe);
        (node.bounds(), probe)
    }

    /// App preferences is icon-only on both Launcher bars, and its accessible
    /// label stays "App preferences". The right group reads eye, Connect,
    /// App preferences, packed to the 24px gutter.
    #[test]
    fn launcher_bars_take_the_shared_layout_and_label_the_preferences_icon() {
        let home = home(false);
        let projection = launcher_catalog::project(&home);
        let (bar, live) = probe(super::top_bar(&home, &projection, false));
        assert_eq!(bar.height, super::BAR_HEIGHT);
        let at = |id: &Id| {
            live.focus
                .iter()
                .find(|(target, _)| target == id)
                .map(|(_, bounds)| *bounds)
                .unwrap()
        };
        let settings = at(&launcher_catalog::settings_id());
        assert_eq!((settings.width, settings.height), (32.0, 32.0));
        assert_eq!(settings.x + settings.width, 1024.0 - super::GUTTER);
        let labels: Vec<_> = live
            .text
            .iter()
            .filter(|(value, _)| value == "App preferences")
            .collect();
        assert_eq!(labels.len(), 1, "{:?}", live.text);
        assert_eq!(labels[0].1, settings);
        let eye = at(&launcher_catalog::privacy_id());
        // A session still being checked leaves the Connect entry inert.
        let connect = live
            .text
            .iter()
            .find(|(value, _)| value == "Connect")
            .unwrap()
            .1;
        assert!(eye.x < connect.x && connect.x < settings.x);
        assert!(live.text.iter().any(|(value, _)| value == "Hide balances"));
        let brand = live
            .text
            .iter()
            .find(|(value, _)| value == "Tenshu")
            .unwrap()
            .1;
        // The 20px cube and its 8px gap sit at the gutter.
        assert_eq!(brand.x, super::GUTTER + 28.0);

        let chrome = super::snapshot(&home, &projection);
        let (bar, quiet) = probe(super::quiet_top_bar::<()>(&chrome));
        assert_eq!(bar.height, super::BAR_HEIGHT);
        assert!(quiet.focus.is_empty());
        let label = quiet
            .text
            .iter()
            .find(|(value, _)| value == "App preferences")
            .unwrap()
            .1;
        assert_eq!((label.width, label.height), (32.0, 32.0));
        assert_eq!(label.x + label.width, 1024.0 - super::GUTTER);
        assert!(quiet
            .text
            .iter()
            .any(|(value, _)| value == "Balances shown"));
    }

    #[test]
    #[ignore = "writes PNG frames; needs a GPU adapter"]
    fn top_bar_capture_launcher() {
        let Some(directory) = directory() else {
            return;
        };
        let mut renderer = renderer();
        for (appearance, palette) in themes() {
            let home = home(appearance == "light");
            for (width, height) in WINDOWS {
                save(
                    &mut renderer,
                    launcher_catalog::launcher_view(&home),
                    Size::new(width, height),
                    &palette,
                    None,
                    directory.join(format!("launcher-{appearance}-{width}x{height}.png")),
                );
            }
            for (id, name) in [
                (launcher_catalog::privacy_id(), "eye"),
                (launcher_catalog::sign_in_id(), "connect"),
                (launcher_catalog::settings_id(), "preferences"),
            ] {
                save(
                    &mut renderer,
                    launcher_catalog::launcher_view(&home),
                    Size::new(1024.0, 768.0),
                    &palette,
                    Some(id),
                    directory.join(format!("launcher-{appearance}-focus-{name}-1024x768.png")),
                );
            }
            // The live bar over the quiet one the unlock face stands on.
            let projection = launcher_catalog::project(&home);
            let chrome = super::snapshot(&home, &projection);
            let bars = iced::widget::column![
                super::top_bar(&home, &projection, false),
                rule::horizontal(1),
                super::quiet_top_bar(&chrome),
                rule::horizontal(1),
            ];
            save(
                &mut renderer,
                bars.into(),
                Size::new(1024.0, 2.0 * (super::BAR_HEIGHT + 1.0)),
                &palette,
                None,
                directory.join(format!(
                    "launcher-{appearance}-live-and-quiet-bars-1024.png"
                )),
            );
        }
    }
}
