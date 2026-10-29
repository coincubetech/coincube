//! Device-wide App Preferences sheet. The GUI supplies the last accepted
//! snapshot, observed platform inputs, and real diagnostic capabilities.
//! This leaf emits typed intents; it never persists or changes a preference.
//!
//! Geometry follows the App preferences artboard
//! (`design-review/canvas/P-AppPrefs.dc.html`): the 1040px Connect sheet
//! surface, a header over a rule, a 232px section list with its own rule, and
//! content inset 28/40px with a 700px reading width. Every row carries the
//! "This device" tag in its own trailing column.

use coincube_ui::{
    component::tenshu::{
        segment, segmented, setting_row, text as tenshu_text, toggle, ActionButton, ActionKind,
        SegmentSize, Tone,
    },
    font,
    icon::*,
    theme,
    widget::{action::Action as FocusableAction, Column, Element, Row},
};
use iced::{advanced::widget::Id, widget::container, Alignment, Length};

use crate::app::{
    settings::preferences::{
        DevicePreferences, EffectivePreferences, LauncherStyle, MotionPreference,
        NetworkActivitySource, PreferenceChange, ThemePreference,
    },
    state::preferences::{BalancePrivacy, PreferencesPageState, Section},
};
use coincube_ui::theme::palette::ThemeMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Close,
    RetryLoad,
    OpenLogs,
    CopySupportSummary,
    RepairRegistration,
    OpenDownloadStats,
    OpenLicenses,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    FocusAction(Id),
    SelectSection(Section),
    Change(PreferenceChange),
    ToggleComponentVersions,
    ToggleRegistrationRepair,
    Action(Action),
}

/// Values are populated from real owners. Missing fields render as unavailable;
/// neither a stream connection nor a repair result is inferred by this view.
#[derive(Debug, Default, Clone, Copy)]
pub struct Diagnostics<'a> {
    pub stream_status: Option<&'a str>,
    pub registration_status: Option<&'a str>,
    pub can_repair_registration: bool,
    pub can_open_logs: bool,
    /// Already sanitized by the caller's explicit support-summary allowlist;
    /// displayed for inspection before the user chooses Copy.
    pub support_summary_preview: Option<&'a str>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct About<'a> {
    pub build_provenance: Option<&'a str>,
    pub daemon_version: Option<&'a str>,
    pub spark_bridge_version: Option<&'a str>,
    pub can_open_download_stats: bool,
    pub can_open_licenses: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ViewModel<'a> {
    /// None means no accepted load yet. Controls stay inert and balances must
    /// remain suppressed by the GUI until an accepted snapshot is available.
    pub saved: Option<DevicePreferences>,
    pub effective: Option<EffectivePreferences>,
    pub system_theme: Option<ThemeMode>,
    pub system_reduces_motion: Option<bool>,
    pub cubes_renderer_available: bool,
    pub saving: bool,
    /// Effective session privacy from the GUI owner. Never infer it from
    /// `saved.hide_balances`: a local Hide can mask before its save succeeds.
    pub balance_privacy: BalancePrivacy,
    /// Mirrors `BalancePrivacyChrome::interactive`: explicit saves disable
    /// controls; a periodic read keeps them usable and queues a click.
    pub balance_privacy_interactive: bool,
    /// The last save committed but its durability could not be confirmed.
    /// It is applied, not rolled back; the notice is neutral.
    pub durability_unconfirmed: bool,
    /// True while an initial load or explicit retry is in flight.
    pub loading: bool,
    pub load_error: Option<&'a str>,
    pub save_error: Option<&'a str>,
    pub diagnostics: Diagnostics<'a>,
    pub about: About<'a>,
}

pub const CONTENT_SCROLL_ID: &str = "tenshu.preferences.content-scroll";

/// Where every App preferences setting is stored (`.scope`).
const SCOPE: &str = "This device";
/// Width of the trailing tag column (`.row` third column).
const TAG_COLUMN: f32 = 92.0;
/// Reading width of a section (`.sec`).
const READING_WIDTH: f32 = 700.0;

pub fn view<'a>(page: &'a PreferencesPageState, model: ViewModel<'a>) -> Element<'a, Message> {
    // `.shead`: 24px title over a 13px muted line, the 36px Close on the right.
    let header = container(
        Row::new()
            .push(
                Column::new()
                    .push(
                        iced::widget::text("App preferences")
                            .size(24)
                            .line_height(iced::widget::text::LineHeight::Relative(1.25))
                            .font(font::SEMIBOLD)
                            .style(theme::tenshu::text::ink),
                    )
                    .push(
                        tenshu_text::body(
                            "These settings apply to this device, not to any Cube or account.",
                        )
                        .size(13)
                        .style(theme::tenshu::text::muted),
                    )
                    .spacing(2)
                    .width(Length::Fill),
            )
            .push(
                FocusableAction::new(
                    "tenshu.preferences.close",
                    centred(
                        Row::new()
                            .push(cross_icon().size(14).style(theme::tenshu::text::ink))
                            .push(tenshu_text::label("Close"))
                            .spacing(6)
                            .align_y(Alignment::Center),
                    ),
                    Message::FocusAction,
                )
                .height(Length::Fixed(36.0))
                .padding(iced::Padding {
                    left: 10.0,
                    right: 12.0,
                    ..iced::Padding::ZERO
                })
                .style(|theme, status| {
                    theme::tenshu::button::action(
                        theme,
                        status,
                        ActionKind::Secondary,
                        theme::tenshu::button::ActionShape::Compact,
                    )
                })
                .on_press(Message::Action(Action::Close)),
            )
            .spacing(16)
            .align_y(Alignment::Center),
    )
    .padding(iced::Padding {
        top: 18.0,
        right: 24.0,
        bottom: 18.0,
        left: 28.0,
    })
    .width(Length::Fill);

    // `.nav`: the six sections, then the note pinned to the bottom.
    let sections = Section::ALL.map(|section| {
        let icon = match section {
            Section::Appearance => circle_half_icon(),
            Section::Launcher => cube_outline_icon(),
            Section::BalancesAndMotion => eye_outline_icon(),
            Section::Developer => code_slash_icon(),
            Section::Diagnostics => activity_icon(),
            Section::About => tooltip_icon(),
        };
        segment(section.action_id(), section, section.label()).icon(icon)
    });
    let navigation = Column::new()
        .push(
            coincube_ui::component::tenshu::nav_list(
                sections,
                Some(page.section),
                Message::FocusAction,
            )
            .on_select(Message::SelectSection),
        )
        .push(iced::widget::Space::new().height(Length::Fill))
        .push(
            container(tenshu_text::caption(
                "Cube settings stay inside each Cube. Account services are under Connect.",
            ))
            .padding([0, 12]),
        )
        .height(Length::Fill);

    let body = Row::new()
        .push(
            container(navigation)
                .padding([16, 12])
                .width(Length::Fixed(232.0))
                .height(Length::Fill),
        )
        .push(vertical_rule())
        .push(
            iced::widget::scrollable(
                container(container(section_content(page, model)).max_width(READING_WIDTH))
                    .padding(iced::Padding {
                        top: 28.0,
                        right: 40.0,
                        bottom: 40.0,
                        left: 40.0,
                    })
                    .width(Length::Fill),
            )
            .id(CONTENT_SCROLL_ID)
            .width(Length::Fill)
            .height(Length::Fill),
        )
        .height(Length::Fill);

    container(
        Column::new()
            .push(header)
            .push(horizontal_rule())
            .push(body)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .clip(true)
    .style(sheet_style)
    .into()
}

fn section_content<'a>(
    page: &'a PreferencesPageState,
    model: ViewModel<'a>,
) -> Element<'a, Message> {
    let mut content = Column::new()
        .push(
            container(tenshu_text::title(page.section.label()))
                .padding(iced::Padding::default().bottom(16)),
        )
        .width(Length::Fill);
    let mut notices: Vec<(String, Tone)> = Vec::new();
    let mut notice = |line: String, tone: Tone| notices.push((line, tone));
    if let Some(error) = model.load_error {
        notice(
            format!("Device preferences could not be loaded: {error}"),
            Tone::Bad,
        );
    }
    if let Some(error) = model.save_error {
        let status = if model.balance_privacy == BalancePrivacy::HiddenLocally {
            "Could not save. Amounts remain hidden in this session. Retry Hide to save that choice."
        } else {
            "Could not save. Your previous settings remain active."
        };
        notice(format!("{status} {error}"), Tone::Bad);
    }
    if model.saving {
        notice("Saving device preferences…".to_string(), Tone::Muted);
    }
    if model.durability_unconfirmed {
        notice(
            "Saved. Durability could not be confirmed.".to_string(),
            Tone::Muted,
        );
    }
    let idle = !model.saving && !model.loading;
    let Some(saved) = model.saved else {
        let status = if model.load_error.is_some() {
            "Device preferences are unavailable. Retry the load before changing settings. \
             Amounts stay hidden until you choose."
        } else {
            "Loading device preferences. Amounts remain hidden until loading succeeds."
        };
        notice(status.to_string(), Tone::Muted);
        content = content.push(notice_column(notices));
        if model.load_error.is_some() {
            content = content.push(
                Column::new()
                    .push(
                        ActionButton::new(
                            "tenshu.preferences.retry-load",
                            "Retry load",
                            ActionKind::Primary,
                            Message::FocusAction,
                        )
                        .compact()
                        .on_press_maybe(idle.then_some(Message::Action(Action::RetryLoad))),
                    )
                    .push(explicit_privacy_choice(
                        idle && model.balance_privacy_interactive,
                    ))
                    .spacing(12)
                    .padding(iced::Padding::default().top(12)),
            );
        }
        return content.into();
    };
    if !notices.is_empty() {
        content = content
            .push(container(notice_column(notices)).padding(iced::Padding::default().bottom(16)));
    }
    let enabled = idle;
    let mut rows: Vec<Element<'a, Message>> = Vec::new();
    match page.section {
        Section::Appearance => {
            let current = match model.system_theme {
                Some(ThemeMode::Light) => "System is currently: Light",
                Some(ThemeMode::Dark) => "System is currently: Dark",
                None => "System theme unavailable; System uses a dark fallback",
            };
            rows.push(
                setting_row(
                    "Theme",
                    segmented(
                        [
                            segment(
                                "tenshu.preferences.theme.system",
                                ThemePreference::System,
                                "System",
                            )
                            .icon(display_icon()),
                            segment(
                                "tenshu.preferences.theme.light",
                                ThemePreference::Light,
                                "Light",
                            )
                            .icon(sun_icon()),
                            segment(
                                "tenshu.preferences.theme.dark",
                                ThemePreference::Dark,
                                "Dark",
                            )
                            .icon(moon_icon()),
                        ],
                        Some(saved.theme),
                        Message::FocusAction,
                    )
                    .size(SegmentSize::Compact)
                    .on_select_maybe(enabled.then_some(
                        |value: ThemePreference| Message::Change(PreferenceChange::Theme(value)),
                    )),
                )
                .tag(SCOPE)
                .trailing_tag()
                .description("Colors for the Launcher and every Cube on this device.")
                .note(current, Tone::Muted)
                .into(),
            );
        }
        Section::Launcher => {
            let status = if !model.cubes_renderer_available
                && saved.launcher_style == LauncherStyle::Cubes
            {
                "Showing List now: the Cubes view is unavailable."
            } else if model
                .effective
                .is_some_and(|value| value.launcher_style == LauncherStyle::List)
                && saved.launcher_style == LauncherStyle::Cubes
            {
                "Showing List now: reduced motion is on."
            } else if saved.launcher_style == LauncherStyle::List {
                "Showing List."
            } else {
                "Showing Cubes."
            };
            rows.push(
                setting_row(
                    "Launcher style",
                    segmented(
                        [
                            segment(
                                "tenshu.preferences.launcher.cubes",
                                LauncherStyle::Cubes,
                                "Cubes",
                            )
                            .icon(cube_outline_icon()),
                            segment(
                                "tenshu.preferences.launcher.list",
                                LauncherStyle::List,
                                "List",
                            )
                            .icon(list_ul_icon()),
                        ],
                        Some(saved.launcher_style),
                        Message::FocusAction,
                    )
                    .size(SegmentSize::Compact)
                    .on_select_maybe(enabled.then_some(
                        |value: LauncherStyle| {
                            Message::Change(PreferenceChange::LauncherStyle(value))
                        },
                    )),
                )
                .tag(SCOPE)
                .trailing_tag()
                .description(
                    "List is used automatically under reduced motion or when 3D rendering is \
                     unavailable.",
                )
                .note(status, Tone::Muted)
                .into(),
            );
            rows.push(
                setting_row(
                    "Network activity",
                    toggle(
                        "tenshu.preferences.launcher.network-activity",
                        saved.network_activity,
                        Message::FocusAction,
                    )
                    .switch()
                    .on_toggle_maybe(enabled.then_some(|shown: bool| {
                        Message::Change(PreferenceChange::NetworkActivity(shown))
                    })),
                )
                .tag(SCOPE)
                .trailing_tag()
                .description(
                    "Recent blocks, the mempool, fee rate, hashrate and the next halving behind \
                     the Cubes, for the network the Launcher is filtered to.",
                )
                .into(),
            );
            if saved.network_activity {
                rows.push(
                    setting_row(
                        "Activity source",
                        segmented(
                            [
                                segment(
                                    "tenshu.preferences.launcher.activity.automatic",
                                    NetworkActivitySource::Automatic,
                                    "Automatic",
                                ),
                                segment(
                                    "tenshu.preferences.launcher.activity.local",
                                    NetworkActivitySource::LocalNode,
                                    "Local node",
                                ),
                                segment(
                                    "tenshu.preferences.launcher.activity.connect",
                                    NetworkActivitySource::Connect,
                                    "Connect",
                                ),
                            ],
                            Some(saved.network_activity_source),
                            Message::FocusAction,
                        )
                        .size(SegmentSize::Compact)
                        .on_select_maybe(enabled.then_some(
                            |value: NetworkActivitySource| {
                                Message::Change(PreferenceChange::NetworkActivitySource(value))
                            },
                        )),
                    )
                    .tag(SCOPE)
                    .trailing_tag()
                    .description(
                        "Automatic reads a local node that is already running, else COINCUBE | \
                         Connect, or mempool.space for the test networks Connect does not \
                         serve. The Launcher never starts a node.",
                    )
                    .into(),
                );
            }
        }
        Section::BalancesAndMotion => {
            let privacy_enabled = enabled && model.balance_privacy_interactive;
            let system_status = match model.system_reduces_motion {
                Some(true) => "System is currently: Reduced motion",
                Some(false) => "System is currently: Full motion",
                None => "System motion preference unavailable",
            };
            let control: Element<'a, Message> = match model.balance_privacy {
                BalancePrivacy::Unresolved => explicit_privacy_choice(privacy_enabled),
                BalancePrivacy::HiddenLocally => retry_hide_controls(privacy_enabled),
                privacy => {
                    let change = privacy.toggle_change();
                    toggle(
                        "tenshu.preferences.hide-balances",
                        privacy.masks_amounts(),
                        Message::FocusAction,
                    )
                    .switch()
                    .on_toggle_maybe(
                        privacy_enabled.then_some(move |_: bool| Message::Change(change)),
                    )
                    .into()
                }
            };
            let mut hide = setting_row("Hide balances", control)
                .tag(SCOPE)
                .trailing_tag()
                .description(
                    "Amounts show as •••••• in every Cube. The eye in the top bar flips the same \
                     setting.",
                );
            if !model.balance_privacy_interactive {
                hide = hide.note(
                    "Balance visibility changes are temporarily unavailable.",
                    Tone::Muted,
                );
            }
            rows.push(hide.into());
            rows.push(
                setting_row(
                    "Motion",
                    segmented(
                        [
                            segment(
                                "tenshu.preferences.motion.system",
                                MotionPreference::FollowSystem,
                                "Follow system",
                            ),
                            segment(
                                "tenshu.preferences.motion.reduced",
                                MotionPreference::Reduced,
                                "Reduced",
                            ),
                        ],
                        Some(saved.motion),
                        Message::FocusAction,
                    )
                    .size(SegmentSize::Compact)
                    .on_select_maybe(enabled.then_some(
                        |value: MotionPreference| Message::Change(PreferenceChange::Motion(value)),
                    )),
                )
                .tag(SCOPE)
                .trailing_tag()
                .description(
                    "Reduced removes the Cube turn and other movement. Security checks never \
                     wait for animation.",
                )
                .note(system_status, Tone::Muted)
                .into(),
            );
        }
        Section::Developer => {
            rows.push(
                setting_row(
                    "Developer mode",
                    toggle(
                        "tenshu.preferences.developer-mode",
                        saved.developer_mode,
                        Message::FocusAction,
                    )
                    .switch()
                    .on_toggle_maybe(
                        enabled.then_some(|on: bool| {
                            Message::Change(PreferenceChange::DeveloperMode(on))
                        }),
                    ),
                )
                .tag(SCOPE)
                .trailing_tag()
                .description("Show test networks in the Cubes network dropdown.")
                .into(),
            );
            rows.push(
                setting_row(
                    "Bitcoin Blake2b",
                    toggle(
                        "tenshu.preferences.bitcoin-blake2b-beta",
                        saved.bitcoin_blake2b_beta,
                        Message::FocusAction,
                    )
                    .switch()
                    .on_toggle_maybe(enabled.then_some(|on: bool| {
                        Message::Change(PreferenceChange::BitcoinBlake2bBeta(on))
                    })),
                )
                .tag(SCOPE)
                .trailing_tag()
                .description(
                    "Show the Bitcoin Blake2b network in the Launcher, when creating Cubes and in \
                     Vault tools.",
                )
                .into(),
            );
        }
        Section::Diagnostics => {
            let diagnostics = model.diagnostics;
            let mut logs = Column::new().width(Length::Fill).push(
                setting_row(
                    "Sanitized logs",
                    Row::new()
                        .push(
                            ActionButton::new(
                                "tenshu.preferences.open-logs",
                                "Open folder",
                                ActionKind::Secondary,
                                Message::FocusAction,
                            )
                            .compact()
                            .icon(folder_icon())
                            .on_press_maybe(
                                diagnostics
                                    .can_open_logs
                                    .then_some(Message::Action(Action::OpenLogs)),
                            ),
                        )
                        .push(
                            ActionButton::new(
                                "tenshu.preferences.copy-support-summary",
                                "Copy summary",
                                ActionKind::Secondary,
                                Message::FocusAction,
                            )
                            .compact()
                            .icon(copy_icon())
                            .on_press_maybe(
                                diagnostics
                                    .support_summary_preview
                                    .map(|_| Message::Action(Action::CopySupportSummary)),
                            ),
                        )
                        .spacing(8),
                )
                .tag(SCOPE)
                .trailing_tag()
                .description(
                    "Logs are sanitized before they're written. Review a copy before you share it.",
                )
                .without_divider(),
            );
            // The allowlisted summary stays inspectable before Copy.
            if let Some(preview) = diagnostics.support_summary_preview {
                logs = logs.push(
                    container(status_text(preview, Tone::Neutral))
                        .padding([12, 16])
                        .width(Length::Fill)
                        .style(review_style),
                );
                logs = logs.push(iced::widget::Space::new().height(Length::Fixed(14.0)));
            }
            rows.push(logs.push(horizontal_rule()).into());

            let mut repair = Column::new().width(Length::Fill).push(
                setting_row(
                    "Repair device registration",
                    ActionButton::new(
                        "tenshu.preferences.repair-details",
                        "Repair…",
                        ActionKind::Secondary,
                        Message::FocusAction,
                    )
                    .compact()
                    .on_press(Message::ToggleRegistrationRepair),
                )
                .tag(SCOPE)
                .trailing_tag()
                .description("For when Connect no longer recognises this device.")
                .without_divider(),
            );
            if page.registration_repair_expanded {
                repair = repair
                    .push(
                        container(
                            Column::new()
                                .push(
                                    tenshu_text::body("Re-register this device with Connect?")
                                        .font(font::SEMIBOLD)
                                        .style(theme::tenshu::text::ink),
                                )
                                .push(
                                    Column::new()
                                        .push(bullet(
                                            "Re-registers this device with your Connect account.",
                                        ))
                                        .push(bullet("Requires signing in to Connect first."))
                                        .push(bullet("Does not change any Cube, wallet or key."))
                                        .spacing(2)
                                        .padding(iced::Padding {
                                            top: 6.0,
                                            bottom: 12.0,
                                            ..iced::Padding::ZERO
                                        }),
                                )
                                .push(
                                    Row::new()
                                        .push(
                                            ActionButton::new(
                                                "tenshu.preferences.repair-registration",
                                                "Sign in and repair",
                                                ActionKind::Primary,
                                                Message::FocusAction,
                                            )
                                            .compact()
                                            .on_press_maybe(
                                                diagnostics.can_repair_registration.then_some(
                                                    Message::Action(Action::RepairRegistration),
                                                ),
                                            ),
                                        )
                                        .push(
                                            ActionButton::new(
                                                "tenshu.preferences.repair-cancel",
                                                "Cancel",
                                                ActionKind::Secondary,
                                                Message::FocusAction,
                                            )
                                            .compact()
                                            .on_press(Message::ToggleRegistrationRepair),
                                        )
                                        .spacing(12)
                                        .align_y(Alignment::Center),
                                ),
                        )
                        .padding([14, 16])
                        .width(Length::Fill)
                        .style(review_style),
                    )
                    .push(iced::widget::Space::new().height(Length::Fixed(14.0)));
            }
            rows.push(repair.push(horizontal_rule()).into());

            let stream: Element<'a, Message> = match diagnostics.stream_status {
                Some(status) => status_text(status, Tone::Neutral).into(),
                None => status_line(dash_circle_icon(), "Unavailable", Tone::Muted),
            };
            rows.push(
                setting_row("Stream status", stream)
                    .tag(SCOPE)
                    .trailing_tag()
                    .description(
                        "Live updates from Connect to this device. Separate from account sign-in \
                         and each Cube's registration.",
                    )
                    .into(),
            );
        }
        Section::About => {
            let about = model.about;
            rows.push(
                setting_row("Tenshu version", value_text(crate::VERSION.to_string()))
                    .tag(SCOPE)
                    .trailing_tag()
                    .description("Installed on this device")
                    .into(),
            );
            rows.push(
                setting_row(
                    "Build provenance",
                    value_text(about.build_provenance.unwrap_or("Unavailable")),
                )
                .tag(SCOPE)
                .trailing_tag()
                .description("Which build this is, and whether it was reproduced")
                .into(),
            );
            rows.push(component_versions_row(page.component_versions_expanded));
            if page.component_versions_expanded {
                rows.push(sub_row(
                    "Wallet daemon",
                    about.daemon_version.unwrap_or("Not loaded"),
                ));
                rows.push(sub_row(
                    "Spark bridge",
                    about.spark_bridge_version.unwrap_or("Not loaded"),
                ));
            }
            rows.push(
                setting_row(
                    "Download Stats",
                    link_action(
                        "tenshu.preferences.open-download-stats",
                        "View Download Stats",
                        about
                            .can_open_download_stats
                            .then_some(Message::Action(Action::OpenDownloadStats)),
                    ),
                )
                .tag(SCOPE)
                .trailing_tag()
                .description("Public download counts for Tenshu releases")
                .into(),
            );
            rows.push(
                setting_row(
                    "Open-source licenses",
                    link_action(
                        "tenshu.preferences.open-licenses",
                        "View licenses",
                        about
                            .can_open_licenses
                            .then_some(Message::Action(Action::OpenLicenses)),
                    ),
                )
                .tag(SCOPE)
                .trailing_tag()
                .description("Licenses for the software Tenshu is built on")
                .into(),
            );
        }
    }
    // `.rows`: a rule above the first row; each row draws the rule under it.
    content
        .push(
            rows.into_iter()
                .fold(Column::new().push(horizontal_rule()), |list, row| {
                    list.push(row)
                }),
        )
        .into()
}

/// Component versions (`.disc` in a `.row`): the chevron and label are the
/// disclosure action; the caption and the tag sit as in any other row.
fn component_versions_row<'a>(expanded: bool) -> Element<'a, Message> {
    let chevron = if expanded {
        collapsed_icon()
    } else {
        chevron_right()
    };
    let disclosure = FocusableAction::new(
        "tenshu.preferences.component-versions",
        Row::new()
            .push(
                chevron
                    .size(16)
                    .width(Length::Fixed(16.0))
                    .style(theme::tenshu::text::ink),
            )
            .push(tenshu_text::label("Component versions"))
            .spacing(6)
            .align_y(Alignment::Center),
        Message::FocusAction,
    )
    .padding(0)
    .style(theme::tenshu::control::disclosure)
    .on_press(Message::ToggleComponentVersions);
    row_frame(
        Column::new()
            .push(disclosure)
            .push(
                tenshu_text::body("Wallet daemon and Spark bridge")
                    .size(13)
                    .style(theme::tenshu::text::muted),
            )
            .spacing(2)
            .width(Length::Fill)
            .into(),
        None,
        12.0,
        0.0,
    )
}

/// A component version under Component versions (`.row.sub`): 13px, inset
/// 20px, 10px vertical padding.
fn sub_row<'a>(label: &'static str, value: &'a str) -> Element<'a, Message> {
    row_frame(
        tenshu_text::body(label)
            .size(13)
            .style(theme::tenshu::text::ink)
            .width(Length::Fill)
            .into(),
        Some(value_text(value).into()),
        10.0,
        20.0,
    )
}

/// `.row` built by hand for rows `setting_row` cannot express: content,
/// optional control, the trailing tag, then the rule.
fn row_frame<'a>(
    text: Element<'a, Message>,
    control: Option<Element<'a, Message>>,
    padding_y: f32,
    inset: f32,
) -> Element<'a, Message> {
    let mut row = Row::new().push(text).spacing(24).align_y(Alignment::Center);
    if let Some(control) = control {
        row = row.push(control);
    }
    row = row.push(
        container(scope_tag())
            .width(Length::Fixed(TAG_COLUMN))
            .align_x(iced::alignment::Horizontal::Right),
    );
    Column::new()
        .push(container(row).padding(iced::Padding {
            top: padding_y,
            bottom: padding_y,
            left: inset,
            right: 0.0,
        }))
        .push(horizontal_rule())
        .width(Length::Fill)
        .into()
}

fn scope_tag<'a>() -> Element<'a, Message> {
    container(tenshu_text::caption(SCOPE))
        .padding([0, 6])
        .style(theme::tenshu::control::tag)
        .into()
}

fn notice_column<'a>(notices: Vec<(String, Tone)>) -> Element<'a, Message> {
    notices
        .into_iter()
        .fold(Column::new().spacing(8), |column, (line, tone)| {
            column.push(status_text(line, tone))
        })
        .into()
}

/// A value in the control column (`.val`), 13px ink.
fn value_text<'a>(
    value: impl iced::widget::text::IntoFragment<'a>,
) -> iced::widget::Text<'a, theme::Theme> {
    tenshu_text::body(value)
        .size(13)
        .style(theme::tenshu::text::ink)
}

/// A 13px status or notice line in `tone`.
fn status_text<'a>(
    line: impl iced::widget::text::IntoFragment<'a>,
    tone: Tone,
) -> iced::widget::Text<'a, theme::Theme> {
    tenshu_text::body(line)
        .size(13)
        .style(move |t| theme::tenshu::text::tone(t, tone))
}

/// A status with its glyph (`.stt` + tone).
fn status_line<'a>(
    icon: iced::widget::Text<'a, theme::Theme>,
    label: &'a str,
    tone: Tone,
) -> Element<'a, Message> {
    Row::new()
        .push(
            icon.size(16)
                .width(Length::Fixed(16.0))
                .style(move |t| theme::tenshu::text::tone(t, tone)),
        )
        .push(status_text(label, tone))
        .spacing(6)
        .align_y(Alignment::Center)
        .into()
}

fn bullet<'a>(line: &'a str) -> Element<'a, Message> {
    Row::new()
        .push(
            tenshu_text::body("•")
                .size(13)
                .style(theme::tenshu::text::ink),
        )
        .push(
            tenshu_text::body(line)
                .size(13)
                .style(theme::tenshu::text::ink),
        )
        .spacing(8)
        .padding(iced::Padding::default().left(4))
        .into()
}

/// `.linkbtn`: ink text with an accent underline and a trailing chevron. The
/// underline is drawn by an accent copy of the label beneath the ink one.
fn link_action<'a>(
    id: &'static str,
    label: &'static str,
    message: Option<Message>,
) -> Element<'a, Message> {
    let enabled = message.is_some();
    let ink = move |theme: &theme::Theme| {
        let color = theme.colors.shell.ink;
        iced::widget::text::Style {
            color: Some(if enabled {
                color
            } else {
                theme::tenshu::faded(color, theme::tenshu::DISABLED_ALPHA)
            }),
        }
    };
    // Disabled, the label is one faded, underlined ink line; enabled, the
    // ink label is drawn over an accent copy that contributes the underline.
    let underline = iced::widget::rich_text([iced::widget::span::<(), _>(label).underline(true)])
        .size(13)
        .font(font::REGULAR)
        .wrapping(iced::widget::text::Wrapping::None)
        .style(move |theme: &theme::Theme| {
            let shell = &theme.colors.shell;
            iced::widget::text::Style {
                color: Some(if enabled {
                    shell.accent
                } else {
                    theme::tenshu::faded(shell.ink, theme::tenshu::DISABLED_ALPHA)
                }),
            }
        });
    let label: Element<'a, Message> = if enabled {
        iced::widget::stack![
            underline,
            iced::widget::text(label)
                .size(13)
                .font(font::REGULAR)
                .wrapping(iced::widget::text::Wrapping::None)
                .style(ink)
        ]
        .into()
    } else {
        underline.into()
    };
    FocusableAction::new(
        id,
        Row::new()
            .push(label)
            .push(
                chevron_right()
                    .size(14)
                    .width(Length::Fixed(14.0))
                    .style(ink),
            )
            .spacing(6)
            .align_y(Alignment::Center),
        Message::FocusAction,
    )
    .padding([2, 0])
    .style(|theme, status| {
        theme::tenshu::button::action(
            theme,
            status,
            ActionKind::Quiet,
            theme::tenshu::button::ActionShape::Compact,
        )
    })
    .on_press_maybe(message)
    .into()
}

/// Shown while no accepted privacy choice exists: amounts are masked, and the
/// user may persist an explicit choice only when the shared balance controls
/// are interactive and no load or save is in flight.
/// Neither button is a fallback default.
fn explicit_privacy_choice(enabled: bool) -> Element<'static, Message> {
    Column::new()
        .spacing(6)
        .push(status_text(
            if enabled {
                "Not set yet. Amounts stay hidden until you choose."
            } else {
                "Not set yet. Amounts stay hidden while visibility changes are unavailable."
            },
            Tone::Muted,
        ))
        .push(
            Row::new()
                .spacing(8)
                .push(
                    ActionButton::new(
                        "tenshu.preferences.balances.show",
                        "Show balances",
                        ActionKind::Secondary,
                        Message::FocusAction,
                    )
                    .compact()
                    .on_press_maybe(
                        enabled.then_some(Message::Change(PreferenceChange::HideBalances(false))),
                    ),
                )
                .push(
                    ActionButton::new(
                        "tenshu.preferences.balances.keep-hidden",
                        "Keep hidden",
                        ActionKind::Secondary,
                        Message::FocusAction,
                    )
                    .compact()
                    .on_press_maybe(
                        enabled.then_some(Message::Change(PreferenceChange::HideBalances(true))),
                    ),
                ),
        )
        .into()
}

/// Hide already masks this session, but the accepted on-disk choice still
/// says Show. Make Retry Hide the primary action; an explicit Show remains
/// available for a user who chooses to reveal again.
fn retry_hide_controls(enabled: bool) -> Element<'static, Message> {
    Column::new()
        .spacing(6)
        .push(status_text(
            "Balances are hidden now. Hide has not been saved on this device.",
            Tone::Muted,
        ))
        .push(
            Row::new()
                .spacing(8)
                .push(
                    ActionButton::new(
                        "tenshu.preferences.balances.retry-hide",
                        "Retry Hide",
                        ActionKind::Primary,
                        Message::FocusAction,
                    )
                    .compact()
                    .on_press_maybe(
                        enabled.then_some(Message::Change(PreferenceChange::HideBalances(true))),
                    ),
                )
                .push(
                    ActionButton::new(
                        "tenshu.preferences.balances.show-explicitly",
                        "Show balances",
                        ActionKind::Secondary,
                        Message::FocusAction,
                    )
                    .compact()
                    .on_press_maybe(
                        enabled.then_some(Message::Change(PreferenceChange::HideBalances(false))),
                    ),
                ),
        )
        .into()
}

/// Centres a control's content on its fixed height.
fn centred<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content).center_y(Length::Fill).into()
}

fn horizontal_rule<'a>() -> Element<'a, Message> {
    container(iced::widget::Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(theme::tenshu::control::divider)
        .into()
}

fn vertical_rule<'a>() -> Element<'a, Message> {
    container(iced::widget::Space::new())
        .width(Length::Fixed(1.0))
        .height(Length::Fill)
        .style(theme::tenshu::control::divider)
        .into()
}

/// The sheet surface (`.sheet`): panel fill, 1px line, 12px radius and the
/// drop shadow.
fn sheet_style(theme: &theme::Theme) -> container::Style {
    let shell = &theme.colors.shell;
    container::Style {
        text_color: Some(shell.ink),
        background: Some(iced::Background::Color(shell.panel)),
        border: iced::Border {
            color: shell.line,
            width: 1.0,
            radius: 12.0.into(),
        },
        shadow: iced::Shadow {
            color: shell.shadow,
            offset: iced::Vector::new(0.0, 24.0),
            blur_radius: 48.0,
        },
        ..Default::default()
    }
}

/// The repair review and summary preview (`.review`): selected fill, 8px radius.
fn review_style(theme: &theme::Theme) -> container::Style {
    container::Style {
        text_color: Some(theme.colors.shell.ink),
        background: Some(iced::Background::Color(theme.colors.shell.selected)),
        border: iced::Border {
            radius: 8.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::{
        layout,
        renderer::Headless,
        widget::{operation, operation::focusable, Operation, Tree},
        Layout,
    };
    use iced::{keyboard, mouse, Event, Rectangle, Size};

    fn renderer() -> iced::Renderer {
        for bytes in coincube_ui::font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            coincube_ui::font::REGULAR,
            14.into(),
            Some("tiny-skia"),
        ))
        .unwrap()
    }

    fn operate<T: 'static>(
        element: &mut Element<'_, Message>,
        tree: &mut Tree,
        node: &layout::Node,
        renderer: &iced::Renderer,
        operation: impl Operation<T> + 'static,
    ) -> Option<T> {
        let mut operation: Box<dyn Operation<T>> = Box::new(operation);
        loop {
            element.as_widget_mut().operate(
                tree,
                Layout::new(node),
                renderer,
                &mut operation::black_box(operation.as_mut()),
            );
            match operation.finish() {
                operation::Outcome::None => return None,
                operation::Outcome::Some(value) => return Some(value),
                operation::Outcome::Chain(next) => operation = next,
            }
        }
    }

    fn key(named: keyboard::key::Named, repeat: bool, pressed: bool) -> Event {
        let key = keyboard::Key::Named(named);
        let physical_key =
            keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified);
        let location = keyboard::Location::Standard;
        let modifiers = keyboard::Modifiers::empty();
        if pressed {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key,
                physical_key,
                location,
                modifiers,
                text: None,
                repeat,
            })
        } else {
            Event::Keyboard(keyboard::Event::KeyReleased {
                key: key.clone(),
                modified_key: key,
                physical_key,
                location,
                modifiers,
            })
        }
    }

    fn activate(
        model: ViewModel<'_>,
        section: Section,
        id: &'static str,
        named: keyboard::key::Named,
    ) -> (bool, Vec<Message>) {
        let page = PreferencesPageState {
            section,
            ..Default::default()
        };
        let mut element = view(&page, model);
        let mut tree = Tree::new(element.as_widget());
        let renderer = renderer();
        let size = Size::new(1024.0, 768.0);
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        operate(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            focusable::focus::<()>(Id::new(id)),
        );
        let focused = operate(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            focusable::find_focused(),
        ) == Some(Id::new(id));
        let mut messages = Vec::new();
        for event in [
            key(named, false, true),
            key(named, true, true),
            key(named, false, false),
        ] {
            element.as_widget_mut().update(
                &mut tree,
                &event,
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut iced::advanced::Shell::new(&mut messages),
                &Rectangle::with_size(size),
            );
        }
        (focused, messages)
    }

    #[test]
    fn rendered_boolean_controls_toggle_both_directions_without_optimistic_state() {
        for (section, id, change) in [
            (
                Section::BalancesAndMotion,
                "tenshu.preferences.hide-balances",
                PreferenceChange::HideBalances(true),
            ),
            (
                Section::Developer,
                "tenshu.preferences.developer-mode",
                PreferenceChange::DeveloperMode(true),
            ),
            (
                Section::Developer,
                "tenshu.preferences.bitcoin-blake2b-beta",
                PreferenceChange::BitcoinBlake2bBeta(true),
            ),
        ] {
            let saved = DevicePreferences::default();
            // Privacy comes from the accepted receipt, not `saved`.
            let privacy = |on: bool| match change {
                PreferenceChange::HideBalances(_) if on => BalancePrivacy::Hidden,
                PreferenceChange::HideBalances(_) => BalancePrivacy::Shown,
                _ => BalancePrivacy::Unresolved,
            };
            let (focused, messages) = activate(
                ViewModel {
                    saved: Some(saved),
                    balance_privacy: privacy(false),
                    balance_privacy_interactive: true,
                    ..Default::default()
                },
                section,
                id,
                keyboard::key::Named::Enter,
            );
            assert!(focused, "{}", id);
            assert_eq!(messages, vec![Message::Change(change)]);
            let enabled = match change {
                PreferenceChange::HideBalances(_) => DevicePreferences {
                    hide_balances: true,
                    ..saved
                },
                PreferenceChange::DeveloperMode(_) => DevicePreferences {
                    developer_mode: true,
                    ..saved
                },
                PreferenceChange::BitcoinBlake2bBeta(_) => DevicePreferences {
                    bitcoin_blake2b_beta: true,
                    ..saved
                },
                _ => unreachable!(),
            };
            let expected_off = match change {
                PreferenceChange::HideBalances(_) => PreferenceChange::HideBalances(false),
                PreferenceChange::DeveloperMode(_) => PreferenceChange::DeveloperMode(false),
                PreferenceChange::BitcoinBlake2bBeta(_) => {
                    PreferenceChange::BitcoinBlake2bBeta(false)
                }
                _ => unreachable!(),
            };
            let (focused, messages) = activate(
                ViewModel {
                    saved: Some(enabled),
                    balance_privacy: privacy(true),
                    balance_privacy_interactive: true,
                    ..Default::default()
                },
                section,
                id,
                keyboard::key::Named::Space,
            );
            assert!(focused, "{}", id);
            assert_eq!(messages, vec![Message::Change(expected_off)]);
            let (focused, messages) = activate(
                ViewModel {
                    saved: Some(enabled),
                    balance_privacy: privacy(true),
                    balance_privacy_interactive: true,
                    saving: true,
                    ..Default::default()
                },
                section,
                id,
                keyboard::key::Named::Enter,
            );
            assert!(!focused, "saving control {} was focusable", id);
            assert!(messages.is_empty());
            // A rejected save leaves the supplied accepted snapshot unchanged.
            let (_, messages) = activate(
                ViewModel {
                    saved: Some(enabled),
                    balance_privacy: privacy(true),
                    balance_privacy_interactive: true,
                    save_error: Some("write denied"),
                    ..Default::default()
                },
                section,
                id,
                keyboard::key::Named::Enter,
            );
            assert_eq!(messages, vec![Message::Change(expected_off)]);
        }
    }

    #[test]
    fn initial_load_error_has_focusable_retry_and_blocks_repeat_while_loading() {
        let model = ViewModel {
            load_error: Some("read denied"),
            ..Default::default()
        };
        let (focused, messages) = activate(
            model,
            Section::About,
            "tenshu.preferences.retry-load",
            keyboard::key::Named::Enter,
        );
        assert!(focused);
        assert_eq!(messages, vec![Message::Action(Action::RetryLoad)]);
        let (focused, messages) = activate(
            ViewModel {
                loading: true,
                ..model
            },
            Section::About,
            "tenshu.preferences.retry-load",
            keyboard::key::Named::Enter,
        );
        assert!(!focused);
        assert!(messages.is_empty());
        // An explicit Show/Keep-hidden save after the failed load is pending:
        // the retained error stays visible but Retry is not a target either.
        let (focused, messages) = activate(
            ViewModel {
                saving: true,
                ..model
            },
            Section::About,
            "tenshu.preferences.retry-load",
            keyboard::key::Named::Enter,
        );
        assert!(
            !focused,
            "Retry focusable while an explicit save is pending"
        );
        assert!(messages.is_empty(), "{:?}", messages);
        let (focused, messages) = activate(
            model,
            Section::About,
            "tenshu.preferences.retry-load",
            keyboard::key::Named::Space,
        );
        assert!(focused, "idle failure Retry must still work");
        assert_eq!(messages, vec![Message::Action(Action::RetryLoad)]);
        let (focused, messages) = activate(
            ViewModel {
                saved: Some(DevicePreferences::default()),
                load_error: None,
                ..model
            },
            Section::About,
            "tenshu.preferences.retry-load",
            keyboard::key::Named::Enter,
        );
        assert!(!focused);
        assert!(messages.is_empty());
    }

    #[test]
    fn unresolved_privacy_offers_only_an_explicit_choice_and_ignores_saved_default() {
        // An accepted snapshot whose privacy marker is unresolved: its
        // `hide_balances == false` must not render as an "Off" toggle.
        let unresolved = ViewModel {
            saved: Some(DevicePreferences::default()),
            balance_privacy: BalancePrivacy::Unresolved,
            balance_privacy_interactive: true,
            ..Default::default()
        };
        let (focused, messages) = activate(
            unresolved,
            Section::BalancesAndMotion,
            "tenshu.preferences.hide-balances",
            keyboard::key::Named::Enter,
        );
        assert!(!focused, "unresolved privacy must not render the toggle");
        assert!(messages.is_empty(), "{:?}", messages);
        for (id, hide) in [
            ("tenshu.preferences.balances.show", false),
            ("tenshu.preferences.balances.keep-hidden", true),
        ] {
            let (focused, messages) = activate(
                unresolved,
                Section::BalancesAndMotion,
                id,
                keyboard::key::Named::Enter,
            );
            assert!(focused, "{}", id);
            assert_eq!(
                messages,
                vec![Message::Change(PreferenceChange::HideBalances(hide))]
            );
            for busy in [
                ViewModel {
                    saving: true,
                    ..unresolved
                },
                ViewModel {
                    loading: true,
                    ..unresolved
                },
            ] {
                let (focused, messages) = activate(
                    busy,
                    Section::BalancesAndMotion,
                    id,
                    keyboard::key::Named::Enter,
                );
                assert!(!focused, "{} must be inert while busy", id);
                assert!(messages.is_empty(), "{:?}", messages);
            }
        }
        // No accepted receipt at all: after a load error the explicit choice
        // sits beside Retry; while loading neither is available.
        let failed = ViewModel {
            load_error: Some("Your privacy setting could not be loaded."),
            balance_privacy_interactive: true,
            ..Default::default()
        };
        let (focused, messages) = activate(
            failed,
            Section::Appearance,
            "tenshu.preferences.balances.show",
            keyboard::key::Named::Space,
        );
        assert!(focused);
        assert_eq!(
            messages,
            vec![Message::Change(PreferenceChange::HideBalances(false))]
        );
        let (focused, messages) = activate(
            ViewModel {
                loading: true,
                ..failed
            },
            Section::Appearance,
            "tenshu.preferences.balances.show",
            keyboard::key::Named::Space,
        );
        assert!(!focused);
        assert!(messages.is_empty(), "{:?}", messages);
    }

    #[test]
    fn privacy_controls_follow_shared_chrome_availability() {
        for privacy in [BalancePrivacy::Hidden, BalancePrivacy::Shown] {
            let model = ViewModel {
                saved: Some(DevicePreferences::default()),
                balance_privacy: privacy,
                // The live shell chrome has `interactive: false` until every
                // passive source amount has been checked.
                balance_privacy_interactive: false,
                ..Default::default()
            };
            let (focused, messages) = activate(
                model,
                Section::BalancesAndMotion,
                "tenshu.preferences.hide-balances",
                keyboard::key::Named::Enter,
            );
            assert!(!focused, "{:?}", privacy);
            assert!(messages.is_empty(), "{:?}: {:?}", privacy, messages);
        }

        for saved in [Some(DevicePreferences::default()), None] {
            for id in [
                "tenshu.preferences.balances.show",
                "tenshu.preferences.balances.keep-hidden",
            ] {
                let model = ViewModel {
                    saved,
                    balance_privacy: BalancePrivacy::Unresolved,
                    load_error: saved.is_none().then_some("read denied"),
                    ..Default::default()
                };
                let (focused, messages) = activate(
                    model,
                    Section::BalancesAndMotion,
                    id,
                    keyboard::key::Named::Space,
                );
                assert!(!focused, "{} with saved={:?}", id, saved);
                assert!(messages.is_empty(), "{}: {:?}", id, messages);
            }
        }
    }

    #[test]
    fn local_hide_presents_retry_and_separate_explicit_show() {
        let model = ViewModel {
            saved: Some(DevicePreferences::default()),
            balance_privacy: BalancePrivacy::HiddenLocally,
            balance_privacy_interactive: true,
            save_error: Some("write denied"),
            ..Default::default()
        };
        let (focused, messages) = activate(
            model,
            Section::BalancesAndMotion,
            "tenshu.preferences.balances.retry-hide",
            keyboard::key::Named::Enter,
        );
        assert!(focused);
        assert_eq!(
            messages,
            vec![Message::Change(PreferenceChange::HideBalances(true))]
        );
        let (focused, messages) = activate(
            model,
            Section::BalancesAndMotion,
            "tenshu.preferences.balances.show-explicitly",
            keyboard::key::Named::Enter,
        );
        assert!(focused);
        assert_eq!(
            messages,
            vec![Message::Change(PreferenceChange::HideBalances(false))]
        );
        let (focused, messages) = activate(
            ViewModel {
                saving: true,
                ..model
            },
            Section::BalancesAndMotion,
            "tenshu.preferences.balances.retry-hide",
            keyboard::key::Named::Enter,
        );
        assert!(!focused);
        assert!(messages.is_empty());

        let page = PreferencesPageState {
            section: Section::BalancesAndMotion,
            ..Default::default()
        };
        for (scenario, presented, has_error) in [
            ("failed-hide", model, true),
            (
                "reloaded-show",
                ViewModel {
                    save_error: None,
                    ..model
                },
                false,
            ),
        ] {
            let mounted = crate::app::view::shared::landing::capture::inspect(
                scenario,
                view(&page, presented),
                (1440, 900),
                "dark",
                coincube_ui::theme::Theme::dark(),
            );
            assert!(mounted.contains("Hide has not been saved on this device."));
            assert!(mounted.contains("Retry Hide"));
            assert_eq!(mounted.contains("Could not save."), has_error);
        }
    }

    #[test]
    fn close_section_and_disclosure_actions_are_keyboard_targets() {
        let model = ViewModel {
            saved: Some(DevicePreferences::default()),
            ..Default::default()
        };
        for (section, id, expected) in [
            (
                Section::Appearance,
                "tenshu.preferences.close",
                Message::Action(Action::Close),
            ),
            (
                Section::Appearance,
                Section::About.action_id(),
                Message::SelectSection(Section::About),
            ),
            (
                Section::Diagnostics,
                "tenshu.preferences.repair-details",
                Message::ToggleRegistrationRepair,
            ),
            (
                Section::About,
                "tenshu.preferences.component-versions",
                Message::ToggleComponentVersions,
            ),
        ] {
            let (focused, messages) = activate(model, section, id, keyboard::key::Named::Space);
            assert!(focused, "{}", id);
            assert_eq!(messages, vec![expected]);
        }
    }
}
