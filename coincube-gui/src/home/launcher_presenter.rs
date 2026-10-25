//! v113 Launcher List and Opening presentation. The catalog, unlock and
//! loader remain the authorities for rows, actions and startup state.

use iced::{
    advanced::widget::Id,
    widget::{button as iced_button, container, responsive, rule, text::Wrapping, Space, Stack},
    Alignment, Length, Size,
};

use coincube_ui::{
    component::{
        quote_display::Quote,
        tenshu::{self, ActionButton, ActionKind},
    },
    icon, theme,
    widget::{action::Action, text_input, Column, Container, Element, Row, TextInput},
};

use super::{
    launcher_catalog::{self, CatalogMessage, LauncherMode, Projection},
    launcher_coin, launcher_scene, CubeCatalogKey, DeleteCubeMessage, Home, ViewMessage,
};
use crate::{
    chain::{ChainId, ChainIdExt},
    loader,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LauncherScene {
    pub query: String,
    /// These counts are unfiltered; search never changes quota facts.
    pub local_total: usize,
    pub remote_total: usize,
    /// Selected identity, not a row number that could move on refresh.
    pub selected: Option<CubeCatalogKey>,
}

/// Project only catalog facts that the new scene may reuse. The production
/// catalog still reconciles keys against current state and owns every
/// launch, restore and delete intent.
pub(super) fn project_catalog(catalog: &Projection<'_>) -> LauncherScene {
    LauncherScene {
        query: catalog.query.clone(),
        local_total: catalog.local_total,
        remote_total: catalog.remote_total,
        selected: catalog
            .selected
            .and_then(|index| catalog.locals.get(index).map(|row| row.key.clone())),
    }
}

const DETAILS_WIDTH: f32 = 440.0;
const PANEL_GAP: f32 = 24.0;
const LIST_MIN_WIDTH: f32 = 560.0;
const TABLE_MIN_VIEW_WIDTH: f32 = 1100.0;
const TABLE_MIN_LIST_WIDTH: f32 = 840.0;
const AVATAR_WIDTH: f32 = 44.0;
const NETWORK_WIDTH: f32 = 145.0;
const WALLETS_WIDTH: f32 = 160.0;
const BACKUP_WIDTH: f32 = 145.0;
const ACTION_WIDTH: f32 = 72.0;
const CELL_GAP: f32 = 14.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalletFaceMark {
    Vault,
    ImportedBitcoin,
    None,
}

fn wallet_face_mark(cube: &crate::app::settings::CubeSettings) -> WalletFaceMark {
    if cube.vault_wallet_id.is_some() {
        WalletFaceMark::Vault
    } else if cube.has_imported_bitcoin() {
        // An import operation may exist before its wallet is installed.
        WalletFaceMark::ImportedBitcoin
    } else {
        WalletFaceMark::None
    }
}

// A fixed-camera Cube with chain-specific edge and mark. User-supplied names
// never enter the SVG; they remain selectable text beneath each Cube.
fn cube_glyph_with_opacity(
    cube: &crate::app::settings::CubeSettings,
    light: bool,
    opacity: f32,
) -> iced::widget::Svg<'static, theme::Theme> {
    let scale = 1.0 - 0.08 * ((1.0 - opacity) / 0.6).clamp(0.0, 1.0);
    let shift_x = 90.0 * (1.0 - scale);
    let shift_y = 84.5 * (1.0 - scale);
    let source = cube_glyph_source(cube, light)
        .replace("<svg ", &format!("<svg opacity=\"{opacity:.3}\" "))
        .replacen(
            "<ellipse",
            &format!(
                "<g transform=\"translate({shift_x:.2} {shift_y:.2}) scale({scale:.3})\"><ellipse"
            ),
            1,
        )
        .replace("</svg>", "</g></svg>");
    iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(source.into_bytes()))
}

pub(super) fn cube_glyph_source(cube: &crate::app::settings::CubeSettings, light: bool) -> String {
    let chain = cube.network;
    let edge = if chain.is_blake2b() {
        "#d4a62a"
    } else {
        "#f3821d"
    };
    let mark = launcher_scene::rest_glyph_coin(chain, light);
    let top_points = launcher_scene::polygon_points(launcher_scene::rest_glyph_face(4));
    let left_points = launcher_scene::polygon_points(launcher_scene::rest_glyph_face(2));
    let right_points = launcher_scene::polygon_points(launcher_scene::rest_glyph_face(0));
    let dash = if matches!(chain, ChainId::Bitcoin | ChainId::BitcoinBlake2b) {
        "none"
    } else {
        "5 3"
    };
    let (top, left, right, marks, shadow) = if light {
        ("#e6e0d5", "#d6cfc2", "#c9c1b2", "#30302d", "#4d463b")
    } else {
        ("#41423e", "#2e2f2c", "#262724", "#ecebe6", "#080908")
    };
    // A face mark is shown only for a fact persisted on this Cube. Spark and
    // Liquid inventories are unknown before unlock, so neither gets a mark.
    let wallet_mark = match wallet_face_mark(cube) {
        WalletFaceMark::Vault => {
            r##"<path d="M58 73 67 77V87c0 7-5 12-9 14-4-2-9-7-9-14V77Z" fill="none" stroke="{marks}" stroke-width="2" stroke-linejoin="round"/>"##
        }
        WalletFaceMark::ImportedBitcoin => {
            r##"<circle cx="58" cy="85" r="12" fill="none" stroke="{marks}" stroke-width="2"/><path d="M58 78v14m-4-11h7c4 0 4 4 0 4h-7m0 0h8c4 0 4 4 0 4h-8" fill="none" stroke="{marks}" stroke-width="2"/>"##
        }
        WalletFaceMark::None => "",
    };
    let backup_mark = if cube.backed_up || cube.has_recovery_kit() {
        r##"<circle cx="137" cy="64" r="9" fill="none" stroke="{marks}" stroke-width="1.5"/><path d="m133 64 3 3 5-6" fill="none" stroke="{marks}" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>"##
    } else {
        ""
    };
    let wallet_mark = wallet_mark.replace("{marks}", marks);
    let backup_mark = backup_mark.replace("{marks}", marks);
    // The right-face mark distinguishes the passkey route from the ordinary
    // gated route. It does not establish PIN presence or a retained session.
    let unlock_mark = if cube.is_passkey_cube() {
        r##"<circle cx="117" cy="82" r="6" fill="none" stroke="{marks}" stroke-width="2"/><path d="m122 86 12 12m-5-5 4-4m-1 7 4-4" fill="none" stroke="{marks}" stroke-width="2" stroke-linecap="round"/>"##
    } else {
        r##"<path d="M116 83v-6a7 7 0 0 1 14 0v6m-17 0h20v18h-20z" fill="none" stroke="{marks}" stroke-width="2" stroke-linejoin="round"/><circle cx="123" cy="91" r="1.5" fill="{marks}"/>"##
    }
    .replace("{marks}", marks);
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="27 8 126 153"><ellipse cx="90" cy="155" rx="60" ry="10" fill="{shadow}" opacity=".35"/><polygon points="{top_points}" fill="{top}" stroke="{edge}" stroke-width="1.25" stroke-dasharray="{dash}"/><polygon points="{left_points}" fill="{left}" stroke="{edge}" stroke-width="1.25" stroke-dasharray="{dash}"/><polygon points="{right_points}" fill="{right}" stroke="{edge}" stroke-width="1.25" stroke-dasharray="{dash}"/><defs><clipPath id="launcher-top"><polygon points="{top_points}"/></clipPath></defs><g clip-path="url(#launcher-top)" color="{marks}">{mark}</g>{wallet_mark}{unlock_mark}{backup_mark}</svg>"##,
    )
}

fn switcher_label(name: &str) -> String {
    const MAX_CHARS: usize = 10;
    if name.chars().count() <= MAX_CHARS {
        name.to_owned()
    } else {
        format!("{}…", name.chars().take(MAX_CHARS - 1).collect::<String>())
    }
}

// The generated avatar is not available in this pre-unlock catalog. Keep a
// stable, readable initial alongside the name in both the floor and details.
fn catalog_avatar(name: &str) -> Container<'static, ViewMessage> {
    let initial = name
        .chars()
        .find(|ch| ch.is_alphanumeric())
        .map(|ch| ch.to_uppercase().collect::<String>())
        .unwrap_or_else(|| "?".into());
    Container::new(tenshu::label(initial))
        .center_x(Length::Fixed(34.0))
        .center_y(Length::Fixed(34.0))
        .style(|theme: &theme::Theme| container::Style {
            background: Some(iced::Background::Color(theme.colors.shell.panel)),
            border: iced::Border {
                color: theme.colors.shell.line,
                width: 1.0,
                radius: 17.0.into(),
            },
            text_color: Some(theme.colors.shell.ink),
            ..Default::default()
        })
}

/// Standalone unlock screens use the saved appearance until the tab's live
/// resolved theme is passed by the GUI shell.
pub(crate) fn saved_unlock_theme(
    datadir_root: &std::path::Path,
) -> coincube_ui::theme::palette::ThemeMode {
    use crate::app::settings::global::GlobalSettings;
    let root = crate::dir::CoincubeDirectory::new(datadir_root.to_path_buf());
    GlobalSettings::load_theme_mode(&GlobalSettings::path(&root))
}
fn floor_plane(light: bool) -> iced::widget::Svg<'static, theme::Theme> {
    let fill = if light { "#efebe4" } else { "#1d1f1d" };
    let line = if light { "#ddd7cc" } else { "#292b29" };
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 960 420" preserveAspectRatio="none"><path d="M140 0H820L960 420H0Z" fill="{fill}" stroke="{line}" stroke-width="1"/></svg>"##,
    );
    iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(svg.into_bytes()))
}

fn focus(id: Id) -> ViewMessage {
    ViewMessage::Catalog(CatalogMessage::Focus(id))
}

fn search_id() -> text_input::Id {
    text_input::Id::new("tenshu.launcher.catalog:control:search")
}

fn clear_id() -> Id {
    Id::from("tenshu.launcher.catalog:control:clear-query")
}

fn clear_empty_id() -> Id {
    Id::from("tenshu.launcher.catalog:control:clear-query-empty")
}

fn selected_style(
    selected: bool,
) -> impl Fn(&theme::Theme, iced_button::Status) -> iced_button::Style {
    move |theme, status| {
        let shell = theme.colors.shell;
        iced_button::Style {
            background: Some(iced::Background::Color(if selected {
                shell.selected
            } else if matches!(
                status,
                iced_button::Status::Hovered | iced_button::Status::Pressed
            ) {
                shell.hover
            } else {
                iced::Color::TRANSPARENT
            })),
            text_color: shell.ink,
            border: iced::Border {
                radius: 6.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

fn floor_style(
    selected: bool,
) -> impl Fn(&theme::Theme, iced_button::Status) -> iced_button::Style {
    move |theme, status| {
        let shell = theme.colors.shell;
        iced_button::Style {
            background: Some(iced::Background::Color(
                if matches!(
                    status,
                    iced_button::Status::Hovered | iced_button::Status::Pressed
                ) {
                    shell.hover
                } else {
                    iced::Color::TRANSPARENT
                },
            )),
            text_color: shell.ink,
            border: iced::Border {
                color: if selected {
                    shell.accent
                } else {
                    iced::Color::TRANSPARENT
                },
                width: if selected { 2.0 } else { 0.0 },
                radius: 8.0.into(),
            },
            ..Default::default()
        }
    }
}

pub(super) fn local_wallets(cube: &crate::app::settings::CubeSettings) -> &'static str {
    use crate::app::settings::BitcoinImportStage;
    if let Some(import) = &cube.bitcoin_import {
        return match &import.stage {
            BitcoinImportStage::PendingSource => "Bitcoin import pending · others unknown",
            BitcoinImportStage::Installing { .. } => "Bitcoin import installing · others unknown",
            BitcoinImportStage::Syncing { .. } => "Bitcoin import syncing · others unknown",
            BitcoinImportStage::Ready { .. } => "Bitcoin imported · others unknown",
        };
    }
    if cube.vault_wallet_id.is_some() {
        "Vault · others unknown"
    } else {
        "Status unknown"
    }
}

pub(super) fn backup(cube: &crate::app::settings::CubeSettings) -> (&'static str, tenshu::Tone) {
    match (cube.backed_up, cube.has_recovery_kit()) {
        (true, true) => ("Phrase + Kit recorded", tenshu::Tone::Good),
        (true, false) => ("Phrase recorded", tenshu::Tone::Good),
        (false, true) => ("Kit recorded (cached)", tenshu::Tone::Good),
        (false, false) => ("Status unknown", tenshu::Tone::Muted),
    }
}

pub(super) fn unlock_prompt(cube: &crate::app::settings::CubeSettings) -> &'static str {
    if cube.is_passkey_cube() {
        "Passkey prompt"
    } else {
        "PIN prompt"
    }
}

fn text_tone(tone: tenshu::Tone) -> impl Fn(&theme::Theme) -> iced::widget::text::Style {
    move |theme| theme::tenshu::text::tone(theme, tone)
}

fn network_coin(
    chain: ChainId,
    size: f32,
    light: bool,
) -> iced::widget::Svg<'static, theme::Theme> {
    let border = if size == 18.0 { 1.0 } else { 1.5 };
    iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(
        launcher_coin::widget_source(chain, size, border, light).into_bytes(),
    ))
    .width(Length::Fixed(size))
    .height(Length::Fixed(size))
}

fn network_label<'a>(chain: ChainId, size: f32, light: bool) -> Element<'a, ViewMessage> {
    Row::new()
        .spacing(6)
        .align_y(Alignment::Center)
        .push(network_coin(chain, size, light))
        .push(tenshu::caption(chain.label()))
        .into()
}

fn local_facts<'a>(
    row: &launcher_catalog::LocalRow<'_>,
    table: bool,
    light: bool,
) -> Element<'a, ViewMessage> {
    let (backup_text, backup_tone) = backup(row.cube);
    let name =
        Column::new()
            .spacing(2)
            .width(Length::Fill)
            .push(
                tenshu::label(row.name.clone())
                    .width(Length::Fill)
                    .wrapping(Wrapping::WordOrGlyph),
            )
            .push(tenshu::caption("Lock state unknown"))
            .push(row.sync_warning.as_ref().map(|warning| {
                tenshu::caption(warning.clone()).style(text_tone(tenshu::Tone::Warn))
            }));
    if table {
        Row::new()
            .spacing(CELL_GAP)
            .align_y(Alignment::Center)
            .push(Container::new(icon::cube_icon()).width(Length::Fixed(AVATAR_WIDTH)))
            .push(name)
            .push(
                Container::new(network_label(row.key.chain, 16.0, light))
                    .width(Length::Fixed(NETWORK_WIDTH)),
            )
            .push(
                tenshu::caption(local_wallets(row.cube))
                    .width(Length::Fixed(WALLETS_WIDTH))
                    .wrapping(Wrapping::WordOrGlyph),
            )
            .push(
                tenshu::body(backup_text)
                    .style(text_tone(backup_tone))
                    .width(Length::Fixed(BACKUP_WIDTH))
                    .wrapping(Wrapping::WordOrGlyph),
            )
            .into()
    } else {
        Column::new()
            .spacing(6)
            .width(Length::Fill)
            .push(
                Row::new()
                    .spacing(12)
                    .align_y(Alignment::Center)
                    .push(icon::cube_icon())
                    .push(name),
            )
            .push(network_label(row.key.chain, 16.0, light))
            .push(
                tenshu::caption(format!(
                    "Wallets: {} · Backup: {}",
                    local_wallets(row.cube),
                    backup_text
                ))
                .wrapping(Wrapping::WordOrGlyph),
            )
            .into()
    }
}

fn local_row<'a>(
    row: &launcher_catalog::LocalRow<'_>,
    selected: bool,
    table: bool,
    light: bool,
) -> Element<'a, ViewMessage> {
    let select = Action::new(
        launcher_catalog::select_id(&row.key),
        local_facts(row, table, light),
        focus,
    )
    .width(Length::Fill)
    .padding([12, 12])
    .style(selected_style(selected))
    .on_press_maybe(
        row.actionable
            .then(|| ViewMessage::Catalog(CatalogMessage::Select(row.key.clone()))),
    );
    let open: Element<'a, ViewMessage> = ActionButton::new(
        launcher_catalog::open_id(&row.key),
        "Open",
        ActionKind::Quiet,
        focus,
    )
    .compact()
    .on_press_maybe(
        row.actionable
            .then(|| ViewMessage::RunCatalog(row.key.clone())),
    )
    .into();
    Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(select)
        .push(Container::new(open).width(if table {
            Length::Fixed(ACTION_WIDTH)
        } else {
            Length::Shrink
        }))
        .into()
}

fn detail_line<'a>(
    label: &'static str,
    value: Element<'a, ViewMessage>,
) -> Element<'a, ViewMessage> {
    Row::new()
        .spacing(16)
        .width(Length::Fill)
        .push(tenshu::caption(label).width(Length::Fixed(72.0)))
        .push(value)
        .into()
}

fn details<'a>(row: &launcher_catalog::LocalRow<'_>, light: bool) -> Element<'a, ViewMessage> {
    let (backup_text, backup_tone) = backup(row.cube);
    let mut content = Column::new()
        .spacing(14)
        .push(
            Row::new()
                .spacing(12)
                .align_y(Alignment::Center)
                .push(catalog_avatar(&row.name))
                .push(
                    tenshu::section(row.name.clone())
                        .width(Length::Fill)
                        .wrapping(Wrapping::WordOrGlyph),
                )
                .push(
                    ActionButton::new(
                        launcher_catalog::close_details_id(),
                        "Back",
                        ActionKind::Quiet,
                        focus,
                    )
                    .compact()
                    .on_press(ViewMessage::Catalog(CatalogMessage::ClearSelection)),
                ),
        )
        .push(detail_line(
            "Network",
            network_label(row.key.chain, 18.0, light),
        ))
        .push(detail_line("Lock", tenshu::body("State unknown").into()))
        .push(detail_line(
            "Backup",
            tenshu::body(backup_text)
                .style(text_tone(backup_tone))
                .into(),
        ))
        .push(detail_line(
            "Vault",
            tenshu::body(row.vault.clone())
                .wrapping(Wrapping::WordOrGlyph)
                .into(),
        ))
        .push(detail_line(
            "Wallets",
            tenshu::body(local_wallets(row.cube)).into(),
        ))
        .push(detail_line(
            "Connect",
            tenshu::body(row.connect.1.clone())
                .wrapping(Wrapping::WordOrGlyph)
                .into(),
        ));
    content = content.push(detail_line(
        "Next step",
        tenshu::body(unlock_prompt(row.cube)).into(),
    ));
    content = content.push(tenshu::caption(
        "The next screen checks access before this Cube opens.",
    ));
    content = content.push(
        Row::new()
            .spacing(8)
            .push(
                ActionButton::new(
                    launcher_catalog::details_open_id(&row.key),
                    "Open",
                    ActionKind::Primary,
                    focus,
                )
                .compact()
                .on_press(ViewMessage::RunCatalog(row.key.clone())),
            )
            .push(
                ActionButton::new(
                    launcher_catalog::rename_id(&row.key),
                    "Rename",
                    ActionKind::Quiet,
                    focus,
                )
                .compact()
                .on_press(ViewMessage::RenameCatalog(row.key.clone())),
            )
            .push((!row.cube.network.is_blake2b()).then(|| {
                ActionButton::new(
                    launcher_catalog::delete_id(&row.key),
                    "Delete…",
                    ActionKind::Destructive,
                    focus,
                )
                .compact()
                .on_press(ViewMessage::DeleteCatalog(row.key.clone()))
            })),
    );
    tenshu::card(content).max_width(DETAILS_WIDTH).into()
}

/// The floor face has a fixed 420px rectangle. Keep its content deliberately
/// shorter than the List details card so actions stay on the visible face.
fn floor_details<'a>(
    row: &launcher_catalog::LocalRow<'_>,
    light: bool,
) -> Element<'a, ViewMessage> {
    let (backup_text, backup_tone) = backup(row.cube);
    Container::new(
        Column::new()
            .spacing(12)
            .push(
                Row::new()
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .push(catalog_avatar(&row.name))
                    .push(
                        tenshu::section(row.name.clone())
                            .width(Length::Fill)
                            .wrapping(Wrapping::WordOrGlyph),
                    ),
            )
            .push(
                Row::new()
                    .spacing(6)
                    .align_y(Alignment::Center)
                    .push(tenshu::caption("Network ·"))
                    .push(network_label(row.key.chain, 18.0, light)),
            )
            .push(rule::horizontal(1))
            .push(detail_line("Lock", tenshu::body("State unknown").into()))
            .push(detail_line(
                "Backup",
                tenshu::body(backup_text)
                    .style(text_tone(backup_tone))
                    .into(),
            ))
            .push(detail_line(
                "Connect",
                tenshu::body(row.connect.1.clone())
                    .wrapping(Wrapping::WordOrGlyph)
                    .into(),
            ))
            .push(detail_line(
                "Wallets",
                tenshu::body(local_wallets(row.cube)).into(),
            ))
            .push(Space::new().height(Length::Fill))
            .push(
                Row::new()
                    .spacing(12)
                    .align_y(Alignment::Center)
                    .push(
                        ActionButton::new(
                            launcher_catalog::details_open_id(&row.key),
                            "Open",
                            ActionKind::Primary,
                            focus,
                        )
                        .compact()
                        .on_press(ViewMessage::RunFloorCatalog(row.key.clone())),
                    )
                    .push(
                        ActionButton::new(
                            launcher_catalog::close_details_id(),
                            "Back",
                            ActionKind::Quiet,
                            focus,
                        )
                        .compact()
                        .on_press(ViewMessage::Catalog(CatalogMessage::ClearSelection)),
                    )
                    .push((!row.cube.network.is_blake2b()).then(|| {
                        ActionButton::new(
                            launcher_catalog::delete_id(&row.key),
                            "Delete…",
                            ActionKind::Destructive,
                            focus,
                        )
                        .compact()
                        .on_press(ViewMessage::DeleteCatalog(row.key.clone()))
                    })),
            )
            .push(tenshu::caption(format!(
                "Next step · {}",
                unlock_prompt(row.cube)
            ))),
    )
    .width(Length::Fixed(420.0))
    .height(Length::Fixed(420.0))
    .padding(28)
    .into()
}

fn remote_row<'a>(cube: &super::RemoteCube) -> Element<'a, ViewMessage> {
    let restore = launcher_catalog::remote_restore(cube);
    Row::new()
        .spacing(12)
        .align_y(Alignment::Center)
        .push(icon::cloud_arrow_down_icon())
        .push(
            Column::new()
                .spacing(4)
                .width(Length::Fill)
                .push(
                    tenshu::label(super::catalog_display_name(&cube.name))
                        .wrapping(Wrapping::WordOrGlyph),
                )
                .push(tenshu::caption(restore.status).wrapping(Wrapping::WordOrGlyph)),
        )
        .push(restore.action.map(|(message, label)| {
            ActionButton::new(
                launcher_catalog::restore_id(&cube.uuid),
                label,
                ActionKind::Secondary,
                focus,
            )
            .compact()
            .on_press(message)
        }))
        .push(
            ActionButton::new(
                launcher_catalog::remote_delete_id(&cube.uuid),
                "Delete…",
                ActionKind::Destructive,
                focus,
            )
            .compact()
            .on_press(ViewMessage::DeleteCube(DeleteCubeMessage::ShowRemoteModal(
                cube.uuid.clone(),
            ))),
        )
        .into()
}

fn section_heading<'a>(label: &'static str, count: usize) -> Element<'a, ViewMessage> {
    Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(tenshu::label(label))
        .push(tenshu::caption(count.to_string()))
        .into()
}

fn table_header<'a>() -> Element<'a, ViewMessage> {
    Row::new()
        .spacing(CELL_GAP)
        .align_y(Alignment::Center)
        .push(Space::new().width(Length::Fixed(AVATAR_WIDTH)))
        .push(tenshu::caption("Cube").width(Length::Fill))
        .push(tenshu::caption("Network").width(Length::Fixed(NETWORK_WIDTH)))
        .push(tenshu::caption("Wallets").width(Length::Fixed(WALLETS_WIDTH)))
        .push(tenshu::caption("Backup").width(Length::Fixed(BACKUP_WIDTH)))
        .push(Space::new().width(Length::Fixed(ACTION_WIDTH)))
        .into()
}

fn list_layout<'a>(
    projection: &Projection<'_>,
    scene: &LauncherScene,
    width: f32,
    light: bool,
) -> Element<'a, ViewMessage> {
    let list_width =
        if scene.selected.is_some() && width >= LIST_MIN_WIDTH + DETAILS_WIDTH + PANEL_GAP {
            width - DETAILS_WIDTH - PANEL_GAP
        } else {
            width
        };
    let table = width >= TABLE_MIN_VIEW_WIDTH && list_width >= TABLE_MIN_LIST_WIDTH;
    let search = Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(
            TextInput::new("Search Cubes by name", &scene.query)
                .id(search_id())
                .on_input(|query| ViewMessage::Catalog(CatalogMessage::Query(query)))
                .padding(10)
                .width(Length::Fill),
        )
        .push((!scene.query.is_empty()).then(|| {
            ActionButton::new(clear_id(), "Clear", ActionKind::Quiet, focus)
                .compact()
                .on_press(ViewMessage::Catalog(CatalogMessage::ClearQuery))
        }));
    let mut list = Column::new().spacing(8).width(Length::Fill).push(search);
    if projection.no_matches() {
        list = list.push(tenshu::card(
            Column::new()
                .spacing(8)
                .push(tenshu::body(format!(
                    "No Cubes match “{}”.",
                    scene.query.trim()
                )))
                .push(
                    ActionButton::new(clear_empty_id(), "Clear search", ActionKind::Quiet, focus)
                        .compact()
                        .on_press(ViewMessage::Catalog(CatalogMessage::ClearQuery)),
                ),
        ));
    }
    if !projection.locals.is_empty() {
        list = list.push(section_heading("On this device", projection.locals.len()));
        if table {
            list = list.push(table_header());
        }
        for row in &projection.locals {
            list = list.push(local_row(
                row,
                scene.selected.as_ref() == Some(&row.key),
                table,
                light,
            ));
        }
    }
    if !projection.remotes.is_empty() {
        list = list.push(section_heading(
            "Available in Connect",
            projection.remotes.len(),
        ));
        for cube in projection.remotes.iter().copied() {
            list = list.push(remote_row(cube));
        }
    }
    if scene.local_total + scene.remote_total == 0 && !projection.no_matches() {
        list = list.push(tenshu::card(tenshu::body("No Cubes on this network.")));
    }
    let details = scene
        .selected
        .as_ref()
        .and_then(|key| projection.locals.iter().find(|row| &row.key == key))
        .map(|row| details(row, light));
    match details {
        None => list.into(),
        Some(panel) if width >= LIST_MIN_WIDTH + DETAILS_WIDTH + PANEL_GAP => Row::new()
            .spacing(PANEL_GAP)
            .align_y(Alignment::Start)
            .push(list)
            .push(Container::new(panel).width(Length::Fixed(DETAILS_WIDTH)))
            .into(),
        Some(panel) => Column::new()
            .spacing(16)
            .width(Length::Fill)
            .push(panel)
            .push(list)
            .into(),
    }
}

/// The v113 List body. Home's existing chrome, single scroll owner and quota
/// dock surround this pure body in the production Home route.
pub(super) fn list_body(home: &Home) -> Element<'_, ViewMessage> {
    let projection = launcher_catalog::project(home);
    let scene = project_catalog(&projection);
    let light = matches!(
        home.theme_mode,
        coincube_ui::theme::palette::ThemeMode::Light
    );
    let fallback = (home.catalog.mode == LauncherMode::Cubes
        && projection.local_total > launcher_catalog::FLOOR_MAX_CUBES)
        .then(|| tenshu::caption("List view for more than 20 Cubes on this network."));
    Column::new()
        .spacing(8)
        .push(fallback)
        .push(
            responsive(move |size: Size| list_layout(&projection, &scene, size.width, light))
                .height(Length::Shrink),
        )
        .into()
}

fn floor_cube<'a>(
    row: &launcher_catalog::LocalRow<'_>,
    selected: bool,
    width: f32,
    light: bool,
    opacity: f32,
) -> Element<'a, ViewMessage> {
    let glyph = cube_glyph_with_opacity(row.cube, light, opacity)
        .width(Length::Fixed(launcher_scene::REST_GLYPH_WIDTH))
        .height(Length::Fixed(launcher_scene::REST_GLYPH_HEIGHT));
    let label = Container::new(
        Row::new()
            .spacing(8)
            .align_y(Alignment::Center)
            .push(catalog_avatar(&row.name))
            .push(
                Column::new()
                    .spacing(2)
                    .push(tenshu::label(row.name.clone()).wrapping(Wrapping::WordOrGlyph))
                    .push(tenshu::caption(if row.actionable {
                        unlock_prompt(row.cube)
                    } else {
                        "Unavailable on this network"
                    })),
            ),
    )
    .width(Length::Fixed(width - 18.0))
    .center_x(Length::Fill);
    Action::new(
        launcher_catalog::select_id(&row.key),
        Column::new()
            .spacing(4)
            .align_x(Alignment::Center)
            .push(glyph)
            .push(label),
        focus,
    )
    .width(Length::Fill)
    .padding([10, 6])
    .style(floor_style(selected))
    .on_press_maybe(
        row.actionable
            .then(|| ViewMessage::Catalog(CatalogMessage::SelectFloor(row.key.clone()))),
    )
    .into()
}

fn page_arrow<'a>(previous: bool, target: Option<usize>) -> Element<'a, ViewMessage> {
    let label = if previous { "‹" } else { "›" };
    let id = if previous {
        "tenshu.launcher.page.previous"
    } else {
        "tenshu.launcher.page.next"
    };
    Action::new(Id::from(id), tenshu::label(label).size(30), focus)
        .width(Length::Fixed(48.0))
        .height(Length::Fixed(48.0))
        .style(move |theme: &theme::Theme, status| iced_button::Style {
            background: Some(iced::Background::Color(theme.colors.shell.panel)),
            text_color: if target.is_some() {
                theme.colors.shell.ink
            } else {
                theme.colors.shell.muted
            },
            border: iced::Border {
                color: if matches!(status, iced_button::Status::Hovered) {
                    theme.colors.shell.ink
                } else {
                    theme.colors.shell.line
                },
                width: 1.0,
                radius: 24.0.into(),
            },
            ..Default::default()
        })
        .on_press_maybe(target.map(|page| ViewMessage::Catalog(CatalogMessage::Page(page))))
        .into()
}

fn page_dot<'a>(index: usize, active: bool) -> Element<'a, ViewMessage> {
    let dot = Container::new(Space::new())
        .width(Length::Fixed(8.0))
        .height(Length::Fixed(8.0))
        .style(move |theme: &theme::Theme| container::Style {
            background: Some(iced::Background::Color(if active {
                theme.colors.shell.accent
            } else {
                theme.colors.shell.line
            })),
            border: iced::Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    Action::new(
        Id::from(format!("tenshu.launcher.page.dot.{index}")),
        dot,
        focus,
    )
    .padding(8)
    .style(|theme: &theme::Theme, status| iced_button::Style {
        background: Some(iced::Background::Color(
            if matches!(
                status,
                iced_button::Status::Hovered | iced_button::Status::Pressed
            ) {
                theme.colors.shell.hover
            } else {
                iced::Color::TRANSPARENT
            },
        )),
        text_color: theme.colors.shell.ink,
        ..Default::default()
    })
    .on_press(ViewMessage::Catalog(CatalogMessage::Page(index)))
    .into()
}

fn floor_layout<'a>(
    projection: &Projection<'_>,
    page: usize,
    width: f32,
    light: bool,
    motion: Option<&launcher_scene::Motion>,
) -> Element<'a, ViewMessage> {
    debug_assert!(!projection.locals.is_empty());
    // The wide row is exactly 1200px. Switching to it any earlier lets the
    // clipped stage cut the first and last Cube at supported window widths.
    let compact = width < 1200.0;
    let pages = projection
        .locals
        .len()
        .div_ceil(launcher_catalog::FLOOR_PAGE_SIZE)
        .max(1);
    let page = page.min(pages - 1);
    let stage_height = if compact { 600.0 } else { 740.0 };
    let backdrop = Container::new(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(stage_height))
        .style(|theme: &theme::Theme| container::Style {
            background: Some(iced::Background::Color(theme.colors.shell.background)),
            ..Default::default()
        });
    let plane = Container::new(
        floor_plane(light)
            .width(Length::Fill)
            .height(Length::Fixed(stage_height * 0.62))
            .content_fit(iced::ContentFit::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fixed(stage_height))
    .align_y(iced::alignment::Vertical::Bottom);
    let mut stage = Stack::new()
        .width(Length::Fill)
        .height(Length::Fixed(stage_height))
        .push(backdrop)
        .push(plane);
    let selected = projection
        .selected
        .and_then(|index| projection.locals.get(index));
    let visible: Vec<_> = projection
        .locals
        .iter()
        .skip(page * launcher_catalog::FLOOR_PAGE_SIZE)
        .take(launcher_catalog::FLOOR_PAGE_SIZE)
        .collect();
    let floor_width = if compact { width - 96.0 } else { 1200.0 };
    let slot_width =
        (floor_width - 12.0 * visible.len().saturating_sub(1) as f32) / visible.len().max(1) as f32;
    let slot_center = |index: usize| {
        (width - floor_width) * 0.5 + slot_width * 0.5 + index as f32 * (slot_width + 12.0)
    };
    let rest_y = if compact { 221.0 } else { 265.0 };
    let mut cubes = Row::new()
        .spacing(12)
        .align_y(Alignment::Center)
        .width(Length::Fixed(floor_width));
    for peer in &visible {
        let moving = selected.is_some_and(|row| row.key == peer.key)
            || motion
                .and_then(|m| m.previous.as_ref())
                .is_some_and(|key| *key == peer.key);
        cubes = cubes.push(if moving {
            Space::new()
                .width(Length::Fill)
                .height(Length::Fixed(330.0))
                .into()
        } else {
            floor_cube(
                peer,
                false,
                if compact { 200.0 } else { 250.0 },
                light,
                if selected.is_some() {
                    motion.map_or(0.4, |m| m.peer_opacity(std::time::Instant::now()))
                } else {
                    1.0
                },
            )
        });
    }
    stage = stage.push(
        Container::new(
            Column::new()
                .push(Space::new().height(Length::Fixed(if compact { 90.0 } else { 134.0 })))
                .push(
                    Container::new(cubes)
                        .width(Length::Fill)
                        .center_x(Length::Fill),
                ),
        )
        .width(Length::Fill)
        .height(Length::Fixed(stage_height)),
    );

    if let Some(row) = selected {
        let now = std::time::Instant::now();
        let progress = motion.map_or(1.0, |m| m.progress(now));
        let start_index = visible
            .iter()
            .position(|peer| peer.key == row.key)
            .unwrap_or(0);
        let outgoing = motion.and_then(|m| {
            m.previous.as_ref().and_then(|key| {
                let index = visible.iter().position(|peer| peer.key == *key)?;
                let peer = visible[index];
                Some((
                    (slot_center(index), rest_y),
                    peer.key.chain,
                    launcher_scene::FaceText {
                        name: &peer.name,
                        backup: backup(peer.cube).0,
                        connect: &peer.connect.1,
                        wallets: local_wallets(peer.cube),
                        passkey: peer.cube.is_passkey_cube(),
                        vault: peer.cube.vault_wallet_id.is_some(),
                        imported_bitcoin: peer.cube.has_imported_bitcoin(),
                        backup_confirmed: peer.cube.backed_up || peer.cube.has_recovery_kit(),
                    },
                    m.outgoing_progress(now),
                ))
            })
        });
        let target_y = if compact { 300.0 } else { 340.0 };
        let image = launcher_scene::source(
            width,
            stage_height,
            (slot_center(start_index), rest_y),
            target_y,
            row.key.chain,
            launcher_scene::FaceText {
                name: &row.name,
                backup: backup(row.cube).0,
                connect: &row.connect.1,
                wallets: local_wallets(row.cube),
                passkey: row.cube.is_passkey_cube(),
                vault: row.cube.vault_wallet_id.is_some(),
                imported_bitcoin: row.cube.has_imported_bitcoin(),
                backup_confirmed: row.cube.backed_up || row.cube.has_recovery_kit(),
            },
            light,
            progress,
            outgoing,
        );
        stage = stage.push(
            iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(image.into_bytes()))
                .width(Length::Fill)
                .height(Length::Fixed(stage_height))
                .content_fit(iced::ContentFit::Fill),
        );
        // The selected Cube itself becomes the centered face. Controls arrive
        // only when it is square to the camera and remain real Iced widgets.
        if progress >= 1.0 {
            let panel = Container::new(floor_details(row, light))
                .width(Length::Fixed(420.0))
                .height(Length::Fixed(420.0))
                .style(|theme: &theme::Theme| container::Style {
                    background: Some(iced::Background::Color(theme.colors.shell.panel)),
                    border: iced::Border {
                        color: theme.colors.shell.accent,
                        width: 2.0,
                        radius: 4.0.into(),
                    },
                    ..Default::default()
                });
            stage = stage.push(
                Container::new(
                    Column::new()
                        .push(Space::new().height(Length::Fixed(target_y - 210.0)))
                        .push(panel),
                )
                .width(Length::Fill)
                .height(Length::Fixed(stage_height))
                .center_x(Length::Fill),
            );
        }
        let mut switcher = Row::new().spacing(8).align_y(Alignment::Center);
        for peer in &visible {
            switcher =
                switcher.push(
                    ActionButton::new(
                        launcher_catalog::select_id(&peer.key),
                        switcher_label(&peer.name),
                        if peer.key == row.key {
                            ActionKind::Primary
                        } else {
                            ActionKind::Quiet
                        },
                        focus,
                    )
                    .compact()
                    .width(Length::Fixed(if compact {
                        ((width - 48.0) / visible.len().max(1) as f32 - 8.0).min(180.0)
                    } else {
                        210.0
                    }))
                    .on_press_maybe(peer.actionable.then(|| {
                        ViewMessage::Catalog(CatalogMessage::SelectFloor(peer.key.clone()))
                    })),
                );
        }
        stage = stage.push(
            Container::new(switcher)
                .width(Length::Fill)
                .height(Length::Fixed(stage_height))
                .align_x(iced::alignment::Horizontal::Center)
                .align_y(iced::alignment::Vertical::Bottom)
                .padding(iced::Padding {
                    top: 0.0,
                    right: 0.0,
                    bottom: 24.0,
                    left: 0.0,
                }),
        );
    }
    if pages > 1 && projection.selected.is_none() {
        let arrows = Row::new()
            .width(Length::Fill)
            .align_y(Alignment::Center)
            .push(page_arrow(true, page.checked_sub(1)))
            .push(Space::new().width(Length::Fill))
            .push(page_arrow(false, (page + 1 < pages).then_some(page + 1)));
        stage = stage.push(
            Container::new(arrows)
                .width(Length::Fill)
                .height(Length::Fixed(stage_height))
                .padding([0, 18])
                .align_y(iced::alignment::Vertical::Center),
        );
        let mut pager = Row::new().spacing(4).align_y(Alignment::Center);
        for index in 0..pages {
            pager = pager.push(page_dot(index, index == page));
        }
        pager = pager.push(tenshu::caption(format!(
            "Page {} of {} · swipe, scroll sideways or use the arrow keys",
            page + 1,
            pages
        )));
        stage = stage.push(
            Container::new(
                Column::new()
                    .height(Length::Fill)
                    .push(Space::new().height(Length::Fill))
                    .push(pager)
                    .push(Space::new().height(Length::Fixed(24.0))),
            )
            .width(Length::Fill)
            .height(Length::Fixed(stage_height))
            .center_x(Length::Fill),
        );
    }
    let mut content = Column::new()
        .spacing(0)
        .width(Length::Fill)
        .push(stage.clip(true));
    if !projection.remotes.is_empty() {
        let mut remote = Column::new()
            .spacing(16)
            .width(Length::Fill)
            .padding([24, 24])
            .push(section_heading(
                "Available in Connect",
                projection.remotes.len(),
            ));
        for cube in projection.remotes.iter().copied() {
            remote = remote.push(remote_row(cube));
        }
        content = content.push(remote);
    }
    content.into()
}

pub(super) fn floor_body(home: &Home) -> Element<'_, ViewMessage> {
    let projection = launcher_catalog::project(home);
    if !projection.floor_eligible(home.catalog.mode) {
        return list_body(home);
    }
    let page = home.catalog.page;
    let light = matches!(
        home.theme_mode,
        coincube_ui::theme::palette::ThemeMode::Light
    );
    let motion = home.catalog.motion.clone();
    let scene = project_catalog(&projection);
    responsive(move |size: Size| {
        if size.width < 840.0 {
            list_layout(&projection, &scene, size.width, light)
        } else {
            floor_layout(&projection, page, size.width, light, motion.as_ref())
        }
    })
    .height(Length::Shrink)
    .into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpeningScene {
    cube_name: String,
    network: ChainId,
}

/// The loader's Step remains the only source of progress and errors. The
/// scene projects just the safe identity displayed above its existing body.
pub(crate) fn project_opening(name: &str, network: ChainId) -> OpeningScene {
    OpeningScene {
        cube_name: crate::cube_display::name(name),
        network,
    }
}

pub(crate) fn unlock_turn_progress(started: std::time::Instant, now: std::time::Instant) -> f32 {
    launcher_scene::second_turn_progress(started, now)
}

pub(crate) fn unlock_turn<'a, M: 'a>(
    cube: &crate::app::settings::CubeSettings,
    mode: coincube_ui::theme::palette::ThemeMode,
    progress: f32,
) -> Element<'a, M> {
    let light = matches!(mode, coincube_ui::theme::palette::ThemeMode::Light);
    let name = crate::cube_display::name(&cube.name);
    let source = launcher_scene::second_turn_source(
        cube.network,
        light,
        launcher_scene::FaceText {
            name: &name,
            backup: backup(cube).0,
            connect: "Checking…",
            wallets: local_wallets(cube),
            passkey: cube.is_passkey_cube(),
            vault: cube.vault_wallet_id.is_some(),
            imported_bitcoin: cube.has_imported_bitcoin(),
            backup_confirmed: cube.backed_up || cube.has_recovery_kit(),
        },
        progress,
    );
    let cube_image =
        iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(source.into_bytes()))
            .width(Length::Fixed(420.0))
            .height(Length::Fixed(420.0));
    Container::new(cube_image)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(|theme: &theme::Theme| container::Style {
            background: Some(iced::Background::Color(theme.colors.shell.background)),
            ..Default::default()
        })
        .into()
}

/// Keep the real unlock widgets mounted behind the noninteractive turn art.
/// They can take focus and input as soon as Open changes the tab state.
pub(crate) fn unlock_turn_with_controls<'a, M: 'a>(
    cube: &crate::app::settings::CubeSettings,
    mode: coincube_ui::theme::palette::ThemeMode,
    progress: Option<f32>,
    controls: Element<'a, M>,
) -> Element<'a, M> {
    let visual: Element<'a, M> = match progress {
        Some(progress) => unlock_turn(cube, mode, progress),
        None => Space::new().width(Length::Fill).height(Length::Fill).into(),
    };
    Stack::new()
        .width(Length::Fill)
        .height(Length::Fill)
        .push(controls)
        .push(visual)
        .into()
}

/// The resting unlock face shared by the real PIN and passkey screens. Their
/// existing widgets and messages stay inside `controls`; this only supplies
/// the Launcher stage around them.
pub(crate) fn unlock_face<'a, M: 'a>(
    cube: &crate::app::settings::CubeSettings,
    mode: coincube_ui::theme::palette::ThemeMode,
    controls: Element<'a, M>,
) -> Element<'a, M> {
    let light = matches!(mode, coincube_ui::theme::palette::ThemeMode::Light);
    // The tab hands off to the established PIN/passkey state immediately.
    // Render that state's live widgets on the second (270°) square face.
    let back = iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(
        launcher_scene::unlock_source(cube.network, light).into_bytes(),
    ))
    .width(Length::Fixed(420.0))
    .height(Length::Fixed(420.0));
    let face = Container::new(
        Column::new()
            .spacing(12)
            .push(tenshu::caption(format!(
                "Open {} · {}",
                crate::cube_display::name(&cube.name),
                cube.network.label()
            )))
            .push(controls),
    )
    .width(Length::Fixed(416.0))
    .height(Length::Fixed(416.0))
    .padding(22)
    .style(|theme: &theme::Theme| container::Style {
        background: Some(iced::Background::Color(theme.colors.shell.panel)),
        ..Default::default()
    });
    Container::new(
        Stack::new()
            .width(Length::Fixed(420.0))
            .height(Length::Fixed(420.0))
            .push(back)
            .push(Container::new(face).center(Length::Fill)),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .center_x(Length::Fill)
    .center_y(Length::Fill)
    .into()
}

fn unlock_floor_stage<'a, M: 'a + 'static>(
    context: &launcher_catalog::LauncherSceneContext,
    cube: &crate::app::settings::CubeSettings,
    mode: coincube_ui::theme::palette::ThemeMode,
    width: f32,
    controls: Option<Element<'a, M>>,
    progress: Option<f32>,
) -> Element<'a, M> {
    let light = matches!(mode, coincube_ui::theme::palette::ThemeMode::Light);
    let compact = width < 1200.0;
    let stage_height = if compact { 600.0 } else { 740.0 };
    let target_y = if compact { 300.0 } else { 340.0 };
    let floor_width = if compact { width - 96.0 } else { 1200.0 };
    let slot_width = (floor_width - 12.0 * context.peers.len().saturating_sub(1) as f32)
        / context.peers.len().max(1) as f32;
    let mut peers = Row::new()
        .spacing(12)
        .align_y(Alignment::Center)
        .width(Length::Fixed(floor_width));
    for (index, peer) in context.peers.iter().enumerate() {
        if index == context.selected_slot && peer.key == context.selected {
            peers = peers.push(
                Space::new()
                    .width(Length::Fill)
                    .height(Length::Fixed(330.0)),
            );
            continue;
        }
        let source = if light {
            &peer.glyph_light
        } else {
            &peer.glyph_dark
        };
        let source = source.replace("<svg ", "<svg opacity=\"0.4\" ");
        let glyph =
            iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(source.into_bytes()))
                .width(Length::Fixed(launcher_scene::REST_GLYPH_WIDTH))
                .height(Length::Fixed(launcher_scene::REST_GLYPH_HEIGHT));
        peers = peers.push(
            Container::new(
                Column::new()
                    .spacing(4)
                    .align_x(Alignment::Center)
                    .push(glyph)
                    .push(tenshu::label(peer.name.clone()).wrapping(Wrapping::WordOrGlyph))
                    .push(tenshu::caption(peer.status)),
            )
            .width(Length::Fixed(slot_width))
            .height(Length::Fixed(330.0)),
        );
    }
    let backdrop = Container::new(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(stage_height))
        .style(|theme: &theme::Theme| container::Style {
            background: Some(iced::Background::Color(theme.colors.shell.background)),
            ..Default::default()
        });
    let plane = Container::new(
        floor_plane(light)
            .width(Length::Fill)
            .height(Length::Fixed(stage_height * 0.62))
            .content_fit(iced::ContentFit::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fixed(stage_height))
    .align_y(iced::alignment::Vertical::Bottom);
    let mut stage = Stack::new()
        .width(Length::Fill)
        .height(Length::Fixed(stage_height))
        .push(backdrop)
        .push(plane)
        .push(
            Container::new(
                Column::new()
                    .push(Space::new().height(Length::Fixed(if compact { 90.0 } else { 134.0 })))
                    .push(
                        Container::new(peers)
                            .width(Length::Fill)
                            .center_x(Length::Fill),
                    ),
            )
            .width(Length::Fill)
            .height(Length::Fixed(stage_height)),
        );
    if let Some(progress) = progress {
        let name = crate::cube_display::name(&cube.name);
        let svg = launcher_scene::second_turn_on_floor_source(
            width,
            stage_height,
            target_y,
            cube.network,
            light,
            launcher_scene::FaceText {
                name: &name,
                backup: context.backup,
                connect: &context.connect,
                wallets: context.wallets,
                passkey: cube.is_passkey_cube(),
                vault: cube.vault_wallet_id.is_some(),
                imported_bitcoin: cube.has_imported_bitcoin(),
                backup_confirmed: cube.backed_up || cube.has_recovery_kit(),
            },
            progress,
        );
        stage = stage.push(
            iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(svg.into_bytes()))
                .width(Length::Fill)
                .height(Length::Fixed(stage_height))
                .content_fit(iced::ContentFit::Fill),
        );
    } else if let Some(controls) = controls {
        let face = Container::new(
            Column::new()
                .spacing(12)
                .push(tenshu::caption(format!(
                    "Open {} · {}",
                    crate::cube_display::name(&cube.name),
                    cube.network.label()
                )))
                .push(controls),
        )
        .width(Length::Fixed(416.0))
        .height(Length::Fixed(416.0))
        .padding(22)
        .style(|theme: &theme::Theme| container::Style {
            background: Some(iced::Background::Color(theme.colors.shell.panel)),
            ..Default::default()
        });
        let cube_face = Stack::new()
            .width(Length::Fixed(420.0))
            .height(Length::Fixed(420.0))
            .push(
                iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(
                    launcher_scene::unlock_source(cube.network, light).into_bytes(),
                ))
                .width(Length::Fixed(420.0))
                .height(Length::Fixed(420.0)),
            )
            .push(Container::new(face).center(Length::Fill));
        stage = stage.push(
            Container::new(
                Column::new()
                    .push(Space::new().height(Length::Fixed(target_y - 210.0)))
                    .push(cube_face),
            )
            .width(Length::Fill)
            .height(Length::Fixed(stage_height))
            .center_x(Length::Fill),
        );
    }
    Column::new()
        .width(Length::Fill)
        .height(Length::Fill)
        .push(
            Container::new(
                Row::new()
                    .spacing(12)
                    .align_y(Alignment::Center)
                    .push(icon::cube_icon())
                    .push(tenshu::label("Tenshu"))
                    .push(tenshu::caption(format!(
                        "{} · Page {}",
                        cube.network.label(),
                        context.page + 1
                    ))),
            )
            .height(Length::Fixed(64.0))
            .width(Length::Fill)
            .padding([0.0, 24.0])
            .style(opening_rail_style),
        )
        .push(stage.clip(true))
        .push(
            Container::new(tenshu::caption("Create Cube     Import or restore a Cube"))
                .width(Length::Fill)
                .height(Length::Fill)
                .padding([20.0, 24.0])
                .style(opening_rail_style),
        )
        .into()
}

pub(crate) fn unlock_face_on_floor<'a, M: 'a + 'static>(
    cube: &'a crate::app::settings::CubeSettings,
    mode: coincube_ui::theme::palette::ThemeMode,
    context: &'a launcher_catalog::LauncherSceneContext,
    controls: impl Fn() -> Element<'a, M> + 'a,
) -> Element<'a, M> {
    responsive(move |size: Size| {
        unlock_floor_stage(context, cube, mode, size.width, Some(controls()), None)
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

pub(crate) fn unlock_turn_with_floor_controls<'a, M: 'a + 'static>(
    cube: &'a crate::app::settings::CubeSettings,
    mode: coincube_ui::theme::palette::ThemeMode,
    context: &'a launcher_catalog::LauncherSceneContext,
    progress: Option<f32>,
    controls: Element<'a, M>,
) -> Element<'a, M> {
    // Keep the same root and child shape after the turn. Iced diffs widget
    // state by position, so replacing this Stack with bare controls drops the
    // focused PIN field or passkey action at the animation boundary.
    let visual: Element<'a, M> = match progress {
        Some(progress) => responsive(move |size: Size| {
            unlock_floor_stage(context, cube, mode, size.width, None, Some(progress))
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
        None => Space::new().width(Length::Fill).height(Length::Fill).into(),
    };
    Stack::new()
        .width(Length::Fill)
        .height(Length::Fill)
        .push(controls)
        .push(visual)
        .into()
}

fn opening_rail_style(theme: &theme::Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(theme.colors.shell.rail)),
        text_color: Some(theme.colors.shell.ink),
        ..Default::default()
    }
}

/// Places the real loader stage under the quiet Opening rail. The loader body
/// owns scan guidance, backend diagnostics and the error action set; rendering
/// it from the same Step prevents this scene from granting a new Retry route.
pub(crate) fn opening_body<'a>(
    scene: OpeningScene,
    step: &'a loader::Step,
    quote: &'a Quote,
    image: &'a iced::widget::image::Handle,
    restricted_claim: bool,
) -> Element<'a, loader::ViewMessage> {
    let rail = Container::new(
        Row::new()
            .spacing(16)
            .align_y(Alignment::Center)
            .height(Length::Fill)
            .push(
                Row::new()
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(icon::cube_icon())
                    .push(tenshu::label("Tenshu")),
            )
            .push(
                tenshu::body(format!("Opening {}", scene.cube_name))
                    .width(Length::Fill)
                    .wrapping(Wrapping::WordOrGlyph),
            )
            .push(tenshu::caption(scene.network.label())),
    )
    .width(Length::Fill)
    .height(Length::Fixed(64.0))
    .padding([0.0, 24.0])
    .style(opening_rail_style);
    let stage = Container::new(loader::view(step, quote, image, restricted_claim))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill);
    Column::new()
        .width(Length::Fill)
        .height(Length::Fill)
        .push(rail)
        .push(rule::horizontal(1))
        .push(stage)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::view::shared::landing::capture;
    use crate::dir::CoincubeDirectory;
    use coincube_core::miniscript::bitcoin::Network;
    use coincube_ui::font;
    use iced::advanced::{
        layout as widget_layout,
        renderer::Headless,
        widget::{operation::Focusable, Operation, Tree},
        Layout,
    };
    use iced::Rectangle;
    use std::path::PathBuf;

    fn floor_context(
        cube: &crate::app::settings::CubeSettings,
    ) -> launcher_catalog::LauncherSceneContext {
        let peer = crate::app::settings::CubeSettings::new_with_raw_id(
            "peer".into(),
            "Travel".into(),
            cube.network,
        );
        let selected = CubeCatalogKey {
            chain: cube.network,
            cube_id: cube.id.clone(),
        };
        let peer_key = CubeCatalogKey {
            chain: peer.network,
            cube_id: peer.id.clone(),
        };
        launcher_catalog::LauncherSceneContext {
            selected: selected.clone(),
            page: 0,
            selected_slot: 0,
            backup: "Status unknown",
            connect: "Sovereign — local only".into(),
            wallets: "Not yet known",
            peers: vec![
                launcher_catalog::LauncherScenePeer {
                    key: selected,
                    name: "Everyday".into(),
                    status: "PIN prompt",
                    glyph_dark: cube_glyph_source(cube, false),
                    glyph_light: cube_glyph_source(cube, true),
                },
                launcher_catalog::LauncherScenePeer {
                    key: peer_key,
                    name: "Travel".into(),
                    status: "PIN prompt",
                    glyph_dark: cube_glyph_source(&peer, false),
                    glyph_light: cube_glyph_source(&peer, true),
                },
            ],
        }
    }

    #[test]
    fn unlock_floor_keeps_peer_and_live_input_at_compact_and_wide_sizes() {
        let cube = crate::app::settings::CubeSettings::new_with_raw_id(
            "floor".into(),
            "Everyday".into(),
            ChainId::Bitcoin,
        );
        let context = floor_context(&cube);
        let pin = String::new();
        for (width, height, appearance, palette, mode) in [
            (
                1024,
                768,
                "dark",
                theme::Theme::dark(),
                coincube_ui::theme::palette::ThemeMode::Dark,
            ),
            (
                1440,
                900,
                "light",
                theme::Theme::light(),
                coincube_ui::theme::palette::ThemeMode::Light,
            ),
        ] {
            let scene = unlock_face_on_floor(&cube, mode, &context, || {
                iced::widget::text_input("", &pin)
                    .id(crate::pin_input::first_digit_id())
                    .on_input(|value| value)
                    .into()
            });
            // The inspector reserves 100px for an outer chrome that this
            // standalone Launcher scene owns itself.
            let mounted = capture::inspect_full_window(
                "v113-unlock-floor",
                scene,
                (width, height + 100),
                appearance,
                palette,
            );
            assert!(mounted.contains("Travel"));
            assert!(mounted.contains("PIN prompt"));
            assert!(mounted.contains("Open Everyday"));
            assert!(mounted.has_focus(crate::pin_input::first_digit_id()));
            let face = mounted
                .text
                .iter()
                .find(|(value, _)| value.starts_with("Open Everyday"))
                .map(|(_, bounds)| *bounds)
                .expect("selected unlock face caption");
            let expected = if width == 1024 { 176.0 } else { 216.0 };
            assert!(
                (face.y - expected).abs() < 22.0,
                "{}px floor face moved from its selected pose: {:?}",
                width,
                face
            );
        }
    }

    #[test]
    fn unlock_turn_keeps_pin_focus_when_art_completes() {
        #[derive(Default)]
        struct FocusTargets {
            ids: Vec<iced::advanced::widget::Id>,
            first_focused: bool,
        }
        impl Operation for FocusTargets {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn focusable(
                &mut self,
                id: Option<&iced::advanced::widget::Id>,
                _: Rectangle,
                state: &mut dyn Focusable,
            ) {
                if let Some(id) = id {
                    self.ids.push(id.clone());
                    if *id == crate::pin_input::first_digit_id() {
                        self.first_focused = state.is_focused();
                    }
                }
            }
        }
        struct FocusFirst;
        impl Operation for FocusFirst {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn focusable(
                &mut self,
                id: Option<&iced::advanced::widget::Id>,
                _: Rectangle,
                state: &mut dyn Focusable,
            ) {
                if id == Some(&crate::pin_input::first_digit_id()) {
                    state.focus();
                }
            }
        }

        let cube = crate::app::settings::CubeSettings::new_with_raw_id(
            "unlock-turn".into(),
            "Everyday".into(),
            ChainId::Bitcoin,
        );
        let pin = String::new();
        let controls: Element<'_, String> = iced::widget::text_input("", &pin)
            .id(crate::pin_input::first_digit_id())
            .on_input(|value| value)
            .into();
        let mut element = unlock_turn_with_controls(
            &cube,
            coincube_ui::theme::palette::ThemeMode::Dark,
            Some(0.25),
            controls,
        );
        for bytes in font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            font::REGULAR,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &widget_layout::Limits::new(Size::ZERO, Size::new(1000.0, 900.0)),
        );
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut FocusFirst);
        let mut targets = FocusTargets::default();
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut targets);
        assert!(targets.ids.contains(&crate::pin_input::first_digit_id()));
        assert!(targets.first_focused);

        drop(element);
        let settled_controls: Element<'_, String> = iced::widget::text_input("", &pin)
            .id(crate::pin_input::first_digit_id())
            .on_input(|value| value)
            .into();
        let mut settled = unlock_turn_with_controls(
            &cube,
            coincube_ui::theme::palette::ThemeMode::Dark,
            None,
            settled_controls,
        );
        tree.diff(settled.as_widget());
        let settled_node = settled.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &widget_layout::Limits::new(Size::ZERO, Size::new(1000.0, 900.0)),
        );
        let mut targets = FocusTargets::default();
        settled.as_widget_mut().operate(
            &mut tree,
            Layout::new(&settled_node),
            &renderer,
            &mut targets,
        );
        assert!(
            targets.first_focused,
            "input focus must survive turn completion"
        );

        let mode = coincube_ui::theme::palette::ThemeMode::Dark;
        let context = floor_context(&cube);
        let floor_controls = unlock_face_on_floor(
            &cube,
            coincube_ui::theme::palette::ThemeMode::Dark,
            &context,
            || {
                iced::widget::text_input("", &pin)
                    .id(crate::pin_input::first_digit_id())
                    .on_input(|value| value)
                    .into()
            },
        );
        let mut floor_turn = unlock_turn_with_floor_controls(
            &cube,
            coincube_ui::theme::palette::ThemeMode::Dark,
            &context,
            Some(0.0),
            floor_controls,
        );
        let mut floor_tree = Tree::new(floor_turn.as_widget());
        let floor_node = floor_turn.as_widget_mut().layout(
            &mut floor_tree,
            &renderer,
            &widget_layout::Limits::new(Size::ZERO, Size::new(1024.0, 768.0)),
        );
        floor_turn.as_widget_mut().operate(
            &mut floor_tree,
            Layout::new(&floor_node),
            &renderer,
            &mut FocusFirst,
        );
        let mut floor_targets = FocusTargets::default();
        floor_turn.as_widget_mut().operate(
            &mut floor_tree,
            Layout::new(&floor_node),
            &renderer,
            &mut floor_targets,
        );
        assert!(floor_targets.first_focused, "PIN focuses at turn start");

        drop(floor_turn);
        let settled_floor_controls = unlock_face_on_floor(&cube, mode, &context, || {
            iced::widget::text_input("", &pin)
                .id(crate::pin_input::first_digit_id())
                .on_input(|value| value)
                .into()
        });
        let mut settled_floor =
            unlock_turn_with_floor_controls(&cube, mode, &context, None, settled_floor_controls);
        floor_tree.diff(settled_floor.as_widget());
        let settled_floor_node = settled_floor.as_widget_mut().layout(
            &mut floor_tree,
            &renderer,
            &widget_layout::Limits::new(Size::ZERO, Size::new(1024.0, 768.0)),
        );
        let mut floor_targets = FocusTargets::default();
        settled_floor.as_widget_mut().operate(
            &mut floor_tree,
            Layout::new(&settled_floor_node),
            &renderer,
            &mut floor_targets,
        );
        assert!(floor_targets.first_focused, "PIN focus survives floor turn");
    }

    #[test]
    fn passkey_begin_action_is_focusable_during_turn() {
        #[derive(Default)]
        struct FocusTargets(Vec<iced::advanced::widget::Id>);
        impl Operation for FocusTargets {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn focusable(
                &mut self,
                id: Option<&iced::advanced::widget::Id>,
                _: Rectangle,
                _: &mut dyn Focusable,
            ) {
                if let Some(id) = id {
                    self.0.push(id.clone());
                }
            }
        }

        let cube = crate::app::settings::CubeSettings::new_with_raw_id(
            "passkey-turn".into(),
            "Everyday".into(),
            ChainId::Bitcoin,
        );
        let unlock = crate::passkey_unlock::PasskeyUnlock::new(
            cube.clone(),
            PathBuf::new(),
            crate::pin_entry::PinEntrySuccess::LoadApp {
                datadir: CoincubeDirectory::new(PathBuf::new()),
                config: crate::app::Config::new(false),
                network: ChainId::Bitcoin.bitcoin_network(),
                internal_bitcoind: None,
                backup: None,
                wallet_settings: None,
                connect_client: None,
            },
        );
        let mode = coincube_ui::theme::palette::ThemeMode::Dark;
        let mut element =
            unlock_turn_with_controls(&cube, mode, Some(0.25), unlock.view_with_theme(mode));
        for bytes in font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            font::REGULAR,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &widget_layout::Limits::new(Size::ZERO, Size::new(1000.0, 900.0)),
        );
        let mut targets = FocusTargets::default();
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut targets);
        assert!(targets
            .0
            .contains(&crate::passkey_unlock::begin_button_id()));

        struct FocusPasskey;
        impl Operation for FocusPasskey {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn focusable(
                &mut self,
                id: Option<&iced::advanced::widget::Id>,
                _: Rectangle,
                state: &mut dyn Focusable,
            ) {
                if id == Some(&crate::passkey_unlock::begin_button_id()) {
                    state.focus();
                }
            }
        }
        let context = floor_context(&cube);
        let mut on_floor = unlock_turn_with_floor_controls(
            &cube,
            mode,
            &context,
            Some(0.0),
            unlock.view_with_theme_and_floor(mode, Some(&context)),
        );
        let mut floor_tree = Tree::new(on_floor.as_widget());
        let floor_node = on_floor.as_widget_mut().layout(
            &mut floor_tree,
            &renderer,
            &widget_layout::Limits::new(Size::ZERO, Size::new(1024.0, 768.0)),
        );
        on_floor.as_widget_mut().operate(
            &mut floor_tree,
            Layout::new(&floor_node),
            &renderer,
            &mut FocusPasskey,
        );
        #[derive(Default)]
        struct PasskeyFocused(bool);
        impl Operation for PasskeyFocused {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn focusable(
                &mut self,
                id: Option<&iced::advanced::widget::Id>,
                _: Rectangle,
                state: &mut dyn Focusable,
            ) {
                if id == Some(&crate::passkey_unlock::begin_button_id()) {
                    self.0 = state.is_focused();
                }
            }
        }
        let mut focused = PasskeyFocused::default();
        on_floor.as_widget_mut().operate(
            &mut floor_tree,
            Layout::new(&floor_node),
            &renderer,
            &mut focused,
        );
        assert!(focused.0, "passkey action focuses at turn start");

        drop(on_floor);
        let mut settled_floor = unlock_turn_with_floor_controls(
            &cube,
            mode,
            &context,
            None,
            unlock.view_with_theme_and_floor(mode, Some(&context)),
        );
        floor_tree.diff(settled_floor.as_widget());
        let settled_floor_node = settled_floor.as_widget_mut().layout(
            &mut floor_tree,
            &renderer,
            &widget_layout::Limits::new(Size::ZERO, Size::new(1024.0, 768.0)),
        );
        let mut focused = PasskeyFocused::default();
        settled_floor.as_widget_mut().operate(
            &mut floor_tree,
            Layout::new(&settled_floor_node),
            &renderer,
            &mut focused,
        );
        assert!(focused.0, "passkey focus survives floor turn");
    }

    fn home_with_cubes() -> Home {
        let mut home = Home::new(
            CoincubeDirectory::new(PathBuf::new()),
            Some(Network::Bitcoin),
        )
        .0;
        home.network = ChainId::Bitcoin;
        home.state = super::super::State::Cubes {
            cubes: vec![
                crate::app::settings::CubeSettings::new_with_raw_id(
                    "first".into(),
                    "Everyday".into(),
                    ChainId::Bitcoin,
                ),
                crate::app::settings::CubeSettings::new_with_raw_id(
                    "second".into(),
                    "Travel".into(),
                    ChainId::Bitcoin,
                ),
            ],
            create_cube: false,
            source: ChainId::Bitcoin,
        };
        home.advisory_notice = None;
        home.catalog.selected = Some(CubeCatalogKey {
            chain: ChainId::Bitcoin,
            cube_id: "second".into(),
        });
        home
    }

    #[test]
    fn imported_bitcoin_face_mark_requires_an_installed_attachment() {
        use crate::app::settings::{
            BitcoinImport, BitcoinImportStage, CubeSettings, ImportedWalletAttachment,
        };
        use coincube_core::{
            miniscript::bitcoin::{
                bip32::{DerivationPath, Xpriv, Xpub},
                secp256k1::Secp256k1,
            },
            wallet_descriptor::ImportedWalletDescriptor,
        };
        use std::str::FromStr;

        let mut cube = CubeSettings::new("Importing".into(), ChainId::Bitcoin);
        assert_eq!(wallet_face_mark(&cube), WalletFaceMark::None);
        cube.bitcoin_import = Some(Box::new(BitcoinImport::pending()));
        assert_eq!(wallet_face_mark(&cube), WalletFaceMark::None);

        let secp = Secp256k1::new();
        let master = Xpriv::new_master(Network::Bitcoin, &[29; 32]).unwrap();
        let account = master
            .derive_priv(&secp, &DerivationPath::from_str("m/84h/0h/0h").unwrap())
            .unwrap();
        let xpub = Xpub::from_priv(&secp, &account);
        let key = format!("[{}/84h/0h/0h]{xpub}", master.fingerprint(&secp));
        let descriptor = ImportedWalletDescriptor::parse(
            &format!("wpkh({key}/0/*)"),
            Some(&format!("wpkh({key}/1/*)")),
            ChainId::Bitcoin,
        )
        .unwrap();
        let wallet = ImportedWalletAttachment::new(descriptor, ChainId::Bitcoin, 123, 200).unwrap();

        cube.bitcoin_import.as_mut().unwrap().stage = BitcoinImportStage::Installing {
            wallet: wallet.clone(),
        };
        assert_eq!(wallet_face_mark(&cube), WalletFaceMark::None);
        cube.bitcoin_import.as_mut().unwrap().stage = BitcoinImportStage::Syncing {
            wallet: wallet.clone(),
        };
        assert_eq!(wallet_face_mark(&cube), WalletFaceMark::ImportedBitcoin);
        cube.bitcoin_import.as_mut().unwrap().stage = BitcoinImportStage::Ready { wallet };
        assert_eq!(wallet_face_mark(&cube), WalletFaceMark::ImportedBitcoin);
    }

    #[test]
    fn cube_face_shows_unlock_route_without_inventing_backup_status() {
        let mut cube = crate::app::settings::CubeSettings::new_with_raw_id(
            "fixture".into(),
            "Fixture".into(),
            ChainId::Bitcoin,
        );
        let unknown_backup = cube_glyph_source(&cube, false);
        assert_eq!(unlock_prompt(&cube), "PIN prompt");
        assert!(unknown_backup.contains("M116 83v-6a7"));
        assert!(!unknown_backup.contains("cx=\"137\" cy=\"64\""));

        cube.backed_up = true;
        let recorded_backup = cube_glyph_source(&cube, true);
        assert!(recorded_backup.contains("cx=\"137\" cy=\"64\""));
        assert!(recorded_backup.contains("#e6e0d5"));

        cube.passkey_metadata = Some(crate::app::settings::PasskeyMetadata {
            credential_id: "fixture".into(),
            rp_id: "example.test".into(),
            created_at: 0,
            label: None,
        });
        let passkey = cube_glyph_source(&cube, false);
        assert_eq!(unlock_prompt(&cube), "Passkey prompt");
        assert!(passkey.contains("m122 86 12 12"));
        assert!(!passkey.contains("M116 83v-6a7"));
    }

    #[test]
    fn floor_pages_four_real_cubes_and_never_hides_connect_restore() {
        let mut home = home_with_cubes();
        home.catalog.selected = None;
        if let super::super::State::Cubes { cubes, .. } = &mut home.state {
            cubes[0].set_vault(crate::app::settings::VaultIdentity {
                wallet_id: crate::app::settings::WalletId::new("fixture".into(), Some(1)),
                fingerprint: Some("12345678".into()),
            });
            cubes[0].backed_up = true;
            cubes[1].recovery_kit_password_backed_up = true;
            for index in 3..=9 {
                cubes.push(crate::app::settings::CubeSettings::new_with_raw_id(
                    format!("cube-{index}"),
                    format!("Cube {index}"),
                    ChainId::Bitcoin,
                ));
            }
        }
        home.remote_cubes.push(super::super::RemoteCube {
            id: 7,
            uuid: "restore-me".into(),
            name: "Connect backup".into(),
            network: crate::app::settings::network_to_api_string(ChainId::Bitcoin),
            has_recovery_kit: true,
            has_encrypted_seed: true,
            phone_recoverable: false,
            phone_full_cube: false,
        });
        for (page, visible, hidden) in [
            (0, "Everyday", "Cube 5"),
            (1, "Cube 5", "Everyday"),
            (2, "Cube 9", "Cube 5"),
        ] {
            home.catalog.page = page;
            let scenario = format!("v113-launcher-floor-page{}", page + 1);
            for (size, appearance, palette) in [
                ((1024, 768), "dark", theme::Theme::dark()),
                ((1440, 900), "light", theme::Theme::light()),
            ] {
                home.theme_mode = if appearance == "light" {
                    coincube_ui::theme::palette::ThemeMode::Light
                } else {
                    coincube_ui::theme::palette::ThemeMode::Dark
                };
                let mounted = capture::inspect_full_window(
                    &scenario,
                    floor_body(&home),
                    size,
                    appearance,
                    palette,
                );
                assert!(mounted.contains(visible));
                assert!(!mounted.contains(hidden));
                assert!(mounted.contains(&format!("Page {} of 3", page + 1)));
                assert!(mounted.has_focus(launcher_catalog::restore_id("restore-me")));
            }
        }
    }

    #[test]
    fn selected_floor_keeps_a_real_cube_and_switcher_controls_visible() {
        let mut home = home_with_cubes();
        home.catalog.mode = LauncherMode::Cubes;
        if let super::super::State::Cubes { cubes, .. } = &mut home.state {
            cubes[1].passkey_metadata = Some(crate::app::settings::PasskeyMetadata {
                credential_id: "fixture".into(),
                rp_id: "example.test".into(),
                created_at: 0,
                label: None,
            });
        }
        let selected = CubeCatalogKey {
            chain: ChainId::Bitcoin,
            cube_id: "second".into(),
        };
        let other = CubeCatalogKey {
            chain: ChainId::Bitcoin,
            cube_id: "first".into(),
        };
        for (size, appearance, palette) in [
            ((1024, 768), "dark", theme::Theme::dark()),
            ((1440, 900), "light", theme::Theme::light()),
        ] {
            home.theme_mode = if appearance == "light" {
                coincube_ui::theme::palette::ThemeMode::Light
            } else {
                coincube_ui::theme::palette::ThemeMode::Dark
            };
            let mounted = capture::inspect_full_window(
                "v113-selected-cube-face",
                floor_body(&home),
                size,
                appearance,
                palette,
            );
            assert!(mounted.contains("Travel"));
            assert!(mounted.contains("Everyday"));
            assert!(mounted.contains("Network"));
            assert!(mounted.contains("Next step"));
            assert!(mounted.contains("Passkey prompt"));
            // The dimmed peer retains its own state instead of becoming a
            // generic "Select Cube" label while Travel's face is open.
            assert!(mounted.contains("PIN prompt"));
            assert!(!mounted.contains("Select Cube"));
            assert!(mounted.has_focus(launcher_catalog::details_open_id(&selected)));
            assert!(mounted.has_focus(launcher_catalog::select_id(&other)));
            assert!(mounted.has_focus(launcher_catalog::close_details_id()));
        }
    }

    #[test]
    fn unlock_cube_keeps_network_and_persisted_face_marks_in_both_themes() {
        let mut cube = crate::app::settings::CubeSettings::new_with_raw_id(
            "marked".into(),
            "Marked".into(),
            ChainId::Bitcoin,
        );
        cube.backed_up = true;
        cube.set_vault(crate::app::settings::VaultIdentity {
            wallet_id: crate::app::settings::WalletId::new("fixture".into(), Some(1)),
            fingerprint: Some("12345678".into()),
        });
        let dark = cube_glyph_source(&cube, false);
        let light = cube_glyph_source(&cube, true);
        for svg in [&dark, &light] {
            assert!(
                svg.contains("data:image/png;base64,"),
                "Bitcoin coin missing"
            );
            assert!(svg.contains("clip-path=\"url(#launcher-top)\""));
            assert!(svg.contains("M58 73 67 77V87"), "Vault mark missing");
            assert!(svg.contains("m133 64 3 3 5-6"), "backup mark missing");
        }
        assert!(dark.contains("stroke=\"#f3821d\""));
        assert!(light.contains("stroke=\"#f3821d\""));
        assert_ne!(dark, light);
        for (appearance, mode, palette) in [
            (
                "dark",
                coincube_ui::theme::palette::ThemeMode::Dark,
                theme::Theme::dark(),
            ),
            (
                "light",
                coincube_ui::theme::palette::ThemeMode::Light,
                theme::Theme::light(),
            ),
        ] {
            let controls: Element<'_, ViewMessage> = tenshu::body("Unlock controls").into();
            let mounted = capture::inspect_full_window(
                "v113-unlock-face-identity",
                unlock_face(&cube, mode, controls),
                (1024, 768),
                appearance,
                palette,
            );
            assert!(mounted.contains("Marked"));
            assert!(mounted.contains(cube.network.label()));
            assert!(mounted.contains("Unlock controls"));
        }
    }

    #[test]
    fn selected_floor_switcher_bounds_four_long_names_at_compact_width() {
        let mut home = home_with_cubes();
        home.catalog.mode = LauncherMode::Cubes;
        if let super::super::State::Cubes { cubes, .. } = &mut home.state {
            for (index, cube) in cubes.iter_mut().enumerate() {
                cube.name = format!("Cube {index} {}", "A".repeat(80));
            }
            for index in 2..4 {
                let mut cube = crate::app::settings::CubeSettings::new_with_raw_id(
                    format!("long-{index}"),
                    format!("Cube {index} {}", "B".repeat(80)),
                    ChainId::Bitcoin,
                );
                cube.backed_up = true;
                cubes.push(cube);
            }
        }
        for (appearance, palette) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            let mounted = capture::inspect_full_window(
                "v113-selected-four-long-names",
                floor_body(&home),
                (1024, 768),
                appearance,
                palette,
            );
            for index in 0..4 {
                let label = switcher_label(&format!(
                    "Cube {index} {}",
                    if index < 2 {
                        "A".repeat(80)
                    } else {
                        "B".repeat(80)
                    }
                ));
                assert!(mounted.text.iter().any(|(text, _)| text == &label));
            }
        }
    }

    #[test]
    fn four_cube_floor_stays_inside_stage_across_wide_breakpoint() {
        let mut home = home_with_cubes();
        home.catalog.mode = LauncherMode::Cubes;
        home.catalog.selected = None;
        if let super::super::State::Cubes { cubes, .. } = &mut home.state {
            for index in 3..=4 {
                cubes.push(crate::app::settings::CubeSettings::new_with_raw_id(
                    format!("cube-{index}"),
                    format!("Cube {index}"),
                    ChainId::Bitcoin,
                ));
            }
        }
        let ids = ["first", "second", "cube-3", "cube-4"];
        for width in [1100, 1160, 1200] {
            for (appearance, palette) in [
                ("dark", theme::Theme::dark()),
                ("light", theme::Theme::light()),
            ] {
                home.theme_mode = if appearance == "light" {
                    coincube_ui::theme::palette::ThemeMode::Light
                } else {
                    coincube_ui::theme::palette::ThemeMode::Dark
                };
                let mounted = capture::inspect_full_window(
                    &format!("v113-floor-breakpoint-{width}"),
                    floor_body(&home),
                    (width, 900),
                    appearance,
                    palette,
                );
                let cubes: Vec<_> = ids
                    .iter()
                    .map(|id| {
                        let key = CubeCatalogKey {
                            chain: ChainId::Bitcoin,
                            cube_id: (*id).into(),
                        };
                        mounted
                            .focus
                            .iter()
                            .find(|(actual, _)| actual == &launcher_catalog::select_id(&key))
                            .map(|(_, bounds)| *bounds)
                            .expect("each Cube has a selectable floor target")
                    })
                    .collect();
                assert!(cubes[0].x >= -0.5, "left Cube clipped at {}", width);
                assert!(
                    cubes[3].x + cubes[3].width <= width as f32 + 0.5,
                    "right Cube clipped at {}",
                    width
                );
                assert!(cubes
                    .windows(2)
                    .all(|pair| { pair[0].x + pair[0].width <= pair[1].x + 0.5 }));
            }
        }
    }

    #[test]
    fn mounted_list_preserves_focus_and_unknown_facts_in_both_themes_and_sizes() {
        let home = home_with_cubes();
        let key = CubeCatalogKey {
            chain: ChainId::Bitcoin,
            cube_id: "second".into(),
        };
        for size in [(1024, 768), (1440, 900)] {
            for (appearance, palette) in [
                ("dark", theme::Theme::dark()),
                ("light", theme::Theme::light()),
            ] {
                let mounted = capture::inspect(
                    "v113-launcher-list",
                    list_body(&home),
                    size,
                    appearance,
                    palette,
                );
                assert!(mounted.contains("Everyday"));
                assert!(mounted.contains("Travel"));
                assert!(mounted.contains("Lock state unknown"));
                assert!(mounted.contains("Status unknown"));
                assert!(!mounted.contains("Balance"));
                assert!(!mounted.contains("Checks passed"));
                assert!(mounted.has_focus(search_id().into()));
                assert!(mounted.has_focus(launcher_catalog::select_id(&key)));
                assert!(mounted.has_focus(launcher_catalog::open_id(&key)));
                assert!(mounted.has_focus(launcher_catalog::details_open_id(&key)));
                assert!(mounted.has_focus(launcher_catalog::close_details_id()));
            }
        }
    }

    #[test]
    fn stale_rows_remain_visible_but_not_keyboard_actionable() {
        let mut home = home_with_cubes();
        if let super::super::State::Cubes { source, .. } = &mut home.state {
            *source = ChainId::Testnet4;
        }
        let key = CubeCatalogKey {
            chain: ChainId::Bitcoin,
            cube_id: "second".into(),
        };
        let mounted = capture::inspect(
            "v113-launcher-stale",
            list_body(&home),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        assert!(mounted.contains("Travel"));
        assert!(!mounted.has_focus(launcher_catalog::select_id(&key)));
        assert!(!mounted.has_focus(launcher_catalog::open_id(&key)));
    }

    #[test]
    fn search_and_remote_restore_keep_the_catalog_intents() {
        let mut home = home_with_cubes();
        home.remote_cubes.push(super::super::RemoteCube {
            id: 7,
            uuid: "remote-id".into(),
            name: "Travel backup".into(),
            network: crate::app::settings::network_to_api_string(ChainId::Bitcoin),
            has_recovery_kit: true,
            has_encrypted_seed: true,
            phone_recoverable: false,
            phone_full_cube: false,
        });
        home.catalog.query = "Travel".into();
        let mounted = capture::inspect(
            "v113-launcher-search-remote",
            list_body(&home),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        assert!(mounted.contains("Travel"));
        assert!(!mounted.contains("Everyday"));
        assert!(mounted.contains("Recovery Kit available"));
        assert!(mounted.has_focus(clear_id()));
        assert!(mounted.has_focus(launcher_catalog::restore_id("remote-id")));
        assert!(mounted.has_focus(launcher_catalog::remote_delete_id("remote-id")));
    }

    #[test]
    fn wide_table_mount_keeps_independent_open_and_details_targets() {
        let home = home_with_cubes();
        let projection = launcher_catalog::project(&home);
        let key = CubeCatalogKey {
            chain: ChainId::Bitcoin,
            cube_id: "second".into(),
        };
        let mounted = capture::inspect(
            "v113-launcher-wide-table",
            list_layout(&projection, &project_catalog(&projection), 1400.0, true),
            (2000, 900),
            "light",
            theme::Theme::light(),
        );
        assert!(mounted.contains("Network"));
        assert!(mounted.contains("Wallets"));
        assert!(mounted.has_focus(launcher_catalog::select_id(&key)));
        assert!(mounted.has_focus(launcher_catalog::open_id(&key)));
        assert!(mounted.has_focus(launcher_catalog::details_open_id(&key)));
    }

    #[test]
    fn mounted_opening_keeps_real_loader_status_and_scan_guidance() {
        let quote = Quote {
            id: "fixture".into(),
            text: "A source quote".into(),
            author: "Author".into(),
            source: "Source".into(),
            category: "loading".into(),
            length: "short".into(),
            contexts: Vec::new(),
        };
        let (bytes, _) =
            coincube_ui::component::quote_display::default_image_for_context("loading");
        let image = iced::widget::image::Handle::from_bytes(bytes);
        for (step, expected) in [
            (loader::Step::Connecting, "Connecting to daemon..."),
            (
                loader::Step::FullScan { progress: 0.5 },
                "Scanning the blockchain...",
            ),
        ] {
            let scene = project_opening("Example\n\u{202e} Wallet", ChainId::Bitcoin);
            for (appearance, palette) in [
                ("dark", theme::Theme::dark()),
                ("light", theme::Theme::light()),
            ] {
                let mounted = capture::inspect(
                    "v113-opening",
                    opening_body(scene.clone(), &step, &quote, &image, false),
                    (1024, 768),
                    appearance,
                    palette,
                );
                assert!(mounted.contains("Tenshu"));
                assert!(mounted.contains("Opening Example Wallet"));
                assert!(mounted.contains("Bitcoin"));
                assert!(mounted.contains(expected));
                assert!(mounted.contains("A source quote"));
                assert!(!mounted.contains("Checks passed"));
                if matches!(step, loader::Step::FullScan { .. }) {
                    assert!(mounted.contains("Performing an initial scan"));
                }
            }
        }
    }

    #[test]
    fn mounted_opening_wraps_long_latin_and_cjk_names_at_compact_width() {
        let quote = Quote {
            id: "fixture".into(),
            text: "A source quote".into(),
            author: "Author".into(),
            source: "Source".into(),
            category: "loading".into(),
            length: "short".into(),
            contexts: Vec::new(),
        };
        let (bytes, _) =
            coincube_ui::component::quote_display::default_image_for_context("loading");
        let image = iced::widget::image::Handle::from_bytes(bytes);
        let step = loader::Step::Connecting;
        for raw_name in ["W".repeat(80), "旅".repeat(80)] {
            let scene = project_opening(&raw_name, ChainId::Bitcoin);
            for (appearance, palette) in [
                ("dark", theme::Theme::dark()),
                ("light", theme::Theme::light()),
            ] {
                // `inspect` mounts the actual element and asserts every text
                // bound stays inside its compact viewport.
                let mounted = capture::inspect(
                    "v113-opening-long-name",
                    opening_body(scene.clone(), &step, &quote, &image, false),
                    (1024, 768),
                    appearance,
                    palette,
                );
                assert!(mounted.contains("Opening"));
                assert!(mounted.contains("Connecting to daemon..."));
                assert!(mounted.contains("A source quote"));
            }
        }
    }

    #[test]
    fn catalog_projection_keeps_unfiltered_counts() {
        let catalog = Projection {
            locals: Vec::new(),
            remotes: Vec::new(),
            local_total: 3,
            remote_total: 2,
            selected: None,
            query: "hidden".into(),
            network: ChainId::Bitcoin,
            total_count: 5,
            cube_limit: 10,
            tier: "Free",
            has_local_list: true,
        };
        let scene = project_catalog(&catalog);
        assert_eq!((scene.local_total, scene.remote_total), (3, 2));
        assert_eq!(scene.query, "hidden");
        assert!(scene.selected.is_none());
    }
}
