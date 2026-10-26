//! Device-wide App Preferences sheet. The GUI supplies the last accepted
//! snapshot, observed platform inputs, and real diagnostic capabilities.
//! This leaf emits typed intents; it never persists or changes a preference.

use coincube_ui::{
    component::text,
    theme,
    widget::{action::Action as FocusableAction, Column, ColumnExt, Element, Row},
};
use iced::{advanced::widget::Id, widget::container, Alignment, Length};

use crate::app::{
    settings::preferences::{
        DevicePreferences, EffectivePreferences, LauncherStyle, MotionPreference, PreferenceChange,
        ThemePreference,
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

fn focusable_button<'a>(
    id: &'static str,
    label: &'static str,
    style: fn(&theme::Theme, iced::widget::button::Status) -> iced::widget::button::Style,
    message: Option<Message>,
) -> Element<'a, Message> {
    FocusableAction::new(id, text::p2_regular(label), Message::FocusAction)
        .padding(10)
        .style(style)
        .on_press_maybe(message)
        .into()
}

pub fn view<'a>(page: &'a PreferencesPageState, model: ViewModel<'a>) -> Element<'a, Message> {
    let header: Element<'a, Message> = container(
        Row::new()
            .push(
                Column::new()
                    .spacing(4)
                    .push(text::h2("App preferences"))
                    .push(text::p2_regular(
                        "These settings apply to this device, not to a Cube or account.",
                    )),
            )
            .push(iced::widget::Space::new().width(Length::Fill))
            .push(focusable_button(
                "tenshu.preferences.close",
                "Close",
                theme::button::secondary,
                Some(Message::Action(Action::Close)),
            ))
            .align_y(Alignment::Center),
    )
    .padding(20)
    .width(Length::Fill)
    .style(theme::container::foreground)
    .into();

    let mut navigation = Column::new().spacing(6);
    for section in Section::ALL {
        let style = if section == page.section {
            theme::button::menu_pressed
        } else {
            theme::button::menu
        };
        navigation = navigation.push(focusable_button(
            section.action_id(),
            section.label(),
            style,
            Some(Message::SelectSection(section)),
        ));
    }
    navigation = navigation.push(
        text::caption("Cube settings stay in each Cube. Account services are under Connect.")
            .style(theme::text::secondary),
    );

    let content = section_content(page, model);
    let body: Element<'a, Message> = Row::new()
        .push(
            container(navigation)
                .padding(16)
                .width(Length::Fixed(210.0))
                .height(Length::Fill)
                .style(theme::container::sidebar_primary),
        )
        .push(
            iced::widget::scrollable(container(content).padding(20).width(Length::Fill))
                .id(CONTENT_SCROLL_ID)
                .height(Length::Fill)
                .width(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .into();

    container(Column::new().push(header).push(body))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::container::background)
        .into()
}

fn section_content<'a>(
    page: &'a PreferencesPageState,
    model: ViewModel<'a>,
) -> Element<'a, Message> {
    let mut content = Column::new().spacing(18).width(Length::Fill);
    if let Some(error) = model.load_error {
        content = content.push(
            text::p2_regular(format!("Device preferences could not be loaded: {error}"))
                .style(theme::text::error),
        );
    }
    if let Some(error) = model.save_error {
        let status = if model.balance_privacy == BalancePrivacy::HiddenLocally {
            "Could not save. Amounts remain hidden in this session. Retry Hide to save that choice."
        } else {
            "Could not save. Your previous settings remain active."
        };
        content =
            content.push(text::p2_regular(format!("{status} {error}")).style(theme::text::error));
    }
    if model.saving {
        content = content.push(text::p2_regular("Saving device preferences…"));
    }
    if model.durability_unconfirmed {
        content = content.push(
            text::p2_regular("Saved. Durability could not be confirmed.")
                .style(theme::text::secondary),
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
        content = content.push(text::p2_regular(status));
        if model.load_error.is_some() {
            content = content
                .push(focusable_button(
                    "tenshu.preferences.retry-load",
                    "Retry load",
                    theme::button::primary,
                    idle.then_some(Message::Action(Action::RetryLoad)),
                ))
                .push(explicit_privacy_choice(
                    idle && model.balance_privacy_interactive,
                ));
        }
        return content.into();
    };
    let enabled = idle;
    match page.section {
        Section::Appearance => {
            let current = match model.system_theme {
                Some(ThemeMode::Light) => "System is currently: Light",
                Some(ThemeMode::Dark) => "System is currently: Dark",
                None => "System theme unavailable; System uses a dark fallback",
            };
            content = content
                .push(text::h3("Appearance"))
                .push(preference_row(
                    "Theme",
                    "Colors for the Launcher and every Cube on this device.",
                    Row::new()
                        .spacing(6)
                        .push(choice(
                            "System",
                            saved.theme == ThemePreference::System,
                            enabled,
                            PreferenceChange::Theme(ThemePreference::System),
                        ))
                        .push(choice(
                            "Light",
                            saved.theme == ThemePreference::Light,
                            enabled,
                            PreferenceChange::Theme(ThemePreference::Light),
                        ))
                        .push(choice(
                            "Dark",
                            saved.theme == ThemePreference::Dark,
                            enabled,
                            PreferenceChange::Theme(ThemePreference::Dark),
                        ))
                        .into(),
                ))
                .push(text::caption(current).style(theme::text::secondary));
        }
        Section::Launcher => {
            let status = if !model.cubes_renderer_available
                && saved.launcher_style == LauncherStyle::Cubes
            {
                "Cubes is saved; List is shown until the Cubes view is available."
            } else if model
                .effective
                .is_some_and(|value| value.launcher_style == LauncherStyle::List)
                && saved.launcher_style == LauncherStyle::Cubes
            {
                "Reduced motion is using List. Your Cubes choice remains saved."
            } else {
                "Your selected style applies to this device."
            };
            content = content
                .push(text::h3("Launcher"))
                .push(preference_row(
                    "Launcher style",
                    "List is used when reduced motion is active or Cubes rendering is unavailable.",
                    Row::new()
                        .spacing(6)
                        .push(choice(
                            "Cubes",
                            saved.launcher_style == LauncherStyle::Cubes,
                            enabled,
                            PreferenceChange::LauncherStyle(LauncherStyle::Cubes),
                        ))
                        .push(choice(
                            "List",
                            saved.launcher_style == LauncherStyle::List,
                            enabled,
                            PreferenceChange::LauncherStyle(LauncherStyle::List),
                        ))
                        .into(),
                ))
                .push(text::caption(status).style(theme::text::secondary));
        }
        Section::BalancesAndMotion => {
            let privacy_enabled = enabled && model.balance_privacy_interactive;
            let system_status = match model.system_reduces_motion {
                Some(true) => "System reports reduced motion",
                Some(false) => "System did not request reduced motion",
                None => "System motion preference unavailable",
            };
            content = content
                .push(text::h3("Balances & motion"))
                .push(preference_row(
                    "Hide balances",
                    "Mask passive amounts across every Cube. The top-bar eye changes this same device setting.",
                    match model.balance_privacy {
                        BalancePrivacy::Unresolved => explicit_privacy_choice(privacy_enabled),
                        BalancePrivacy::HiddenLocally => retry_hide_controls(privacy_enabled),
                        privacy => {
                            let hidden = privacy.masks_amounts();
                            boolean_toggle(
                                "tenshu.preferences.hide-balances",
                                if hidden { "On" } else { "Off" },
                                hidden,
                                privacy_enabled,
                                privacy.toggle_change(),
                            )
                        }
                    },
                ))
                .push_maybe((!model.balance_privacy_interactive).then(|| {
                    text::caption("Balance visibility changes are temporarily unavailable.")
                        .style(theme::text::secondary)
                }))
                .push(preference_row(
                    "Motion",
                    "Reduced removes decorative movement; security checks never wait for animation.",
                    Row::new()
                        .spacing(6)
                        .push(choice("Follow system", saved.motion == MotionPreference::FollowSystem, enabled, PreferenceChange::Motion(MotionPreference::FollowSystem)))
                        .push(choice("Reduced", saved.motion == MotionPreference::Reduced, enabled, PreferenceChange::Motion(MotionPreference::Reduced)))
                        .into(),
                ))
                .push(text::caption(system_status).style(theme::text::secondary));
        }
        Section::Developer => {
            content = content
                .push(text::h3("Developer"))
                .push(preference_row(
                    "Developer mode",
                    "Show Bitcoin test networks in device-visible network choices.",
                    boolean_toggle("tenshu.preferences.developer-mode", if saved.developer_mode { "On" } else { "Off" }, saved.developer_mode, enabled, PreferenceChange::DeveloperMode(!saved.developer_mode)),
                ))
                .push(preference_row(
                    "Bitcoin Blake2b beta",
                    "Show the beta network in the Launcher, Cube creation and Vault tools when available.",
                    boolean_toggle("tenshu.preferences.bitcoin-blake2b-beta", if saved.bitcoin_blake2b_beta { "On" } else { "Off" }, saved.bitcoin_blake2b_beta, enabled, PreferenceChange::BitcoinBlake2bBeta(!saved.bitcoin_blake2b_beta)),
                ));
        }
        Section::Diagnostics => {
            let diagnostics = model.diagnostics;
            content = content
                .push(text::h3("Diagnostics"))
                .push(text::p2_regular(
                    "Logs may contain sensitive data. Review any file before sharing it.",
                ))
                .push(action_row(
                    "tenshu.preferences.open-logs",
                    "Log folder",
                    "Open the configured application log folder.",
                    "Open folder",
                    diagnostics.can_open_logs.then_some(Action::OpenLogs),
                ))
                .push(action_row(
                    "tenshu.preferences.copy-support-summary",
                    "Support summary",
                    "Inspect the allowlisted summary below before copying.",
                    "Copy summary",
                    diagnostics
                        .support_summary_preview
                        .map(|_| Action::CopySupportSummary),
                ));
            if let Some(preview) = diagnostics.support_summary_preview {
                content = content.push(
                    container(text::p2_regular(preview))
                        .padding(12)
                        .width(Length::Fill)
                        .style(theme::container::border_grey),
                );
            } else {
                content = content.push(
                    text::caption("Support summary unavailable.").style(theme::text::secondary),
                );
            }
            content = content
                .push(preference_row(
                    "Stream status",
                    "Live Connect updates are separate from sign-in and Cube registration.",
                    text::p2_regular(diagnostics.stream_status.unwrap_or("Unavailable")).into(),
                ))
                .push(preference_row(
                    "Device registration",
                    "Repair requires an eligible signed-in Connect account.",
                    text::p2_regular(diagnostics.registration_status.unwrap_or("Unavailable"))
                        .into(),
                ))
                .push(focusable_button(
                    "tenshu.preferences.repair-details",
                    if page.registration_repair_expanded {
                        "Hide repair details"
                    } else {
                        "Repair details"
                    },
                    theme::button::secondary,
                    Some(Message::ToggleRegistrationRepair),
                ));
            if page.registration_repair_expanded {
                content = content
                    .push(text::p2_regular("Re-registers this device with the selected Connect account. Cubes, wallets and keys are unchanged."))
                    .push(focusable_button("tenshu.preferences.repair-registration", "Sign in and repair", theme::button::primary, diagnostics.can_repair_registration.then_some(Message::Action(Action::RepairRegistration))));
            }
        }
        Section::About => {
            let about = model.about;
            content = content
                .push(text::h3("About"))
                .push(preference_row(
                    "Tenshu version",
                    "Installed on this device.",
                    text::p2_regular(format!("{}", crate::VERSION)).into(),
                ))
                .push(preference_row(
                    "Build provenance",
                    "Only metadata supplied by this build is shown.",
                    text::p2_regular(about.build_provenance.unwrap_or("Unavailable")).into(),
                ))
                .push(focusable_button(
                    "tenshu.preferences.component-versions",
                    if page.component_versions_expanded {
                        "Hide component versions"
                    } else {
                        "Component versions"
                    },
                    theme::button::secondary,
                    Some(Message::ToggleComponentVersions),
                ));
            if page.component_versions_expanded {
                content = content
                    .push(preference_row(
                        "Wallet daemon",
                        "Loaded component version.",
                        text::p2_regular(about.daemon_version.unwrap_or("Not loaded")).into(),
                    ))
                    .push(preference_row(
                        "Spark bridge",
                        "Loaded component version.",
                        text::p2_regular(about.spark_bridge_version.unwrap_or("Not loaded")).into(),
                    ));
            }
            content = content
                .push(action_row(
                    "tenshu.preferences.open-download-stats",
                    "Download Stats",
                    "Public release download counts.",
                    "View Download Stats",
                    about
                        .can_open_download_stats
                        .then_some(Action::OpenDownloadStats),
                ))
                .push(action_row(
                    "tenshu.preferences.open-licenses",
                    "Open-source licenses",
                    "Licenses for the software Tenshu is built on.",
                    "View licenses",
                    about.can_open_licenses.then_some(Action::OpenLicenses),
                ));
        }
    }
    content.into()
}

fn preference_row<'a>(
    label: &'static str,
    description: &'static str,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    container(
        Row::new()
            .spacing(16)
            .align_y(Alignment::Center)
            .push(
                Column::new()
                    .spacing(4)
                    .push(text::p2_medium(label))
                    .push(text::caption(description).style(theme::text::secondary))
                    .push(text::caption("This device").style(theme::text::secondary))
                    .width(Length::Fill),
            )
            .push(control),
    )
    .padding(16)
    .width(Length::Fill)
    .style(theme::container::border_grey)
    .into()
}

fn choice(
    label: &'static str,
    selected: bool,
    enabled: bool,
    change: PreferenceChange,
) -> Element<'static, Message> {
    let style = if selected {
        theme::button::menu_pressed
    } else {
        theme::button::menu
    };
    let id = match change {
        PreferenceChange::Theme(ThemePreference::System) => "tenshu.preferences.theme.system",
        PreferenceChange::Theme(ThemePreference::Light) => "tenshu.preferences.theme.light",
        PreferenceChange::Theme(ThemePreference::Dark) => "tenshu.preferences.theme.dark",
        PreferenceChange::LauncherStyle(LauncherStyle::Cubes) => {
            "tenshu.preferences.launcher.cubes"
        }
        PreferenceChange::LauncherStyle(LauncherStyle::List) => "tenshu.preferences.launcher.list",
        PreferenceChange::Motion(MotionPreference::FollowSystem) => {
            "tenshu.preferences.motion.system"
        }
        PreferenceChange::Motion(MotionPreference::Reduced) => "tenshu.preferences.motion.reduced",
        PreferenceChange::HideBalances(_)
        | PreferenceChange::DeveloperMode(_)
        | PreferenceChange::BitcoinBlake2bBeta(_) => {
            unreachable!("boolean preferences use boolean_toggle")
        }
    };
    focusable_button(
        id,
        label,
        style,
        (enabled && !selected).then_some(Message::Change(change)),
    )
}

/// Shown while no accepted privacy choice exists: amounts are masked, and the
/// user may persist an explicit choice only when the shared balance controls
/// are interactive and no load or save is in flight.
/// Neither button is a fallback default.
fn explicit_privacy_choice(enabled: bool) -> Element<'static, Message> {
    Column::new()
        .spacing(6)
        .push(
            text::caption(if enabled {
                "Not set yet. Amounts stay hidden until you choose."
            } else {
                "Not set yet. Amounts stay hidden while visibility changes are unavailable."
            })
            .style(theme::text::secondary),
        )
        .push(
            Row::new()
                .spacing(6)
                .push(focusable_button(
                    "tenshu.preferences.balances.show",
                    "Show balances",
                    theme::button::menu,
                    enabled.then_some(Message::Change(PreferenceChange::HideBalances(false))),
                ))
                .push(focusable_button(
                    "tenshu.preferences.balances.keep-hidden",
                    "Keep hidden",
                    theme::button::menu,
                    enabled.then_some(Message::Change(PreferenceChange::HideBalances(true))),
                )),
        )
        .into()
}

/// Hide already masks this session, but the accepted on-disk choice still
/// says Show. Make Retry Hide the primary action; an explicit Show remains
/// available for a user who chooses to reveal again.
fn retry_hide_controls(enabled: bool) -> Element<'static, Message> {
    Column::new()
        .spacing(6)
        .push(
            text::caption("Balances are hidden now. Hide has not been saved on this device.")
                .style(theme::text::secondary),
        )
        .push(
            Row::new()
                .spacing(6)
                .push(focusable_button(
                    "tenshu.preferences.balances.retry-hide",
                    "Retry Hide",
                    theme::button::primary,
                    enabled.then_some(Message::Change(PreferenceChange::HideBalances(true))),
                ))
                .push(focusable_button(
                    "tenshu.preferences.balances.show-explicitly",
                    "Show balances",
                    theme::button::menu,
                    enabled.then_some(Message::Change(PreferenceChange::HideBalances(false))),
                )),
        )
        .into()
}

fn boolean_toggle(
    id: &'static str,
    label: &'static str,
    selected: bool,
    enabled: bool,
    change: PreferenceChange,
) -> Element<'static, Message> {
    let style = if selected {
        theme::button::menu_pressed
    } else {
        theme::button::menu
    };
    focusable_button(id, label, style, enabled.then_some(Message::Change(change)))
}

fn action_row<'a>(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    button_label: &'static str,
    action: Option<Action>,
) -> Element<'a, Message> {
    preference_row(
        label,
        description,
        focusable_button(
            id,
            button_label,
            theme::button::secondary,
            action.map(Message::Action),
        ),
    )
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
