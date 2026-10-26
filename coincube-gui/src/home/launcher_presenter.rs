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
    launcher_coin, launcher_gpu, launcher_scene, CubeCatalogKey, DeleteCubeMessage, Home,
    ViewMessage,
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
pub(super) enum WalletFaceMark {
    Vault,
    ImportedBitcoin,
    None,
}

pub(super) fn wallet_face_mark(cube: &crate::app::settings::CubeSettings) -> WalletFaceMark {
    if cube.vault_wallet_id.is_some() {
        WalletFaceMark::Vault
    } else if cube.has_imported_bitcoin() {
        // An import operation may exist before its wallet is installed.
        WalletFaceMark::ImportedBitcoin
    } else {
        WalletFaceMark::None
    }
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
    catalog_avatar_any(name)
}

fn catalog_avatar_any<M: 'static>(name: &str) -> Container<'static, M> {
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

/// `.hit`: the label and hit area laid over each Cube. The prototype draws no
/// box on hover or selection; the Cube itself moves.
fn floor_style() -> impl Fn(&theme::Theme, iced_button::Status) -> iced_button::Style {
    |theme, _status| iced_button::Style {
        background: None,
        text_color: theme.colors.shell.ink,
        ..Default::default()
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

/// `.cb`: a fixed slot with the name and state at its foot and a hit area
/// over the whole Cube. The 3D Cube itself is painted by the GPU stage below.
fn floor_slot<'a>(
    row: &launcher_catalog::LocalRow<'_>,
    layout: &launcher_scene::Layout,
    active: bool,
    dimmed: bool,
) -> Element<'a, ViewMessage> {
    let name = tenshu::label(row.name.clone()).wrapping(Wrapping::WordOrGlyph);
    // `.cb.dim .nm { color: var(--muted) }`: labels stay legible, only muted.
    let name = if dimmed {
        name.style(text_tone(tenshu::Tone::Muted))
    } else {
        name
    };
    let label =
        Container::new(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(catalog_avatar(&row.name))
                .push(Column::new().spacing(2).push(name).push(tenshu::caption(
                    if row.actionable {
                        unlock_prompt(row.cube)
                    } else {
                        "Unavailable on this network"
                    },
                ))),
        )
        .width(Length::Fill)
        .center_x(Length::Fill);
    // `.cb.active .lbl { visibility: hidden }`.
    let content: Element<'a, ViewMessage> = if active {
        Space::new().width(Length::Fill).height(Length::Fill).into()
    } else {
        Column::new()
            .width(Length::Fill)
            .height(Length::Fill)
            .push(Space::new().height(Length::Fill))
            .push(label)
            .into()
    };
    Action::new(launcher_catalog::select_id(&row.key), content, focus)
        .width(Length::Fixed(layout.slot()))
        .height(Length::Fixed(layout.slot_height()))
        .padding(iced::Padding {
            top: 0.0,
            right: 4.0,
            bottom: 10.0,
            left: 4.0,
        })
        .style(floor_style())
        .on_press_maybe(
            row.actionable
                .then(|| ViewMessage::Catalog(CatalogMessage::SelectFloor(row.key.clone()))),
        )
        .into()
}

fn face_text<'r>(row: &'r launcher_catalog::LocalRow<'_>) -> launcher_scene::FaceText<'r> {
    launcher_scene::FaceText {
        name: &row.name,
        backup: backup(row.cube).0,
        connect: &row.connect.1,
        wallets: local_wallets(row.cube),
        passkey: row.cube.is_passkey_cube(),
    }
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

fn stage_height(width: f32) -> f32 {
    if width < 1200.0 {
        600.0
    } else {
        launcher_scene::DESIGN_STAGE_HEIGHT
    }
}

fn floor_layout<'a>(
    projection: &Projection<'_>,
    page: usize,
    width: f32,
    light: bool,
    motion: Option<&launcher_scene::Motion>,
) -> Element<'a, ViewMessage> {
    debug_assert!(!projection.locals.is_empty());
    let stage_height = stage_height(width);
    let layout = launcher_scene::Layout::new(width, stage_height);
    let pages = projection
        .locals
        .len()
        .div_ceil(launcher_catalog::FLOOR_PAGE_SIZE)
        .max(1);
    let page = page.min(pages - 1);
    let selected = projection
        .selected
        .and_then(|index| projection.locals.get(index));
    let selected_key = selected.map(|row| &row.key);
    let visible: Vec<_> = projection
        .locals
        .iter()
        .skip(page * launcher_catalog::FLOOR_PAGE_SIZE)
        .take(launcher_catalog::FLOOR_PAGE_SIZE)
        .collect();
    let now = std::time::Instant::now();
    let pose = |key: &CubeCatalogKey| match motion {
        Some(motion) => motion.pose(key, selected_key, now),
        None => launcher_scene::settled_pose(key, selected_key),
    };
    // Paint order follows the DOM and z-index: peers, `.cb.prev`, `.cb.lead`.
    let previous = motion.and_then(|m| m.previous().or(m.returning_key()));
    let rank = |key: &CubeCatalogKey| {
        if Some(key) == selected_key {
            2
        } else if Some(key) == previous {
            1
        } else {
            0
        }
    };
    let mut order: Vec<usize> = (0..visible.len()).collect();
    order.sort_by_key(|index| rank(&visible[*index].key));
    let count = visible.len();
    let cubes = order.iter().map(|index| {
        let row = visible[*index];
        launcher_gpu::StageCube {
            rest: layout.rest_center(*index, count),
            pose: pose(&row.key),
            marks: launcher_gpu::marks(row.cube),
            details: Some(face_text(row)),
            unlock_name: None,
        }
    });
    let mut stage = Stack::new()
        .width(Length::Fill)
        .height(Length::Fixed(stage_height))
        .push(
            Container::new(launcher_gpu::stage(launcher_gpu::StageData::new(
                &layout, light, true, cubes,
            )))
            .width(Length::Fill)
            .center_x(Length::Fill),
        );

    // `.cb.lead` and `.cb.prev` paint over their neighbours' labels.
    let covers: Vec<_> = visible
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            let pose = pose(&row.key);
            (pose.spin > 0.0)
                .then(|| launcher_scene::bounds(&layout, layout.rest_center(index, count), pose))
        })
        .collect();
    let mut slots = Row::new().spacing(layout.gap()).align_y(Alignment::Start);
    for (index, row) in visible.iter().enumerate() {
        let pose = pose(&row.key);
        let covered = covers.iter().any(|cover| {
            launcher_scene::overlaps(*cover, launcher_scene::label_rect(&layout, index, count))
        });
        slots = slots.push(floor_slot(
            row,
            &layout,
            pose.spin > 0.0 || covered,
            pose.dim > 0.5,
        ));
    }
    stage = stage.push(
        Column::new()
            .width(Length::Fill)
            .push(Space::new().height(Length::Fixed(layout.rows_top())))
            .push(
                Container::new(slots)
                    .width(Length::Fill)
                    .center_x(Length::Fill),
            ),
    );

    if let Some(row) = selected {
        // The selected Cube becomes the centered face. Its controls arrive
        // once it is square to the camera and remain real Iced widgets.
        if pose(&row.key) == launcher_scene::Pose::DETAILS {
            let face = layout.face();
            let (_, target_y) = layout.target();
            stage = stage.push(
                Column::new()
                    .width(Length::Fill)
                    .push(Space::new().height(Length::Fixed(target_y - face / 2.0)))
                    .push(
                        // Clicks on the face never reach the Cubes behind it.
                        Container::new(iced::widget::opaque(floor_details(row, light)))
                            .width(Length::Fill)
                            .center_x(Length::Fill),
                    ),
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
                    .width(Length::Fixed(if width < 1200.0 {
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

/// A lone Cube turning from its details face to the unlock face, for the
/// unlock screens that have no Launcher page behind them.
pub(crate) fn unlock_turn<'a, M: 'a>(
    cube: &crate::app::settings::CubeSettings,
    mode: coincube_ui::theme::palette::ThemeMode,
    progress: f32,
) -> Element<'a, M> {
    lone_cube(cube, mode, launcher_scene::Pose::unlock(progress))
}

fn lone_cube<'a, M: 'a>(
    cube: &crate::app::settings::CubeSettings,
    mode: coincube_ui::theme::palette::ThemeMode,
    pose: launcher_scene::Pose,
) -> Element<'a, M> {
    let light = matches!(mode, coincube_ui::theme::palette::ThemeMode::Light);
    let cube = cube.clone();
    responsive(move |size: Size| {
        let layout = launcher_scene::Layout::centered(size.width, size.height);
        let name = crate::cube_display::name(&cube.name);
        launcher_gpu::stage(launcher_gpu::StageData::new(
            &layout,
            light,
            false,
            [launcher_gpu::StageCube {
                rest: layout.target(),
                pose,
                marks: launcher_gpu::marks(&cube),
                details: Some(launcher_scene::FaceText {
                    name: &name,
                    backup: backup(&cube).0,
                    connect: "Checking…",
                    wallets: local_wallets(&cube),
                    passkey: cube.is_passkey_cube(),
                }),
                unlock_name: Some(&name),
            }],
        ))
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

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
/// The resting unlock face shared by the real PIN and passkey screens. Their
/// existing widgets and messages stay inside `controls`; this only supplies
/// the Launcher stage around them.
pub(crate) fn unlock_face<'a, M: 'a>(
    cube: &crate::app::settings::CubeSettings,
    mode: coincube_ui::theme::palette::ThemeMode,
    controls: Element<'a, M>,
) -> Element<'a, M> {
    // The tab hands off to the established PIN/passkey state immediately.
    // Render that state's live widgets on the second (270°) square face.
    Stack::new()
        .width(Length::Fill)
        .height(Length::Fill)
        .push(lone_cube(cube, mode, launcher_scene::Pose::unlock(1.0)))
        .push(
            Container::new(unlock_controls(cube, controls))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill),
        )
        .into()
}

/// The live PIN or passkey controls laid out inside the 420px unlock face.
fn unlock_controls<'a, M: 'a>(
    cube: &crate::app::settings::CubeSettings,
    controls: Element<'a, M>,
) -> Element<'a, M> {
    Container::new(
        Column::new()
            .spacing(12)
            .push(tenshu::caption(format!(
                "Open {} · {}",
                crate::cube_display::name(&cube.name),
                cube.network.label()
            )))
            .push(controls),
    )
    .width(Length::Fixed(launcher_scene::FACE_SIZE))
    .height(Length::Fixed(launcher_scene::FACE_SIZE))
    .padding(28)
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
    let stage_height = stage_height(width);
    let layout = launcher_scene::Layout::new(width, stage_height);
    let count = context.peers.len();
    let name = crate::cube_display::name(&cube.name);
    let selected_pose = launcher_scene::Pose::unlock(progress.unwrap_or(1.0));
    // Peers first, the turning Cube last, as on the Launcher page.
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by_key(|index| *index == context.selected_slot);
    let cubes = order.into_iter().map(|index| {
        let peer = &context.peers[index];
        let selected = index == context.selected_slot && peer.key == context.selected;
        launcher_gpu::StageCube {
            rest: layout.rest_center(index, count),
            pose: if selected {
                selected_pose
            } else {
                launcher_scene::Pose::DIMMED
            },
            marks: peer.marks,
            details: selected.then_some(launcher_scene::FaceText {
                name: &name,
                backup: context.backup,
                connect: &context.connect,
                wallets: context.wallets,
                passkey: cube.is_passkey_cube(),
            }),
            unlock_name: selected.then_some(name.as_str()),
        }
    });
    let mut stage = Stack::new()
        .width(Length::Fill)
        .height(Length::Fixed(stage_height))
        .push(
            Container::new(launcher_gpu::stage(launcher_gpu::StageData::new(
                &layout, light, true, cubes,
            )))
            .width(Length::Fill)
            .center_x(Length::Fill),
        );
    let mut labels = Row::new().spacing(layout.gap()).align_y(Alignment::Start);
    for (index, peer) in context.peers.iter().enumerate() {
        let selected = index == context.selected_slot && peer.key == context.selected;
        let label: Element<'a, M> = if selected {
            Space::new().into()
        } else {
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(catalog_avatar_any(&peer.name))
                .push(
                    Column::new()
                        .spacing(2)
                        .push(
                            tenshu::label(peer.name.clone())
                                .wrapping(Wrapping::WordOrGlyph)
                                .style(text_tone(tenshu::Tone::Muted)),
                        )
                        .push(tenshu::caption(peer.status)),
                )
                .into()
        };
        let covered = launcher_scene::overlaps(
            launcher_scene::bounds(
                &layout,
                layout.rest_center(context.selected_slot, count),
                selected_pose,
            ),
            launcher_scene::label_rect(&layout, index, count),
        );
        labels = labels.push(
            Container::new(if covered { Space::new().into() } else { label })
                .width(Length::Fixed(layout.slot()))
                .height(Length::Fixed(layout.slot_height()))
                .padding(iced::Padding {
                    top: 0.0,
                    right: 4.0,
                    bottom: 10.0,
                    left: 4.0,
                })
                .align_x(iced::alignment::Horizontal::Center)
                .align_y(iced::alignment::Vertical::Bottom),
        );
    }
    stage = stage.push(
        Column::new()
            .width(Length::Fill)
            .push(Space::new().height(Length::Fixed(layout.rows_top())))
            .push(
                Container::new(labels)
                    .width(Length::Fill)
                    .center_x(Length::Fill),
            ),
    );
    if let (None, Some(controls)) = (progress, controls) {
        let face = layout.face();
        let (_, target_y) = layout.target();
        stage = stage.push(
            Column::new()
                .width(Length::Fill)
                .push(Space::new().height(Length::Fixed(target_y - face / 2.0)))
                .push(
                    Container::new(iced::widget::opaque(unlock_controls(cube, controls)))
                        .width(Length::Fill)
                        .center_x(Length::Fill),
                ),
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
    let stage = Container::new(loader::view_with_retry(
        step,
        quote,
        image,
        !restricted_claim,
    ))
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
                    marks: launcher_gpu::marks(cube),
                },
                launcher_catalog::LauncherScenePeer {
                    key: peer_key,
                    name: "Travel".into(),
                    status: "PIN prompt",
                    marks: launcher_gpu::marks(&peer),
                },
                // The 420px face covers the two middle labels, as the
                // prototype's lead Cube does; the last peer stays readable.
                launcher_catalog::LauncherScenePeer {
                    key: CubeCatalogKey {
                        chain: cube.network,
                        cube_id: "studio".into(),
                    },
                    name: "Studio".into(),
                    status: "PIN prompt",
                    marks: launcher_gpu::marks(&peer),
                },
                launcher_catalog::LauncherScenePeer {
                    key: CubeCatalogKey {
                        chain: cube.network,
                        cube_id: "lab".into(),
                    },
                    name: "Lab".into(),
                    status: "PIN prompt",
                    marks: launcher_gpu::marks(&peer),
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
            assert!(mounted.contains("Lab"));
            assert!(!mounted.contains("Travel"), "label under the face");
            assert!(mounted.contains("PIN prompt"));
            assert!(mounted.contains("Open Everyday"));
            assert!(mounted.has_focus(crate::pin_input::first_digit_id()));
            let face = mounted
                .text
                .iter()
                .find(|(value, _)| value.starts_with("Open Everyday"))
                .map(|(_, bounds)| *bounds)
                .expect("selected unlock face caption");
            // Face top = 64px bar + target (392px of a 742px stage) - 210.
            let expected = if width == 1024 { 199.0 } else { 274.0 };
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
        use launcher_scene::{rest_face_svg, Face};
        let mut cube = crate::app::settings::CubeSettings::new_with_raw_id(
            "fixture".into(),
            "Fixture".into(),
            ChainId::Bitcoin,
        );
        let lock = "M7 11h10v9H7z";
        let check = "M8.5 12.2l2.4 2.4 4.6-5.1";
        let unknown_backup = rest_face_svg(Face::Right, launcher_gpu::marks(&cube), false).unwrap();
        assert_eq!(unlock_prompt(&cube), "PIN prompt");
        assert!(unknown_backup.contains(lock));
        assert!(!unknown_backup.contains(check));

        cube.backed_up = true;
        let recorded_backup = rest_face_svg(Face::Right, launcher_gpu::marks(&cube), true).unwrap();
        assert!(recorded_backup.contains(check));
        assert!(recorded_backup.contains("#202020"), "light ink");

        cube.passkey_metadata = Some(crate::app::settings::PasskeyMetadata {
            credential_id: "fixture".into(),
            rp_id: "example.test".into(),
            created_at: 0,
            label: None,
        });
        let passkey = rest_face_svg(Face::Right, launcher_gpu::marks(&cube), false).unwrap();
        assert_eq!(unlock_prompt(&cube), "Passkey prompt");
        assert!(!passkey.contains(lock));
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
        if let super::super::State::Cubes { cubes, .. } = &mut home.state {
            for name in ["Studio", "Lab"] {
                cubes.push(crate::app::settings::CubeSettings::new_with_raw_id(
                    name.to_lowercase(),
                    name.into(),
                    ChainId::Bitcoin,
                ));
            }
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
        use launcher_scene::{rest_face_svg, Face};
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
        let marks = launcher_gpu::marks(&cube);
        assert!(marks.vault && marks.backup_confirmed && !marks.passkey);
        for light in [false, true] {
            let top = rest_face_svg(Face::Top, marks, light).unwrap();
            assert!(
                top.contains("data:image/png;base64,"),
                "Bitcoin coin missing"
            );
            assert!(rest_face_svg(Face::Left, marks, light)
                .unwrap()
                .contains("M12 3l7 3v5.5"));
            assert!(rest_face_svg(Face::Right, marks, light)
                .unwrap()
                .contains("M8.5 12.2l2.4"));
            // The same marks paint the resting floor and every motion frame:
            // the stage reads them from one persisted record.
            assert_eq!(
                launcher_scene::edge(cube.network, light)[..3],
                [243, 130, 29]
            );
        }
        cube.network = ChainId::Testnet4;
        let testnet = launcher_gpu::marks(&cube);
        assert_eq!(
            launcher_scene::edge(testnet.network, false)[..3],
            [185, 187, 181]
        );
        assert!(rest_face_svg(Face::Top, testnet, false)
            .unwrap()
            .contains("stroke-dasharray=\"2.5 2.5\""));
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
