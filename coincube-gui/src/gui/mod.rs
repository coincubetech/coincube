use iced::{
    event::{self, Event},
    keyboard,
    widget::pane_grid,
    Size, Subscription, Task,
};
use iced_runtime::window;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tracing::{error, info};
use tracing_subscriber::filter::LevelFilter;
extern crate serde;
extern crate serde_json;

use coincube_core::miniscript::bitcoin;
use coincube_ui::widget::{action, Element};

mod cache;
pub mod pane;
pub mod tab;

use crate::{
    app::{
        cache::{FiatPrice, FiatPriceRequest},
        message::{FiatMessage as AppFiatMessage, Message as AppMessage},
        settings::global::{GlobalSettings, PreferencesReceipt, WindowConfig},
        state::preferences::{
            BalancePrivacyChrome, Operation, PreferencesNotice, PreferencesOwner,
            PreferencesPageState, SaveOutcome, SaveRequest,
        },
        view::preferences as preferences_view,
    },
    dir::CoincubeDirectory,
    gui::cache::GlobalCache,
    home,
    logger::setup_logger,
    services::fiat::{
        api::{ListCurrenciesResult, PriceApi, PriceApiError},
        Currency, PriceClient, PriceSource,
    },
};

/// A Show receipt may have gone stale while the GUI was being constructed (or
/// between later refreshes). Recheck the established choice at render time;
/// any read failure is fail-closed until the ordered refresh accepts a receipt.
fn privacy_render_needs_cover(
    preferences: &PreferencesOwner,
    settings_path: &std::path::Path,
) -> bool {
    use crate::app::state::preferences::BalancePrivacy;
    preferences.privacy() == BalancePrivacy::Shown
        && !matches!(
            GlobalSettings::try_current_balances_hidden(settings_path),
            Ok(Some(false))
        )
}

use iced::window::Id;

/// Both automatic polling and explicit signing actions need the current
/// cross-tab binding. A poll completion can arrive after the fork tab opens.
fn refreshes_claim_pair(message: &AppMessage) -> bool {
    use crate::app::{
        state::vault::claim::ClaimEvent,
        view::{ClaimMessage, Message},
    };
    matches!(
        message,
        AppMessage::View(Message::Claim(
            ClaimMessage::CheckFunding
                | ClaimMessage::SignPair
                | ClaimMessage::ReviewPairedBitcoin
                | ClaimMessage::ConfirmPairedBitcoin
                | ClaimMessage::ReviewPairedFork
                | ClaimMessage::ConfirmPairedFork
        )) | AppMessage::Claim(ClaimEvent::DepositPoll | ClaimEvent::DepositStatusChecked(..))
    )
}

/// The eye's message must retain the generation emitted by the App view.
/// A bare `Run` has no provenance and cannot change a device-wide setting.
fn balance_privacy_eye_change(
    tab: Option<&tab::Tab>,
    message: &tab::Message,
) -> Option<(crate::app::settings::preferences::PreferenceChange, bool)> {
    let run_change = |message: &tab::Message| match message {
        tab::Message::Run(AppMessage::View(crate::app::view::Message::ChangeDevicePreference(
            change,
        ))) => Some(*change),
        _ => None,
    };
    match message {
        tab::Message::AppAsync(generation, inner) => run_change(inner).map(|change| {
            let current = tab.is_some_and(|tab| {
                tab.accepts_app_generation(*generation)
                    && matches!(&tab.state, tab::State::App(app) if !app.cube_settings().network.is_blake2b())
            });
            (change, current)
        }),
        tab::Message::ForkAsync(generation, inner) => run_change(inner).map(|change| {
            let current = tab.is_some_and(|tab| {
                tab.accepts_fork_generation(*generation)
                    && matches!(&tab.state, tab::State::App(app) if app.cube_settings().network.is_blake2b())
            });
            (change, current)
        }),
        other => run_change(other).map(|change| (change, false)),
    }
}

/// The Launcher eye may only hide or show balances, and only while the tab
/// that sent it still shows the Launcher. A queued press from a tab that
/// has since opened a Cube is dropped, like a stale App eye.
fn launcher_eye_is_current(
    tab: Option<&tab::Tab>,
    change: &crate::app::settings::preferences::PreferenceChange,
) -> bool {
    matches!(
        change,
        crate::app::settings::preferences::PreferenceChange::HideBalances(_)
    ) && tab.is_some_and(|tab| matches!(tab.state, tab::State::Home(_)))
}

/// Home reload also reconciles fork and Split flow availability. Only feature
/// edits may ask it to run that reconciliation.
fn needs_global_settings_reload(
    before: Option<crate::app::settings::preferences::DevicePreferences>,
    after: Option<crate::app::settings::preferences::DevicePreferences>,
) -> bool {
    match (before, after) {
        (Some(before), Some(after)) => {
            before.developer_mode != after.developer_mode
                || before.bitcoin_blake2b_beta != after.bitcoin_blake2b_beta
        }
        (None, Some(_)) => true,
        _ => false,
    }
}

fn launcher_presentation_changed(
    before: Option<crate::app::settings::preferences::DevicePreferences>,
    after: Option<crate::app::settings::preferences::DevicePreferences>,
) -> bool {
    match (before, after) {
        (Some(before), Some(after)) => {
            before.launcher_style != after.launcher_style || before.motion != after.motion
        }
        (None, Some(_)) => true,
        _ => false,
    }
}

#[cfg(test)]
mod balance_privacy_eye_origin_tests {
    use super::*;
    use crate::app::{self, settings::preferences::PreferenceChange, view};

    #[test]
    fn first_render_rechecks_a_show_receipt_after_another_writer_hides() {
        let root =
            std::env::temp_dir().join(format!("privacy-first-render-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join(crate::app::settings::global::DEFAULT_FILE_NAME);
        let shown =
            GlobalSettings::save_preference(&path, PreferenceChange::HideBalances(false)).unwrap();
        let owner = PreferencesOwner::new(path.clone(), &Ok(shown));
        let shown_bytes = std::fs::read(&path).unwrap();
        assert!(!privacy_render_needs_cover(&owner, &path));
        assert_eq!(std::fs::read(&path).unwrap(), shown_bytes);

        let other_path = path.clone();
        std::thread::spawn(move || {
            GlobalSettings::save_preference(&other_path, PreferenceChange::HideBalances(true))
                .unwrap();
        })
        .join()
        .unwrap();
        assert_eq!(
            owner.privacy(),
            crate::app::state::preferences::BalancePrivacy::Shown
        );
        assert!(privacy_render_needs_cover(&owner, &path));

        std::fs::write(&path, br#"{"hide_balances":false}"#).unwrap();
        assert!(privacy_render_needs_cover(&owner, &path));

        std::fs::write(&path, b"{malformed").unwrap();
        assert!(privacy_render_needs_cover(&owner, &path));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn render_covers_amounts_without_waiting_for_a_settings_writer() {
        use fs4::fs_std::FileExt;
        use std::sync::mpsc;

        let root =
            std::env::temp_dir().join(format!("privacy-render-lock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join(crate::app::settings::global::DEFAULT_FILE_NAME);
        let shown =
            GlobalSettings::save_preference(&path, PreferenceChange::HideBalances(false)).unwrap();
        let owner = PreferencesOwner::new(path.clone(), &Ok(shown));
        let lock = std::fs::File::open(path.with_extension("lock")).unwrap();
        FileExt::lock_exclusive(&lock).unwrap();

        let (sender, receiver) = mpsc::channel();
        let render = std::thread::spawn(move || {
            sender
                .send(privacy_render_needs_cover(&owner, &path))
                .unwrap();
        });
        let covered_while_locked = receiver.recv_timeout(Duration::from_secs(2));
        FileExt::unlock(&lock).unwrap();
        render.join().unwrap();
        assert!(covered_while_locked.unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn queued_eye_event_cannot_change_device_privacy_after_app_closes() {
        let _guard = app::session::test_guard();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let path =
            std::env::temp_dir().join(format!("privacy-eye-origin-{}", uuid::Uuid::new_v4()));
        let (app, _) = app::claim_step1_tests::bitcoin_app(&path);
        let mut tab = tab::Tab::new(1, tab::State::App(app));
        let generation = tab.app_generation_token().unwrap();
        let change = PreferenceChange::HideBalances(false);
        assert!(
            !launcher_eye_is_current(Some(&tab), &change),
            "a Launcher eye event cannot write once an App owns the tab"
        );
        let eye = || {
            tab::Message::AppAsync(
                generation,
                Box::new(tab::Message::Run(AppMessage::View(
                    view::Message::ChangeDevicePreference(change),
                ))),
            )
        };

        assert_eq!(
            balance_privacy_eye_change(Some(&tab), &eye()),
            Some((change, true))
        );
        assert_eq!(
            balance_privacy_eye_change(
                Some(&tab),
                &tab::Message::Run(AppMessage::View(view::Message::ChangeDevicePreference(
                    change
                )))
            ),
            Some((change, false)),
            "a bare Run event has no App generation"
        );
        drop(tab.update(tab::Message::LockCubeToLauncher));
        assert!(matches!(tab.state, tab::State::Home(_)));
        assert!(
            !launcher_eye_is_current(None, &change),
            "a Launcher eye event for a closed tab is dropped"
        );
        assert!(launcher_eye_is_current(Some(&tab), &change));
        assert!(
            !launcher_eye_is_current(Some(&tab), &PreferenceChange::DeveloperMode(true)),
            "the Launcher path writes only the hide-balances preference"
        );
        assert_eq!(
            balance_privacy_eye_change(Some(&tab), &eye()),
            Some((change, false))
        );
        std::fs::remove_dir_all(path).ok();
    }
}

/// Exact provider/account context to revalidate on the next local PIN attempt.
/// Holding this candidate grants neither seed access nor daemon authority.
#[derive(Clone)]
struct ForkUnlockCandidate {
    root: CoincubeDirectory,
    client: crate::services::coincube::CoincubeClient,
    user_id: u32,
}

pub struct GUI {
    panes: pane_grid::State<pane::Pane>,
    focus: Option<pane_grid::Pane>,
    config: Config,
    window_id: Option<Id>,
    window_init: Option<bool>,
    window_config: Option<WindowConfig>,
    global_cache: GlobalCache,
    theme_mode: coincube_ui::theme::palette::ThemeMode,
    system_theme: Option<coincube_ui::theme::palette::ThemeMode>,
    system_reduces_motion: Option<bool>,
    preferences_sheet: Option<PreferencesSheet>,
    preferences: PreferencesOwner,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Key {
    Tab(bool),
    Escape,
}

#[derive(Debug)]
pub enum Message {
    CtrlC,
    Tick,
    FontLoaded(Result<(), iced::font::Error>),
    Pane(pane_grid::Pane, pane::Message),
    KeyPressed(Key),
    Event(iced::Event),

    Clicked(pane_grid::Pane),
    Dragged(pane_grid::DragEvent),
    Resized(pane_grid::ResizeEvent),
    Window(Option<Id>),
    WindowSize(Size),

    Fiat(FiatMessage),
    ToggleTheme,
    SystemTheme(iced::theme::Mode),
    PreferencesSheet(preferences_view::Message),
    Preferences(SaveRequest, Result<PreferencesReceipt, String>),
}

#[derive(Debug)]
pub enum FiatMessage {
    GetPriceResult(FiatPrice),
    /// Result of a request for the list of available currencies for a given source.
    ///
    /// The pane and tab that requested the list are included to be able to return the result.
    ListCurrenciesResult(
        pane_grid::Pane,
        usize, // tab id
        PriceSource,
        Instant,
        Result<ListCurrenciesResult, PriceApiError>,
    ),
}

impl From<FiatMessage> for Message {
    fn from(value: FiatMessage) -> Self {
        Self::Fiat(value)
    }
}

impl From<Result<(), iced::font::Error>> for Message {
    fn from(value: Result<(), iced::font::Error>) -> Self {
        Self::FontLoaded(value)
    }
}

async fn ctrl_c() -> Result<(), ()> {
    if let Err(e) = tokio::signal::ctrl_c().await {
        error!("{}", e);
    }
    info!("Signal received, exiting");
    Ok(())
}

/// Whether an event means a human did something.
///
/// This is the idle auto-lock's only source of truth for "someone is at the
/// machine". Window events are deliberately excluded: a resize or a focus
/// change can be produced by the OS, another application, or a display
/// reconfiguration with nobody present.
fn is_user_input(event: &Event) -> bool {
    matches!(
        event,
        Event::Keyboard(_) | Event::Mouse(_) | Event::Touch(_)
    )
}

/// Only keys the focused widget declined belong to the GUI shell. Captured
/// Escape remains with text fields and modal content.
fn ignored_shell_key(event: &Event, status: event::Status) -> Option<Key> {
    if !matches!(status, event::Status::Ignored) {
        return None;
    }
    match event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Tab),
            modifiers,
            ..
        }) => Some(Key::Tab(modifiers.shift())),
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            repeat: false,
            ..
        }) => Some(Key::Escape),
        _ => None,
    }
}

fn connect_account_message(
    message: &tab::Message,
) -> Option<&crate::app::view::ConnectAccountMessage> {
    match message {
        tab::Message::Launch(home::Message::View(home::ViewMessage::ConnectAccount(message)))
        | tab::Message::Run(AppMessage::View(crate::app::view::Message::ConnectAccount(message))) => {
            Some(message)
        }
        _ => None,
    }
}

/// Guarded App tasks bypass Pane's ordinary task-output mapper. Preserve its
/// navigation bubbles after checking the generation in `GUI::update`.
fn route_guarded_tab_result(
    pane_id: pane_grid::Pane,
    tab_id: usize,
    message: tab::Message,
) -> Message {
    let view = match message {
        tab::Message::OpenLauncher => pane::ViewMessage::OpenLauncher,
        tab::Message::OpenConnectSignIn => pane::ViewMessage::OpenConnectSignIn,
        tab::Message::OpenPlanBilling => pane::ViewMessage::OpenPlanBilling,
        tab::Message::ToggleTheme => pane::ViewMessage::ToggleTheme,
        tab::Message::OpenAppPreferences(source) => {
            return Message::Pane(
                pane_id,
                pane::Message::tab(tab_id, tab::Message::OpenAppPreferences(source)),
            );
        }
        tab::Message::GlobalSettingsChanged => pane::ViewMessage::GlobalSettingsChanged,
        tab::Message::ConnectSignedIn => pane::ViewMessage::ConnectSignedIn,
        tab::Message::OpenBitcoinClaim(handoff) => pane::ViewMessage::OpenBitcoinClaim(handoff),
        tab::Message::OpenForkClaim(handoff) => pane::ViewMessage::OpenForkClaim(handoff),
        message => return Message::Pane(pane_id, pane::Message::tab(tab_id, message)),
    };
    Message::Pane(pane_id, pane::Message::View(view))
}

#[cfg(test)]
fn unresolved_test_preferences() -> PreferencesOwner {
    PreferencesOwner::new(
        std::path::PathBuf::from("unresolved-test-fixture")
            .join(crate::app::settings::global::DEFAULT_FILE_NAME),
        &Err(crate::app::settings::global::PrivacyInitError::Settings(
            crate::app::settings::SettingsError::Unexpected(
                "not loaded by this fixture".to_string(),
            ),
        )),
    )
}

impl GUI {
    /// The process owns one secret session and idle clock. A new public Cube
    /// admission must retire every old secret-bearing surface before its
    /// constructor or unlock callback can run, even when no App tab remains.
    fn retire_public_owners(&mut self) {
        for (_, pane) in self.panes.iter_mut() {
            pane.close_tabs_matching(|tab| {
                matches!(
                    tab.state,
                    tab::State::App(_)
                        | tab::State::Loader(_)
                        | tab::State::Installer(_)
                        | tab::State::Login(_)
                        | tab::State::PinEntry(_)
                        | tab::State::PasskeyUnlock(_)
                )
            });
        }
        crate::app::session::close();
    }

    /// A scoped Claim is the sole exception to full public-owner teardown.
    /// Its source App keeps the one live process session; every legacy second
    /// App or in-progress unlock/installer must still release its resources.
    fn retire_other_public_owners(&mut self, owner: crate::app::cache::AppGeneration) {
        for (_, pane) in self.panes.iter_mut() {
            pane.close_tabs_matching(|tab| {
                let source = matches!(&tab.state, tab::State::App(app)
                    if app.cache().app_generation == owner);
                !source
                    && matches!(
                        tab.state,
                        tab::State::App(_)
                            | tab::State::Loader(_)
                            | tab::State::Installer(_)
                            | tab::State::Login(_)
                            | tab::State::PinEntry(_)
                            | tab::State::PasskeyUnlock(_)
                    )
            });
        }
    }

    #[cfg(test)]
    fn reviewer_unwrap_guarded_message(mut message: &tab::Message) -> &tab::Message {
        while let tab::Message::AppAsync(_, inner) | tab::Message::ForkAsync(_, inner) = message {
            message = inner;
        }
        message
    }

    pub fn title(&self) -> String {
        match cfg!(debug_assertions) {
            true => format!("Tenshu v{} (development)", env!("CARGO_PKG_VERSION")),
            false => format!("Tenshu v{}", env!("CARGO_PKG_VERSION")),
        }
    }

    pub fn new(config: Config, log_level: Option<LevelFilter>) -> (GUI, Task<Message>) {
        // Establish the global privacy receipt before constructing the first pane.
        // A failed or malformed read remains unresolved and masked.
        let privacy = GlobalSettings::load_or_initialize_preferences(&config.coincube_directory);
        let preferences =
            PreferencesOwner::new(GlobalSettings::path(&config.coincube_directory), &privacy);
        let log_level = log_level.unwrap_or(LevelFilter::INFO);
        // Record the resolved (possibly `--datadir`-overridden) data directory
        // process-wide so helpers without a datadir in hand — e.g. the Connect
        // panel's duress fingerprint — use the real path, not the OS default.
        CoincubeDirectory::set_active(config.coincube_directory.clone());

        if let Err(e) = setup_logger(log_level, &config.coincube_directory) {
            eprintln!("Error while setting up logger: {}", e);
        }

        let mut cmds = vec![
            window::oldest().map(Message::Window),
            Task::perform(ctrl_c(), |_| Message::CtrlC),
            iced::system::theme().map(Message::SystemTheme),
        ];
        let (mut pane, cmd) = pane::Pane::new(&config);
        pane.set_balance_privacy(Some(Self::privacy_chrome_for(&preferences)));
        let system_reduces_motion = crate::app::settings::system_accessibility::reduces_motion();
        for tab in &mut pane.tabs {
            tab.set_system_reduces_motion(system_reduces_motion);
        }
        let (mut panes, focused_pane) = pane_grid::State::new(pane);
        cmds.push(cmd.map(move |msg| Message::Pane(focused_pane, msg)));
        let global_settings_path = GlobalSettings::path(&config.coincube_directory);
        crate::services::branta::set_enabled(GlobalSettings::load_recipient_identity_checks(
            &global_settings_path,
        ));
        let window_config = GlobalSettings::load_window_config(&global_settings_path);
        let window_init = window_config.is_some().then_some(true);
        let theme_mode = GlobalSettings::load_theme_mode(&global_settings_path);
        // Propagate persisted theme mode to all pane tabs
        for (_, pane) in panes.iter_mut() {
            pane.set_theme_mode(theme_mode);
        }
        (
            Self {
                panes,
                focus: Some(focused_pane),
                config,
                window_id: None,
                window_init,
                window_config,
                global_cache: GlobalCache::default(),
                theme_mode,
                system_theme: None,
                system_reduces_motion,
                preferences_sheet: None,
                preferences,
            },
            Task::batch(cmds),
        )
    }

    fn privacy_chrome_for(preferences: &PreferencesOwner) -> BalancePrivacyChrome {
        BalancePrivacyChrome {
            privacy: preferences.privacy(),
            busy: preferences.is_busy(),
            interactive: preferences.accepted().is_some() && !preferences.is_busy(),
        }
    }

    fn publish_balance_privacy(&mut self) {
        let chrome = Some(Self::privacy_chrome_for(&self.preferences));
        for (_, pane) in self.panes.iter_mut() {
            pane.set_balance_privacy(chrome);
        }
    }

    /// A single resolved device theme drives the window and every retained tab.
    /// A System choice follows the latest theme reported by iced; until that
    /// arrives it uses the model's documented dark fallback.
    fn publish_appearance(&mut self) {
        let Some(accepted) = self.preferences.accepted() else {
            return;
        };
        let resolved = accepted
            .preferences
            .resolve(self.system_theme, self.system_reduces_motion);
        self.theme_mode = resolved.theme_mode;
        for (_, pane) in self.panes.iter_mut() {
            pane.set_theme_mode(self.theme_mode);
        }
    }

    fn reload_all_global_settings(&mut self) -> Task<Message> {
        let root = &self.config.coincube_directory;
        let mut tasks = Vec::new();
        for (&pane_id, pane) in self.panes.iter_mut() {
            for tab in &mut pane.tabs {
                let tab_id = tab.id;
                tasks.push(tab.reload_global_settings(root).map(move |message| {
                    Message::Pane(pane_id, pane::Message::tab(tab_id, message))
                }));
            }
        }
        Task::batch(tasks)
    }

    fn refresh_all_launcher_modes(&mut self) {
        for (_, pane) in self.panes.iter_mut() {
            for tab in &mut pane.tabs {
                if let tab::State::Home(home) = &mut tab.state {
                    home.refresh_launcher_mode();
                }
            }
        }
    }

    fn preferences_view_model(&self) -> preferences_view::ViewModel<'_> {
        let accepted = self.preferences.accepted();
        let notice = self.preferences.notice();
        preferences_view::ViewModel {
            saved: accepted.map(|value| value.preferences),
            effective: accepted.map(|value| {
                value
                    .preferences
                    .resolve(self.system_theme, self.system_reduces_motion)
            }),
            system_theme: self.system_theme,
            system_reduces_motion: self.system_reduces_motion,
            cubes_renderer_available: true,
            saving: self.preferences.is_busy(),
            balance_privacy: self.preferences.privacy(),
            balance_privacy_interactive: !self.preferences.is_busy(),
            durability_unconfirmed: matches!(notice, PreferencesNotice::SavedUnconfirmed),
            loading: self.preferences.is_busy() && accepted.is_none(),
            load_error: matches!(notice, PreferencesNotice::LoadFailed)
                .then_some("The device settings could not be loaded."),
            save_error: matches!(notice, PreferencesNotice::SaveFailed)
                .then_some("Please try again."),
            diagnostics: preferences_view::Diagnostics::default(),
            about: preferences_view::About::default(),
        }
    }

    fn close_preferences_sheet(&mut self) -> Task<Message> {
        let Some(sheet) = self.preferences_sheet.take() else {
            return Task::none();
        };
        if self.overlay_origin_is_current(&sheet.origin) {
            if let Some(focus) = sheet.origin.focus {
                return iced_runtime::task::widget(
                    iced::advanced::widget::operation::focusable::focus(focus),
                );
            }
        }
        Task::none()
    }

    fn preferences_sheet_message(&mut self, message: preferences_view::Message) -> Task<Message> {
        if !self
            .preferences_sheet
            .as_ref()
            .is_some_and(|sheet| self.overlay_origin_is_current(&sheet.origin))
        {
            return Task::none();
        }
        use preferences_view::{Action, Message as ViewMessage};
        match message {
            ViewMessage::FocusAction(id) => {
                iced_runtime::task::widget(iced::advanced::widget::operation::focusable::focus(id))
            }
            ViewMessage::SelectSection(section) => {
                if let Some(sheet) = &mut self.preferences_sheet {
                    sheet.page.select(section);
                }
                Task::none()
            }
            ViewMessage::Change(change) => self.change_preference(change),
            ViewMessage::ToggleComponentVersions => {
                if let Some(sheet) = &mut self.preferences_sheet {
                    sheet.page.toggle_component_versions();
                }
                Task::none()
            }
            ViewMessage::ToggleRegistrationRepair => {
                if let Some(sheet) = &mut self.preferences_sheet {
                    sheet.page.toggle_registration_repair();
                }
                Task::none()
            }
            ViewMessage::Action(Action::Close) => self.close_preferences_sheet(),
            ViewMessage::Action(Action::RetryLoad) => self.retry_preferences(),
            // These remain disabled until their real diagnostic owners supply
            // an explicit capability to the sheet's view model.
            ViewMessage::Action(_) => Task::none(),
        }
    }

    /// Retries `load_or_initialize_preferences` on the preferences lane.
    /// While the lane is busy this is refused, not queued.
    pub fn retry_preferences(&mut self) -> Task<Message> {
        match self.preferences.begin_initialize() {
            Ok(request) => {
                self.publish_balance_privacy();
                self.run_preferences(request)
            }
            Err(_) => Task::none(),
        }
    }

    /// One explicit device-preference change on the same lane. An unresolved
    /// or busy privacy authority cannot originate a GUI preference change.
    pub fn change_preference(
        &mut self,
        change: crate::app::settings::preferences::PreferenceChange,
    ) -> Task<Message> {
        match self.preferences.begin_save(change) {
            Ok(request) => {
                self.publish_balance_privacy();
                self.run_preferences(request)
            }
            Err(_) => Task::none(),
        }
    }

    fn change_balance_privacy(
        &mut self,
        change: crate::app::settings::preferences::PreferenceChange,
    ) -> Task<Message> {
        let Ok(request) = self.preferences.begin_save(change) else {
            return Task::none();
        };
        self.publish_balance_privacy();
        self.run_preferences(request)
    }

    fn run_preferences(&self, request: SaveRequest) -> Task<Message> {
        let root = self.config.coincube_directory.clone();
        let work = request.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || match work.operation {
                    Operation::Initialize => GlobalSettings::load_or_initialize_preferences(&root)
                        .map_err(|error| error.to_string()),
                    Operation::Save(change) => GlobalSettings::save_preference(&work.root, change)
                        .map_err(|error| error.to_string()),
                })
                .await
                .unwrap_or_else(|_| Err("The preferences task did not finish.".to_string()))
            },
            move |result| Message::Preferences(request, result),
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        if self
            .preferences_sheet
            .as_ref()
            .is_some_and(|sheet| !self.overlay_origin_is_current(&sheet.origin))
        {
            self.preferences_sheet = None;
        }
        // Intercept before stripping AppAsync/ForkAsync so a queued eye press
        // from a closed or replaced Cube cannot mutate device preferences.
        if let Message::Pane(pane_id, pane::Message::Tab(tab_id, inner)) = &message {
            let tab = self
                .panes
                .get(*pane_id)
                .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == *tab_id));
            if let Some((change, current)) = balance_privacy_eye_change(tab, inner) {
                return if current {
                    self.change_balance_privacy(change)
                } else {
                    Task::none()
                };
            }
            if let tab::Message::HomeDevicePreference(change) = inner.as_ref() {
                return if launcher_eye_is_current(tab, change) {
                    self.change_balance_privacy(*change)
                } else {
                    Task::none()
                };
            }
        }
        // Validate session-tagged App callbacks before inspecting global auth
        // actions. A queued event from an old App cannot affect a reopened one.
        let message = match message {
            Message::Pane(pane_id, pane::Message::Tab(tab_id, tab_message))
                if matches!(tab_message.as_ref(), tab::Message::AppAsync(..)) =>
            {
                let tab::Message::AppAsync(generation, message) = *tab_message else {
                    unreachable!();
                };
                let current = self
                    .panes
                    .get(pane_id)
                    .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == tab_id))
                    .is_some_and(|tab| tab.accepts_app_generation(generation));
                if !current {
                    tab::discard_closed_loader_result(*message);
                    return Task::none();
                }
                route_guarded_tab_result(pane_id, tab_id, *message)
            }
            Message::Pane(pane_id, pane::Message::Tab(tab_id, tab_message))
                if matches!(tab_message.as_ref(), tab::Message::ForkAsync(..)) =>
            {
                let tab::Message::ForkAsync(generation, message) = *tab_message else {
                    unreachable!();
                };
                let current = self
                    .panes
                    .get(pane_id)
                    .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == tab_id))
                    .is_some_and(|tab| tab.accepts_fork_generation(generation));
                if !current {
                    tab::discard_closed_loader_result(*message);
                    return Task::none();
                }
                route_guarded_tab_result(pane_id, tab_id, *message)
            }
            Message::Pane(pane_id, pane::Message::Tab(tab_id, tab_message))
                if !self
                    .panes
                    .get(pane_id)
                    .is_some_and(|pane| pane.tabs.iter().any(|tab| tab.id == tab_id)) =>
            {
                tab::discard_closed_loader_result(*tab_message);
                return Task::none();
            }
            message => message,
        };
        // Every hosted fork PIN starts with a completed, scoped account
        // candidate. A cold Home can borrow one unambiguous verified sibling;
        // an account still being checked cannot bypass its gate that way.
        let opening_pin = match &message {
            Message::Pane(pane_id, pane::Message::Tab(tab_id, inner)) => match inner.as_ref() {
                tab::Message::Launch(
                    home::Message::Run(root, _, chain, _)
                    | home::Message::RunFromFloor(root, _, chain, _, _)
                    | home::Message::RunToRecovery(root, _, chain, _)
                    | home::Message::RunClaim(root, _, chain, _, _),
                ) if chain.is_blake2b() => {
                    self.panes
                        .get(*pane_id)
                        .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == *tab_id))
                        .and_then(|tab| match &tab.state {
                            tab::State::Home(home) => {
                                let candidate = if home.connect_account.is_authenticated() {
                                    self.verified_unlock_origin(*pane_id, *tab_id)
                                } else if home.connect_account.authenticated_client().is_none() {
                                    self.unique_unlock_candidate(
                                        root,
                                        &home.connect_account.client.base_url,
                                    )
                                } else {
                                    // A retained token while this Home's account
                                    // gate is checking or closed is not admission.
                                    None
                                };
                                candidate.map(|candidate| (*pane_id, *tab_id, candidate))
                            }
                            _ => None,
                        })
                }
                _ => None,
            },
            _ => None,
        };
        // Shared-client restore reaches readiness without SetSession. Capture
        // its exact current origin before Pane converts this to an originless intent.
        let ready_unlock = match &message {
            Message::Pane(pane_id, pane::Message::Tab(tab_id, inner))
                if matches!(
                    inner.as_ref(),
                    tab::Message::Launch(home::Message::ConnectSignedInBubble)
                        | tab::Message::ConnectSignedIn
                ) =>
            {
                self.verified_unlock_origin(*pane_id, *tab_id)
            }
            _ => None,
        };
        // Reuse a sibling session when opening Home or a Bitcoin Cube. Separate
        // keyring refreshes race token rotation and can leave older Cubes with
        // an unauthenticated account panel. Each adopter still checks /user.
        let session_init = match &message {
            Message::Pane(pane_id, pane::Message::Tab(tab_id, inner))
                if matches!(
                    inner.as_ref(),
                    tab::Message::Launch(home::Message::View(home::ViewMessage::ConnectAccount(
                        crate::app::view::ConnectAccountMessage::Init
                    ))) | tab::Message::Run(AppMessage::View(
                        crate::app::view::Message::ConnectAccount(
                            crate::app::view::ConnectAccountMessage::Init
                        )
                    ))
                ) =>
            {
                Some((*pane_id, *tab_id))
            }
            _ => None,
        };
        if let Some((origin_pane, origin_tab)) = session_init {
            let active_client = self.panes.iter().find_map(|(&pane_id, pane)| {
                pane.tabs.iter().find_map(|tab| {
                    if pane_id == origin_pane && tab.id == origin_tab {
                        return None;
                    }
                    (match &tab.state {
                        tab::State::App(app) => app.shareable_connect_client(),
                        tab::State::Home(home) => home.connect_account.authenticated_client(),
                        _ => None,
                    })
                    .filter(|client| client.token().is_some_and(|token| !token.is_empty()))
                })
            });
            if let Some(client) = active_client {
                if let Some(tab) = self
                    .panes
                    .get_mut(origin_pane)
                    .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == origin_tab))
                {
                    let adopted = match &mut tab.state {
                        tab::State::Home(home)
                            if home.connect_account.can_adopt_shared_session() =>
                        {
                            Some(home.adopt_connect_client(client).map(tab::Message::Launch))
                        }
                        tab::State::App(app) if app.can_adopt_shared_connect_session() => {
                            let task = app.adopt_shared_connect_client(client);
                            Some(tab.emit_app_task(task))
                        }
                        _ => None,
                    };
                    if let Some(task) = adopted {
                        return task.map(move |message| {
                            Message::Pane(origin_pane, pane::Message::tab(origin_tab, message))
                        });
                    }
                }
            }
        }
        // A log-out or a sign-in from any tab reaches every tab, with its kind
        // and where it came from (`Tab::invalidate_fork_session`). A
        // `SetSession` counts as a sign-in only if the tab it came from will
        // accept it: its account panel drops a completion of an operation
        // begun before that tab's last auth invalidation
        // (`ConnectAccountPanel::stale_completion`), and a completion the
        // panel is about to drop must not tell the other tabs anything — this
        // is the first reader of the variant, ahead of that dispatch.
        let auth_change = match &message {
            Message::Pane(pane_id, pane::Message::Tab(tab_id, inner)) => {
                match connect_account_message(inner) {
                    Some(msg) => match msg {
                        crate::app::view::ConnectAccountMessage::LogOut => {
                            Some((tab::AuthChange::LogOut, *pane_id, *tab_id))
                        }
                        crate::app::view::ConnectAccountMessage::SetSession(login, epoch) => {
                            let origin_epoch = self
                                .panes
                                .get(*pane_id)
                                .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == *tab_id))
                                .and_then(|tab| match &tab.state {
                                    tab::State::App(app) => Some(app.connect_auth_epoch()),
                                    tab::State::Home(home) => {
                                        Some(home.connect_account.auth_epoch())
                                    }
                                    _ => None,
                                });
                            let same_fork_session = self
                        .panes
                        .get(*pane_id)
                        .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == *tab_id))
                        .is_some_and(|tab| {
                            matches!(&tab.state, tab::State::App(app) if app.matches_admitted_connect_session(login))
                        });
                            match origin_epoch {
                                Some(current) if *epoch >= current && !same_fork_session => Some((
                                    tab::AuthChange::SignIn {
                                        user_id: login.user.id,
                                        token: zeroize::Zeroizing::new(login.token.clone()),
                                    },
                                    *pane_id,
                                    *tab_id,
                                )),
                                // Stale for its own tab (dropped there on dispatch), or
                                // no account panel to judge it: not a sign-in.
                                _ => None,
                            }
                        }
                        _ => None,
                    },
                    None => None,
                }
            }
            _ => None,
        };
        let auth_from_fork_app = if auth_change.is_some() {
            match &message {
                Message::Pane(pane_id, pane::Message::Tab(tab_id, _)) => self
                    .panes
                    .get(*pane_id)
                    .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == *tab_id))
                    .is_some_and(
                        |tab| matches!(&tab.state, tab::State::App(app) if app.requires_connect()),
                    ),
                _ => false,
            }
        } else {
            false
        };
        let mut auth_tasks = Vec::new();
        if let Some((change, origin_pane, origin_tab)) = auth_change {
            let completed_login = match &message {
                Message::Pane(_, pane::Message::Tab(_, inner)) => {
                    match connect_account_message(inner) {
                        Some(crate::app::view::ConnectAccountMessage::SetSession(login, _)) => {
                            Some(login)
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            for (&pane_id, pane) in self.panes.iter_mut() {
                for tab in &mut pane.tabs {
                    if completed_login.is_some_and(|login| {
                        matches!(&tab.state, tab::State::App(app) if app.matches_admitted_connect_session(login))
                    }) {
                        continue;
                    }
                    let tab_id = tab.id;
                    let originated = pane_id == origin_pane && tab_id == origin_tab;
                    auth_tasks.push(
                        tab.invalidate_fork_session(change.clone(), originated)
                            .map(move |msg| {
                                Message::Pane(pane_id, pane::Message::tab(tab_id, msg))
                            }),
                    );
                }
            }
        }
        // The old fork App is gone. Apply its auth operation to the replacement
        // Home so logout clears saved login state instead of being discarded.
        let message = if auth_from_fork_app {
            match message {
                Message::Pane(pane_id, pane::Message::Tab(tab_id, inner)) => match *inner {
                    tab::Message::Run(AppMessage::View(
                        crate::app::view::Message::ConnectAccount(msg),
                    )) => Message::Pane(
                        pane_id,
                        pane::Message::tab(
                            tab_id,
                            tab::Message::Launch(home::Message::View(
                                home::ViewMessage::ConnectAccount(msg),
                            )),
                        ),
                    ),
                    other => Message::Pane(pane_id, pane::Message::tab(tab_id, other)),
                },
                message => message,
            }
        } else {
            message
        };
        // Dispatch may return early (notably for in-App account messages).
        // Keep those returns inside the helper so invalidation tasks always run.
        auth_tasks.push(self.update_message(message));
        if let Some(candidate) = ready_unlock {
            for (_, pane) in self.panes.iter_mut() {
                for tab in &mut pane.tabs {
                    if let tab::State::PinEntry(pin) = &mut tab.state {
                        pin.adopt_connect_candidate(
                            &candidate.root,
                            candidate.client.clone(),
                            candidate.user_id,
                        );
                    }
                }
            }
        }

        if let Some((pane_id, tab_id, candidate)) = opening_pin {
            if let Some(tab::Tab {
                state: tab::State::PinEntry(pin),
                ..
            }) = self
                .panes
                .get_mut(pane_id)
                .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == tab_id))
            {
                pin.adopt_connect_candidate(&candidate.root, candidate.client, candidate.user_id);
            }
        }

        self.refresh_claim_deposit_display();
        self.refresh_sidebar_session_status();
        Task::batch(auth_tasks)
    }

    fn verified_unlock_origin(
        &self,
        pane_id: pane_grid::Pane,
        tab_id: usize,
    ) -> Option<ForkUnlockCandidate> {
        let tab = self
            .panes
            .get(pane_id)?
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)?;
        match &tab.state {
            tab::State::Home(home) if home.connect_account.is_authenticated() => {
                Some(ForkUnlockCandidate {
                    root: home.datadir_path.clone(),
                    user_id: home.connect_account.user.as_ref()?.id,
                    client: home.connect_account.authenticated_client()?,
                })
            }
            tab::State::App(app) => {
                app.shareable_connect_identity()
                    .map(|(user_id, client)| ForkUnlockCandidate {
                        root: app.datadir().clone(),
                        user_id,
                        client,
                    })
            }
            _ => None,
        }
    }

    /// A cold Home may borrow only one unambiguous completed identity.
    /// Conflicting account or token snapshots require a new account check.
    fn unique_unlock_candidate(
        &self,
        root: &CoincubeDirectory,
        provider: &str,
    ) -> Option<ForkUnlockCandidate> {
        let mut selected: Option<ForkUnlockCandidate> = None;
        for (_, pane) in self.panes.iter() {
            for tab in &pane.tabs {
                let identity = match &tab.state {
                    tab::State::Home(home) if home.connect_account.is_authenticated() => {
                        home.connect_account.user.as_ref().and_then(|user| {
                            home.connect_account
                                .authenticated_client()
                                .map(|client| (home.datadir_path.clone(), user.id, client))
                        })
                    }
                    tab::State::App(app) => app
                        .shareable_connect_identity()
                        .map(|(user_id, client)| (app.datadir().clone(), user_id, client)),
                    _ => None,
                };
                let Some((source_root, user_id, client)) = identity else {
                    continue;
                };
                if !crate::chain::same_path(root.path(), source_root.path())
                    || provider.trim_end_matches('/') != client.base_url.trim_end_matches('/')
                    || client.token().is_none_or(|token| token.trim().is_empty())
                {
                    continue;
                }
                if let Some(previous) = &selected {
                    if previous.user_id != user_id || previous.client.token() != client.token() {
                        return None;
                    }
                } else {
                    selected = Some(ForkUnlockCandidate {
                        root: source_root,
                        client,
                        user_id,
                    });
                }
            }
        }
        selected
    }

    // The footer describes signing in to the app, not the individual Cube's
    // service readiness. Keep it separate from operation/claim authorization.
    fn refresh_sidebar_session_status(&mut self) {
        let signed_in = self.panes.iter().any(|(_, pane)| {
            pane.tabs.iter().any(|tab| match &tab.state {
                tab::State::App(app) => {
                    app.cache().connect_authenticated && app.shareable_connect_client().is_some()
                }
                tab::State::Home(home) => home.connect_account.is_authenticated(),
                _ => false,
            })
        });
        for (_, pane) in self.panes.iter_mut() {
            for tab in &mut pane.tabs {
                if let tab::State::App(app) = &mut tab.state {
                    app.cache_mut().sidebar_connect_signed_in = signed_in;
                }
            }
        }
    }

    fn refresh_claim_deposit_display(&mut self) {
        let displays: Vec<_> = self
            .panes
            .iter()
            .flat_map(|(_, pane)| &pane.tabs)
            .filter_map(|tab| match &tab.state {
                tab::State::App(app) => app.claim_deposit_display(),
                _ => None,
            })
            .collect();
        for (_, pane) in self.panes.iter_mut() {
            for tab in &mut pane.tabs {
                if let tab::State::App(app) = &mut tab.state {
                    app.clear_claim_deposit_display();
                    for (request, bitcoin, fork, expires) in &displays {
                        app.apply_claim_deposit_display(request, *bitcoin, *fork, *expires);
                    }
                }
            }
        }
    }

    fn update_message(&mut self, message: Message) -> Task<Message> {
        match message {
            // we get this message only once at startup
            Message::Window(id) => {
                self.window_id = id;
                // Common case: if there is an already saved screen size we reuse it
                if let (Some(id), Some(WindowConfig { width, height })) = (id, &self.window_config)
                {
                    window::resize(
                        id,
                        Size {
                            width: *width,
                            height: *height,
                        },
                    )
                // Initial startup: we maximize the screen in order to know the max usable screen area
                } else if let Some(id) = &self.window_id {
                    window::maximize(*id, true)
                } else {
                    Task::none()
                }
            }
            Message::WindowSize(monitor_size) => {
                let cloned_cfg = self.window_config.clone();
                match (cloned_cfg, &self.window_init, &self.window_id) {
                    // no previous screen size recorded && window maximized
                    (None, Some(false), Some(id)) => {
                        self.window_init = Some(true);
                        let mut batch = vec![window::maximize(*id, false)];
                        let new_size = if monitor_size.height >= 1200.0 {
                            let size = Size {
                                width: 1200.0,
                                height: 950.0,
                            };
                            batch.push(window::resize(*id, size));
                            size
                        } else {
                            batch.push(window::resize(*id, iced::window::Settings::default().size));
                            iced::window::Settings::default().size
                        };
                        self.window_config = Some(WindowConfig {
                            width: new_size.width,
                            height: new_size.height,
                        });
                        Task::batch(batch)
                    }
                    // we already have a record of the last window size and we update it
                    (Some(WindowConfig { width, height }), _, _) => {
                        if width != monitor_size.width || height != monitor_size.height {
                            if let Some(cfg) = &mut self.window_config {
                                cfg.width = monitor_size.width;
                                cfg.height = monitor_size.height;
                            }
                        }
                        Task::none()
                    }
                    // we ignore the first notification about initial window size it will always be
                    // the default one
                    _ => {
                        if self.window_init.is_none() {
                            self.window_init = Some(false);
                        }
                        Task::none()
                    }
                }
            }
            Message::CtrlC
            | Message::Event(iced::Event::Window(iced::window::Event::CloseRequested)) => {
                for (_, pane) in self.panes.iter_mut() {
                    pane.stop();
                }
                if let Some(window_config) = &self.window_config {
                    let path = GlobalSettings::path(&self.config.coincube_directory);
                    if let Err(e) = GlobalSettings::update_window_config(&path, window_config) {
                        tracing::error!("Failed to update the window config: {e}");
                    }
                }
                iced::window::latest().and_then(iced::window::close)
            }
            Message::KeyPressed(Key::Tab(shift)) => {
                if self.preferences_sheet.is_some() {
                    return if shift {
                        action::focus_previous_visible(preferences_view::CONTENT_SCROLL_ID)
                    } else {
                        action::focus_next_visible(preferences_view::CONTENT_SCROLL_ID)
                    };
                }
                log::debug!("Tab pressed!");
                if shift {
                    action::focus_previous_visible(crate::app::view::nav::shell::MIDDLE_SCROLL_ID)
                } else {
                    action::focus_next_visible(crate::app::view::nav::shell::MIDDLE_SCROLL_ID)
                }
            }
            Message::KeyPressed(Key::Escape) => {
                if self.preferences_sheet.is_some() {
                    return self.close_preferences_sheet();
                }
                let Some((pane_id, tab_id, generation, intent)) = self.focus.and_then(|pane_id| {
                    let pane = self.panes.get(pane_id)?;
                    let tab = pane.tabs.get(pane.focused_tab)?;
                    let tab::State::App(app) = &tab.state else {
                        return None;
                    };
                    Some((
                        pane_id,
                        tab.id,
                        tab.app_generation_token()?,
                        app.shell_escape_intent()?,
                    ))
                }) else {
                    return Task::none();
                };
                self.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        tab_id,
                        tab::Message::AppAsync(
                            generation,
                            Box::new(tab::Message::Run(AppMessage::View(intent))),
                        ),
                    ),
                ))
            }
            Message::Fiat(FiatMessage::GetPriceResult(price)) => {
                if price.request.origin.is_some() {
                    return Task::none();
                }
                if self
                    .global_cache
                    .pending_fiat_price_request(price.source(), price.currency())
                    != Some(&price.request)
                {
                    tracing::debug!(
                        "Ignoring fiat price result for {} from {} as it is not the last request",
                        price.currency(),
                        price.source(),
                    );
                    return Task::none();
                }
                match price.res.as_ref() {
                    Ok(res) => {
                        tracing::debug!(
                            "Fiat price request for {} from {} completed successfully: {:?}",
                            price.currency(),
                            price.source(),
                            res,
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            "Fiat price request for {} from {} returned error: {}",
                            price.currency(),
                            price.source(),
                            e
                        );
                    }
                }
                // Update the cache with the result even if there was an error.
                self.global_cache
                    .remove_fiat_price_request(price.source(), price.currency());
                self.global_cache.insert_fiat_price(price);
                Task::none()
            }
            Message::Fiat(FiatMessage::ListCurrenciesResult(
                pane_id,
                tab_id,
                source,
                instant,
                res,
            )) => {
                if let Ok(list) = res.as_ref() {
                    self.global_cache
                        .insert_currencies(source, instant, list.currencies.clone());
                }
                // Return the result to the tab even if there was an error.
                if let Some(pane) = self.panes.get_mut(pane_id) {
                    pane.update_tab_with_app_msg(
                        tab_id,
                        AppFiatMessage::ListCurrenciesResult(source, res),
                        &self.config,
                    )
                    .map(move |msg| Message::Pane(pane_id, msg))
                } else {
                    Task::none()
                }
            }
            Message::Pane(
                origin,
                pane::Message::View(pane::ViewMessage::OpenForkClaim(handoff)),
            ) => {
                let retained = self.panes.iter().find_map(|(&pane_id, pane)| {
                    pane.tabs.iter().find_map(|tab| match &tab.state {
                        tab::State::App(app)
                            if app.has_claim_pair_context()
                                && handoff.matches_root(app.datadir())
                                && app.cube_settings().id == handoff.bitcoin_cube() =>
                        {
                            Some((pane_id, tab.id))
                        }
                        _ => None,
                    })
                });
                if let Some((pane_id, tab_id)) = retained {
                    let task = self
                        .panes
                        .get_mut(pane_id)
                        .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == tab_id))
                        .map(|tab| {
                            let result = match &mut tab.state {
                                tab::State::App(app) => app.begin_claim_pair_phase(handoff),
                                _ => Err("The Claim source Cube closed.".into()),
                            };
                            tab.emit_app_task(result.unwrap_or_else(|error| {
                                Task::done(AppMessage::View(crate::app::view::Message::ShowError(
                                    error,
                                )))
                            }))
                        })
                        .unwrap_or_else(Task::none);
                    self.focus = Some(pane_id);
                    return task.map(move |message| {
                        Message::Pane(pane_id, pane::Message::tab(tab_id, message))
                    });
                }
                if let Some(pane) = self.panes.get_mut(origin) {
                    pane.update(
                        pane::Message::View(pane::ViewMessage::OpenForkClaim(handoff)),
                        &self.config,
                    )
                    .map(move |message| Message::Pane(origin, message))
                } else {
                    Task::none()
                }
            }
            Message::Pane(
                origin,
                pane::Message::View(pane::ViewMessage::OpenBitcoinClaim(handoff)),
            ) => {
                // Reuse an already-unlocked source in any pane. Opening a second
                // daemon for it could contend with the original wallet owner.
                let existing = self.panes.iter().find_map(|(pane_id, pane)| {
                    pane.tabs
                        .iter()
                        .enumerate()
                        .find_map(|(index, tab)| match &tab.state {
                            tab::State::App(app)
                                if handoff.matches_root(app.datadir())
                                    && app.cube_settings().network
                                        == crate::chain::ChainId::Bitcoin
                                    && app.cube_settings().id == handoff.bitcoin_cube() =>
                            {
                                Some((*pane_id, index, tab.id))
                            }
                            _ => None,
                        })
                });
                if let Some((pane_id, index, tab_id)) = existing {
                    if let Some(pane) = self.panes.get_mut(pane_id) {
                        if let tab::State::App(app) = &mut pane.tabs[index].state {
                            let task = app.resume_returned_bitcoin_claim(&handoff).unwrap_or_else(
                                |error| {
                                    Task::done(crate::app::Message::View(
                                        crate::app::view::Message::ShowError(error),
                                    ))
                                },
                            );
                            let task = pane.tabs[index].emit_app_task(task);
                            pane.focused_tab = index;
                            self.focus = Some(pane_id);
                            return task.map(move |message| {
                                Message::Pane(pane_id, pane::Message::tab(tab_id, message))
                            });
                        }
                    }
                }
                if let Some(pane) = self.panes.get_mut(origin) {
                    pane.update(
                        pane::Message::View(pane::ViewMessage::OpenBitcoinClaim(handoff)),
                        &self.config,
                    )
                    .map(move |message| Message::Pane(origin, message))
                } else {
                    Task::none()
                }
            }
            Message::Pane(_, pane::Message::View(pane::ViewMessage::GlobalSettingsChanged)) => {
                self.reload_all_global_settings()
            }
            // A Home sign-in re-checks the Connect session of every open Cube,
            // in every pane: a split-off Cube is still an open Cube. Each Init
            // re-enters `update`, so it may adopt the verified sibling client.
            Message::Pane(_, pane::Message::View(pane::ViewMessage::ConnectSignedIn)) => {
                let mut tasks = Vec::new();
                for (&pane_id, pane) in self.panes.iter() {
                    for tab in &pane.tabs {
                        if matches!(&tab.state, tab::State::App(_)) {
                            let tab_id = tab.id;
                            tasks.push(
                                tab.emit_app_task(Task::done(AppMessage::View(
                                    crate::app::view::Message::ConnectAccount(
                                        crate::app::view::ConnectAccountMessage::Init,
                                    ),
                                )))
                                .map(move |message| {
                                    Message::Pane(pane_id, pane::Message::tab(tab_id, message))
                                }),
                            );
                        }
                    }
                }
                Task::batch(tasks)
            }
            // Fork tabs wrap task outputs with their session generation.
            // After validation/unwrapping, global intents can arrive directly
            // as tab messages rather than through Pane's task-output mapper.
            Message::Pane(pane_id, pane::Message::Tab(_, inner))
                if matches!(inner.as_ref(), tab::Message::ConnectSignedIn) =>
            {
                self.update(Message::Pane(
                    pane_id,
                    pane::Message::View(pane::ViewMessage::ConnectSignedIn),
                ))
            }
            Message::Pane(_, pane::Message::Tab(_, inner))
                if matches!(inner.as_ref(), tab::Message::ToggleTheme) =>
            {
                self.update(Message::ToggleTheme)
            }
            Message::Pane(_, pane::Message::View(pane::ViewMessage::ToggleTheme)) => {
                self.update(Message::ToggleTheme)
            }
            Message::Pane(pane_id, pane::Message::Tab(tab_id, inner))
                if matches!(inner.as_ref(), tab::Message::OpenAppPreferences(_)) =>
            {
                let tab::Message::OpenAppPreferences(source) = *inner else {
                    unreachable!();
                };
                let current_source = self.panes.get(pane_id).and_then(|pane| {
                    pane.tabs
                        .iter()
                        .find(|tab| tab.id == tab_id)
                        .and_then(tab::Tab::preferences_source)
                });
                if current_source != Some(source) {
                    return Task::none();
                }
                let focus = Some(iced::advanced::widget::Id::new(match source {
                    tab::PreferencesSource::Home => "tenshu.launcher.catalog:control:settings",
                    tab::PreferencesSource::App(_) => {
                        crate::app::view::nav::shell::APP_PREFERENCES_ID
                    }
                }));
                let Some(origin) = self.overlay_origin(focus) else {
                    return Task::none();
                };
                if origin.pane != pane_id || origin.tab_id != tab_id || origin.source != source {
                    return Task::none();
                }
                self.preferences_sheet = Some(PreferencesSheet {
                    origin,
                    page: PreferencesPageState::default(),
                });
                iced_runtime::task::widget(iced::advanced::widget::operation::focusable::focus(
                    iced::advanced::widget::Id::new("tenshu.preferences.close"),
                ))
            }
            Message::Pane(pane_id, pane::Message::Tab(tab_id, inner))
                if matches!(
                    inner.as_ref(),
                    tab::Message::LockCubeToLauncher | tab::Message::LockCube
                ) =>
            {
                let to_launcher = matches!(inner.as_ref(), tab::Message::LockCubeToLauncher)
                    || self.panes.get(pane_id).is_some_and(|pane| {
                        let owner =
                            pane.tabs.iter().find(|tab| tab.id == tab_id).and_then(
                                |tab| match &tab.state {
                                    tab::State::App(app) => Some(app.cache().app_generation),
                                    _ => None,
                                },
                            );
                        matches!(
                            pane.tabs.get(pane.focused_tab).map(|tab| &tab.state),
                            Some(tab::State::Home(_))
                        ) || owner.is_some_and(|owner| {
                            pane.tabs
                                .iter()
                                .any(|tab| tab.claim_open_owner() == Some(owner))
                        })
                    });
                let identity = self
                    .panes
                    .get(pane_id)
                    .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == tab_id))
                    .and_then(|tab| match &tab.state {
                        tab::State::App(app) => Some((
                            app.cache().app_generation,
                            app.datadir().clone(),
                            app.cube_settings().network,
                            app.cube_settings().id.clone(),
                        )),
                        _ => None,
                    });
                let Some((owner, root, chain, cube_id)) = identity else {
                    return Task::none();
                };
                let mut empty_panes = Vec::new();
                for (&other_id, pane) in self.panes.iter_mut() {
                    pane.close_tabs_matching(|sibling| {
                        !(other_id == pane_id && sibling.id == tab_id)
                            && (sibling.claim_open_owner() == Some(owner)
                                || sibling.matches_cube_identity(&root, chain, &cube_id)
                                || matches!(
                                    sibling.state,
                                    tab::State::App(_)
                                        | tab::State::Loader(_)
                                        | tab::State::Installer(_)
                                        | tab::State::Login(_)
                                        | tab::State::PinEntry(_)
                                        | tab::State::PasskeyUnlock(_)
                                ))
                    });
                    if pane.tabs.is_empty() {
                        empty_panes.push(other_id);
                    }
                }
                for empty in empty_panes {
                    self.panes.close(empty);
                    if self.focus == Some(empty) {
                        self.focus = Some(pane_id);
                    }
                }
                if let Some(pane) = self.panes.get_mut(pane_id) {
                    if let Some(index) = pane.tabs.iter().position(|tab| tab.id == tab_id) {
                        if matches!(pane.tabs[index].state, tab::State::App(_)) {
                            if !to_launcher {
                                crate::app::session::close();
                                self.focus = Some(pane_id);
                                return pane
                                    .update(pane::Message::tab(tab_id, *inner), &self.config)
                                    .map(move |message| Message::Pane(pane_id, message));
                            }
                            pane.close_tab(index); // waits for daemon stop
                            crate::app::session::close(); // zeroizes signer/PIN
                            self.focus = Some(pane_id);
                            let task = pane.focus_launcher(&self.config);
                            // Lock makes the Launcher the primary tab even if
                            // a legacy layout retained an unrelated Cube.
                            if pane.focused_tab > 0 {
                                let launcher = pane.tabs.remove(pane.focused_tab);
                                pane.tabs.insert(0, launcher);
                                pane.focused_tab = 0;
                            }
                            return task.map(move |message| Message::Pane(pane_id, message));
                        }
                    }
                }
                Task::none()
            }
            Message::Pane(pane_id, pane::Message::Tab(tab_id, inner))
                if matches!(inner.as_ref(), tab::Message::DuressActivated { .. })
                    || matches!(
                        inner.as_ref(),
                        tab::Message::Run(AppMessage::View(
                            crate::app::view::Message::DuressLockRemote
                        ))
                    ) =>
            {
                let remote = matches!(
                    inner.as_ref(),
                    tab::Message::Run(AppMessage::View(
                        crate::app::view::Message::DuressLockRemote
                    ))
                );
                let result = {
                    let Some(pane) = self.panes.get_mut(pane_id) else {
                        return Task::none();
                    };
                    let Some(index) = pane.tabs.iter().position(|tab| tab.id == tab_id) else {
                        return Task::none();
                    };
                    if remote && !matches!(pane.tabs[index].state, tab::State::App(_)) {
                        return Task::none();
                    }
                    pane.tabs[index].stop();
                    pane.update(pane::Message::Tab(tab_id, inner), &self.config)
                };
                // The wipe is complete: do not leave a Launcher or another
                // Cube surface visible above the cryptic duress state.
                for (&other_id, other_pane) in self.panes.iter_mut() {
                    other_pane.close_tabs_matching(|tab| other_id != pane_id || tab.id != tab_id);
                }
                crate::app::session::close();
                crate::app::claim_intent::clear();
                crate::app::split_intent::clear();
                if let Some(pane) = self.panes.get_mut(pane_id) {
                    pane.focused_tab = pane
                        .tabs
                        .iter()
                        .position(|tab| tab.id == tab_id)
                        .unwrap_or(0);
                }
                self.focus = Some(pane_id);
                result.map(move |message| Message::Pane(pane_id, message))
            }
            Message::Pane(_, pane::Message::View(pane::ViewMessage::SplitTab(_))) => Task::none(),
            Message::Pane(pane_id, pane::Message::View(pane::ViewMessage::CloseTab(i))) => {
                if let Some(pane) = self.panes.get_mut(pane_id) {
                    let _ = pane
                        .update(
                            pane::Message::View(pane::ViewMessage::CloseTab(i)),
                            &self.config,
                        )
                        .map(move |msg| Message::Pane(pane_id, msg));
                    if pane.tabs.is_empty() {
                        self.panes.close(pane_id);
                        if self.focus == Some(pane_id) {
                            self.focus = None;
                        }
                    }
                }
                if !self.panes.iter().any(|(_, p)| !p.tabs.is_empty()) {
                    return iced::window::latest().and_then(iced::window::close);
                }
                Task::none()
            }
            // In case of cube deletion, remove any tab where the cube/wallet is currently running.
            Message::Pane(p, pane::Message::Tab(t, inner))
                if matches!(inner.as_ref(), tab::Message::Launch(_)) =>
            {
                let tab::Message::Launch(msg) = *inner else {
                    unreachable!();
                };
                let mut tasks = Vec::new();
                let public_exit = matches!(
                    msg,
                    home::Message::Run(..)
                        | home::Message::RunFromFloor(..)
                        | home::Message::RunToRecovery(..)
                        | home::Message::RunClaim(..)
                        | home::Message::Install(..)
                        | home::Message::InstallForSplit(..)
                );
                if public_exit {
                    let admission = self
                        .panes
                        .get(p)
                        .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == t))
                        .map_or_else(
                            || Err("The Launcher destination closed.".to_string()),
                            |tab| tab.validate_home_exit(&msg),
                        );
                    if let Err(error) = admission {
                        if let Some(tab::State::Home(home)) = self
                            .panes
                            .get_mut(p)
                            .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == t))
                            .map(|tab| &mut tab.state)
                        {
                            home.set_error(error);
                        }
                        return Task::none();
                    }
                }
                let mut retain_claim_source = false;
                let mut staged_split = None;
                if let Some((root, chain, cube, intent)) = match &msg {
                    home::Message::Run(root, _, chain, cube)
                    | home::Message::RunFromFloor(root, _, chain, cube, _)
                    | home::Message::RunToRecovery(root, _, chain, cube) => {
                        Some((root, chain, cube, None))
                    }
                    home::Message::RunClaim(root, _, chain, cube, intent) => {
                        Some((root, chain, cube, Some(intent)))
                    }
                    _ => None,
                } {
                    let claim_route = intent.is_some_and(|intent| {
                        self.panes.iter().any(|(_, pane)| {
                            pane.tabs.iter().any(|tab| match &tab.state {
                                tab::State::App(app) => {
                                    crate::app::session::current_cube_id().as_deref()
                                        == Some(app.cube_settings().id.as_str())
                                        && intent.validates(app, root, *chain, cube)
                                }
                                _ => false,
                            })
                        })
                    });
                    retain_claim_source = claim_route;
                    if intent.is_some() && !claim_route {
                        crate::app::claim_intent::clear();
                        crate::app::split_intent::clear();
                        if let Some(tab::State::Home(home)) = self
                            .panes
                            .get_mut(p)
                            .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == t))
                            .map(|tab| &mut tab.state)
                        {
                            home.set_error("The Claim source or paired Cube changed. Open Claim again from the source Cube.");
                        }
                        return Task::none();
                    }
                    if let Some(intent) = intent {
                        let already_opening = self.panes.iter().any(|(_, pane)| {
                            pane.tabs
                                .iter()
                                .any(|tab| tab.claim_open_owner() == Some(intent.owner))
                        });
                        if already_opening {
                            return Task::none();
                        }
                        // A restricted Claim may keep only its exact source
                        // App. Legacy other Cube surfaces cannot share the
                        // source's one process session through this exception.
                        self.retire_other_public_owners(intent.owner);
                    }
                    if !claim_route && intent.is_none() && chain.is_blake2b() {
                        staged_split = crate::app::split_intent::take_for_open(&cube.id, *chain);
                    }
                    // Opening the same Cube from the Launcher resumes its live
                    // App. In particular this must not ask for its PIN again.
                    let secret_owners = self
                        .panes
                        .iter()
                        .flat_map(|(_, pane)| pane.tabs.iter())
                        .filter(|tab| {
                            matches!(
                                tab.state,
                                tab::State::App(_)
                                    | tab::State::Loader(_)
                                    | tab::State::Installer(_)
                                    | tab::State::Login(_)
                                    | tab::State::PinEntry(_)
                                    | tab::State::PasskeyUnlock(_)
                            )
                        })
                        .count();
                    // A retained App can resume only while it is the sole
                    // public owner of the exact live process session. Legacy
                    // duplicate tabs, or an App outliving its session, go
                    // through the normal close-then-unlock admission instead.
                    let existing = (secret_owners == 1
                        && crate::app::session::current_cube_id().as_deref()
                            == Some(cube.id.as_str()))
                    .then(|| {
                        self.panes.iter().find_map(|(&pane_id, pane)| {
                            pane.tabs
                                .iter()
                                .enumerate()
                                .find_map(|(index, tab)| match &tab.state {
                                    tab::State::App(app)
                                        if app.datadir() == root
                                            && app.cube_settings().network == *chain
                                            && app.cube_settings().id == cube.id =>
                                    {
                                        Some((pane_id, index))
                                    }
                                    _ => None,
                                })
                        })
                    })
                    .flatten();
                    if let Some((pane_id, index)) = existing {
                        self.focus = Some(pane_id);
                        if let Some(pane) = self.panes.get_mut(pane_id) {
                            if let Some(split) = staged_split.take() {
                                if let tab::State::App(app) = &mut pane.tabs[index].state {
                                    let task = app.resume_foreign_split_intent(split);
                                    let tab_id = pane.tabs[index].id;
                                    let task = pane.tabs[index].emit_app_task(task);
                                    pane.focused_tab = index;
                                    return task.map(move |message| {
                                        Message::Pane(pane_id, pane::Message::tab(tab_id, message))
                                    });
                                }
                            }
                            if let Some(home::ClaimRoute::Return(handoff)) =
                                intent.map(|intent| &intent.route)
                            {
                                if let tab::State::App(app) = &mut pane.tabs[index].state {
                                    let task = app
                                        .resume_returned_bitcoin_claim(handoff)
                                        .unwrap_or_else(|error| {
                                            Task::done(AppMessage::View(
                                                crate::app::view::Message::ShowError(error),
                                            ))
                                        });
                                    let tab_id = pane.tabs[index].id;
                                    let task = pane.tabs[index].emit_app_task(task);
                                    pane.focused_tab = index;
                                    return task.map(move |message| {
                                        Message::Pane(pane_id, pane::Message::tab(tab_id, message))
                                    });
                                }
                            }
                            if matches!(&msg, home::Message::RunToRecovery(..)) {
                                let tab_id = pane.tabs[index].id;
                                let focus = pane
                                    .update(
                                        pane::Message::View(pane::ViewMessage::FocusTab(index)),
                                        &self.config,
                                    )
                                    .map(move |message| Message::Pane(pane_id, message));
                                let recovery = pane.tabs[index]
                                    .emit_app_task(Task::done(AppMessage::View(
                                        crate::app::view::Message::Menu(
                                            crate::app::menu::Menu::Cube(
                                                crate::app::menu::CubeSubMenu::Settings(
                                                    crate::app::menu::CubeSettingsOption::Recovery,
                                                ),
                                            ),
                                        ),
                                    )))
                                    .map(move |message| {
                                        Message::Pane(pane_id, pane::Message::tab(tab_id, message))
                                    });
                                return Task::batch([focus, recovery]);
                            }
                            return pane
                                .update(
                                    pane::Message::View(pane::ViewMessage::FocusTab(index)),
                                    &self.config,
                                )
                                .map(move |message| Message::Pane(pane_id, message));
                        }
                    }

                    // Claim is an existing paired-wallet operation: its source
                    // must remain available to sign the counterpart. Ordinary
                    // Cube switching closes the old wallet and secret session
                    // synchronously before the new Home::Run reaches unlock.
                }
                if let home::Message::InstallForSplit(root, _) = &msg {
                    let imported_operation = self.panes.get(p).is_some_and(|pane| {
                        pane.tabs.iter().find(|tab| tab.id == t).is_some_and(|tab| {
                            matches!(&tab.state, tab::State::Home(home) if home.imported_claim_request().is_some())
                        })
                    });
                    retain_claim_source = self.panes.get(p).is_some_and(|pane| {
                        pane.tabs.iter().find(|tab| tab.id == t).is_some_and(|tab| {
                            let tab::State::Home(home) = &tab.state else {
                                return false;
                            };
                            let (Some(owner), Some(request)) =
                                (home.imported_claim_owner(), home.imported_claim_request())
                            else {
                                return false;
                            };
                            self.panes.iter().any(|(_, pane)| {
                                pane.tabs.iter().any(|tab| match &tab.state {
                                    tab::State::App(app) => {
                                        app.cache().app_generation == owner
                                            && app.datadir() == root
                                            && app.cube_settings().id == request.source.cube_id
                                            && crate::app::session::current_cube_id().as_deref()
                                                == Some(app.cube_settings().id.as_str())
                                            && app.prepare_imported_claim().is_ok_and(
                                                |(current, client)| {
                                                    current.source == request.source
                                                        && request.validate(root, &client).is_ok()
                                                },
                                            )
                                    }
                                    _ => false,
                                })
                            })
                        })
                    });
                    if imported_operation && !retain_claim_source {
                        if let Some(tab::State::Home(home)) = self
                            .panes
                            .get_mut(p)
                            .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == t))
                            .map(|tab| &mut tab.state)
                        {
                            home.set_error("The imported Claim source closed or changed. Open Claim from that Cube again.");
                        }
                        return Task::none();
                    }
                    if retain_claim_source {
                        if let Some(owner) = self.panes.get(p).and_then(|pane| {
                            pane.tabs.iter().find(|tab| tab.id == t).and_then(|tab| {
                                match &tab.state {
                                    tab::State::Home(home) => home.imported_claim_owner(),
                                    _ => None,
                                }
                            })
                        }) {
                            self.retire_other_public_owners(owner);
                        }
                    }
                }
                if public_exit && !retain_claim_source {
                    self.retire_public_owners();
                    if let Some(intent) = staged_split {
                        if let Some(tab) = self
                            .panes
                            .get_mut(p)
                            .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == t))
                        {
                            tab.stage_split_intent(intent);
                        }
                    }
                }
                if let home::Message::View(home::ViewMessage::DeleteCube(
                    home::DeleteCubeMessage::Confirm(..),
                )) = &msg
                {
                    self.retire_public_owners();
                    // When a cube is deleted, close all App and Loader tabs since they won't be valid anymore
                    let mut panes_to_close = Vec::<pane_grid::Pane>::new();
                    for (id, pane) in self.panes.iter_mut() {
                        pane.close_tabs_matching(|tab| {
                            matches!(tab.state, tab::State::App(_) | tab::State::Loader(_))
                        });
                        if pane.tabs.is_empty() {
                            panes_to_close.push(*id);
                        }
                    }
                    for id in panes_to_close {
                        self.panes.close(id);
                    }
                    for (&id, pane) in self.panes.iter() {
                        for tab in &pane.tabs {
                            if let tab::State::Home(l) = &tab.state {
                                let tab_id = tab.id;
                                tasks.push(l.reload().map(move |msg| {
                                    Message::Pane(
                                        id,
                                        pane::Message::tab(tab_id, tab::Message::Launch(msg)),
                                    )
                                }));
                            }
                        }
                    }
                }

                if let Some(pane) = self.panes.get_mut(p) {
                    tasks.push(
                        pane.update(
                            pane::Message::tab(t, tab::Message::Launch(msg)),
                            &self.config,
                        )
                        .map(move |msg| Message::Pane(p, msg)),
                    );
                }

                Task::batch(tasks)
            }
            Message::Pane(loader_pane, pane::Message::Tab(loader_tab, inner))
                if matches!(inner.as_ref(), tab::Message::ClaimResourcesLoaded(_)) =>
            {
                let tab::Message::ClaimResourcesLoaded(resources) = *inner else {
                    unreachable!();
                };
                let mut resources = *resources;
                let loader_present = self.panes.get(loader_pane).is_some_and(|pane| {
                    pane.tabs.iter().any(|tab| {
                        tab.id == loader_tab && matches!(tab.state, tab::State::Loader(_))
                    })
                });
                let source = self.panes.iter().find_map(|(&pane_id, pane)| {
                    pane.tabs.iter().find_map(|tab| match &tab.state {
                        tab::State::App(app)
                            if app.cache().app_generation == resources.intent.owner =>
                        {
                            Some((pane_id, tab.id))
                        }
                        _ => None,
                    })
                });
                let Some((source_pane, source_tab)) = source.filter(|_| loader_present) else {
                    resources.stop_owned();
                    crate::app::claim_intent::clear();
                    crate::app::split_intent::clear();
                    if let Some(pane) = self.panes.get_mut(loader_pane) {
                        if let Some(index) = pane.tabs.iter().position(|tab| tab.id == loader_tab) {
                            pane.close_tab(index);
                        }
                    }
                    return Task::none();
                };
                let admission = self
                    .panes
                    .get_mut(source_pane)
                    .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == source_tab))
                    .and_then(|tab| match &mut tab.state {
                        tab::State::App(app) => Some(app.admit_claim_pair_context(
                            &resources.intent,
                            resources.wallet.clone(),
                            resources.daemon.clone(),
                            resources.cache.clone(),
                            resources.settings.clone(),
                            resources.client.clone(),
                            resources.verified_account,
                            resources.bitcoind.clone(),
                        )),
                        _ => None,
                    })
                    .unwrap_or_else(|| Err("The Claim source Cube closed.".into()));
                if let Err(error) = admission {
                    resources.stop_owned();
                    crate::app::split_intent::clear();
                    if let Some(pane) = self.panes.get_mut(loader_pane) {
                        if let Some(index) = pane.tabs.iter().position(|tab| tab.id == loader_tab) {
                            pane.close_tab(index);
                        }
                    }
                    self.focus = Some(source_pane);
                    return self
                        .panes
                        .get(source_pane)
                        .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == source_tab))
                        .map_or_else(Task::none, |tab| {
                            tab.emit_app_task(Task::done(AppMessage::View(
                                crate::app::view::Message::ShowError(error),
                            )))
                            .map(move |message| {
                                Message::Pane(source_pane, pane::Message::tab(source_tab, message))
                            })
                        });
                }
                if let Some(pane) = self.panes.get_mut(loader_pane) {
                    if let Some(index) = pane.tabs.iter().position(|tab| tab.id == loader_tab) {
                        if let Some(mut tab) = pane.remove_tab(index) {
                            tab.stop();
                        }
                    }
                }
                self.focus = Some(source_pane);
                if let Some(pane) = self.panes.get_mut(source_pane) {
                    if let Some(index) = pane.tabs.iter().position(|tab| tab.id == source_tab) {
                        pane.focused_tab = index;
                    }
                }
                let start = match resources.intent.route {
                    home::ClaimRoute::Begin(handoff) => self
                        .panes
                        .get_mut(source_pane)
                        .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == source_tab))
                        .and_then(|tab| match &mut tab.state {
                            tab::State::App(app) => Some(if app.claim_pair_request().is_some() {
                                Ok(Task::done(AppMessage::View(
                                    crate::app::view::Message::Claim(
                                        crate::app::view::ClaimMessage::CheckFunding,
                                    ),
                                )))
                            } else {
                                app.resume_returned_bitcoin_claim(&handoff)
                            }),
                            _ => None,
                        })
                        .unwrap_or_else(|| Err("The Claim source Cube closed.".into())),
                    home::ClaimRoute::Continue(handoff) => self
                        .panes
                        .get_mut(source_pane)
                        .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == source_tab))
                        .and_then(|tab| match &mut tab.state {
                            tab::State::App(app) => Some(app.begin_claim_pair_phase(handoff)),
                            _ => None,
                        })
                        .unwrap_or_else(|| Err("The Claim source Cube closed.".into())),
                    home::ClaimRoute::Imported(request) => self
                        .panes
                        .get_mut(source_pane)
                        .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == source_tab))
                        .and_then(|tab| match &mut tab.state {
                            tab::State::App(app) => Some(app.begin_imported_split_context(request)),
                            _ => None,
                        })
                        .unwrap_or_else(|| Err("The imported Split source Cube closed.".into())),
                    home::ClaimRoute::Return(_) => Ok(Task::none()),
                };
                match start {
                    Ok(task) => {
                        let task = self
                            .panes
                            .get(source_pane)
                            .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == source_tab))
                            .map_or_else(Task::none, |tab| tab.emit_app_task(task));
                        task.map(move |message| {
                            Message::Pane(source_pane, pane::Message::tab(source_tab, message))
                        })
                    }
                    Err(error) => {
                        if let Some(tab::State::App(app)) = self
                            .panes
                            .get_mut(source_pane)
                            .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == source_tab))
                            .map(|tab| &mut tab.state)
                        {
                            app.revoke_claim();
                        }
                        log::warn!("Claim pair admission could not start: {error}");
                        Task::none()
                    }
                }
            }
            Message::Pane(i, msg) => {
                // Bind the Claim's Bitcoin tab to its exact, currently open
                // BTCB2 wallet immediately before a deposit or signed-pair
                // broadcast check. Fork Cubes
                // intentionally have no generic JSON-RPC socket; borrowing
                // their in-process daemon keeps that restriction intact.
                if let Some(tab_id) = match &msg {
                    pane::Message::Tab(tab_id, inner) if matches!(inner.as_ref(), tab::Message::Run(message) if refreshes_claim_pair(message)) => {
                        Some(tab_id)
                    }
                    _ => None,
                } {
                    let (fork_daemon, begin_pair) = self
                        .panes
                        .get(i)
                        .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == *tab_id))
                        .and_then(|tab| match &tab.state {
                            tab::State::App(app) => {
                                let request = app.claim_pair_request()?;
                                let binding = app.claim_fork_daemon(&request);
                                let begin = if binding.is_none() && !app.has_claim_pair_context() {
                                    app.claim_pair_handoff()
                                } else {
                                    None
                                };
                                Some((binding, begin))
                            }
                            _ => None,
                        })
                        .unwrap_or((None, None));
                    if let Some(tab::State::App(app)) = self
                        .panes
                        .get_mut(i)
                        .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == *tab_id))
                        .map(|tab| &mut tab.state)
                    {
                        app.bind_claim_fork_daemon(fork_daemon);
                    }
                    if matches!(
                        &msg,
                        pane::Message::Tab(_, inner)
                            if matches!(inner.as_ref(), tab::Message::Run(
                                AppMessage::View(crate::app::view::Message::Claim(
                                    crate::app::view::ClaimMessage::CheckFunding
                                ))
                            ))
                    ) {
                        if let Some(handoff) = begin_pair {
                            return if let Some(pane) = self.panes.get_mut(i) {
                                pane.update(
                                    pane::Message::View(pane::ViewMessage::OpenForkPair(handoff)),
                                    &self.config,
                                )
                                .map(move |message| Message::Pane(i, message))
                            } else {
                                Task::none()
                            };
                        }
                    }
                }
                match msg {
                    // Handle ListCurrencies requests separately.
                    pane::Message::Tab(tab_id, inner)
                        if matches!(
                            inner.as_ref(),
                            tab::Message::Run(AppMessage::Fiat(AppFiatMessage::ListCurrencies(_)))
                        ) =>
                    {
                        let tab::Message::Run(AppMessage::Fiat(AppFiatMessage::ListCurrencies(
                            source,
                        ))) = *inner
                        else {
                            tracing::error!("Unexpected message type after unboxing");
                            return Task::none();
                        };
                        // BTCB2 uses only Connect pricing. Currency preferences do not
                        // authorize a Bitcoin/aggregator currency-list request either.
                        let btcb2 = self
                            .panes
                            .get(i)
                            .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == tab_id))
                            .and_then(|tab| tab.cube_settings())
                            .is_some_and(|cube| cube.network.is_blake2b());
                        if btcb2 {
                            if let Some(pane) = self.panes.get_mut(i) {
                                return pane
                                    .update_tab_with_app_msg(
                                        tab_id,
                                        AppFiatMessage::ListCurrenciesResult(
                                            source,
                                            Ok(ListCurrenciesResult {
                                                currencies: Currency::ALL.to_vec(),
                                            }),
                                        ),
                                        &self.config,
                                    )
                                    .map(move |msg| Message::Pane(i, msg));
                            }
                            return Task::none();
                        }
                        // If we already have a fresh list of currencies for this source, return it directly to the tab.
                        if let Some(fresh_list) = self.global_cache.fresh_currencies(source) {
                            tracing::debug!("Using cached currencies list for {}", source,);
                            if let Some(pane) = self.panes.get_mut(i) {
                                return pane
                                    .update_tab_with_app_msg(
                                        tab_id,
                                        AppFiatMessage::ListCurrenciesResult(
                                            source,
                                            Ok(ListCurrenciesResult {
                                                currencies: fresh_list.clone(),
                                            }),
                                        ),
                                        &self.config,
                                    )
                                    .map(move |msg| Message::Pane(i, msg));
                            }
                        } else {
                            tracing::debug!("Requesting list of currencies from {}", source);
                            return Task::perform(
                                async move {
                                    let client = PriceClient::default_from_source(source);
                                    (
                                        tab_id,
                                        source,
                                        Instant::now(),
                                        client.list_currencies().await,
                                    )
                                },
                                move |(tab_id, source, now, res)| {
                                    FiatMessage::ListCurrenciesResult(i, tab_id, source, now, res)
                                        .into()
                                },
                            );
                        }
                    }
                    _ => {
                        if let Some(pane) = self.panes.get_mut(i) {
                            return pane
                                .update(msg, &self.config)
                                .map(move |msg| Message::Pane(i, msg));
                        }
                    }
                }
                Task::none()
            }
            Message::Clicked(pane) => {
                self.focus = Some(pane);
                Task::none()
            }
            Message::Resized(_) | Message::Dragged(_) => Task::none(),
            Message::Preferences(request, result) => {
                let before = self
                    .preferences
                    .accepted()
                    .map(|accepted| accepted.preferences);
                let applied = matches!(
                    self.preferences.complete(&request, result),
                    SaveOutcome::Applied(_)
                );
                self.publish_balance_privacy();
                if applied {
                    let after = self
                        .preferences
                        .accepted()
                        .map(|accepted| accepted.preferences);
                    if before.map(|prefs| prefs.theme) != after.map(|prefs| prefs.theme) {
                        self.publish_appearance();
                    }
                    if launcher_presentation_changed(before, after) {
                        self.refresh_all_launcher_modes();
                    }
                    if needs_global_settings_reload(before, after) {
                        return self.reload_all_global_settings();
                    }
                }
                Task::none()
            }
            Message::ToggleTheme => {
                use crate::app::settings::preferences::{PreferenceChange, ThemePreference};
                use coincube_ui::theme::palette::ThemeMode;
                let next = match self.theme_mode {
                    ThemeMode::Dark => ThemePreference::Light,
                    ThemeMode::Light => ThemePreference::Dark,
                };
                self.change_preference(PreferenceChange::Theme(next))
            }
            Message::SystemTheme(mode) => {
                self.system_theme = match mode {
                    iced::theme::Mode::Light => Some(coincube_ui::theme::palette::ThemeMode::Light),
                    iced::theme::Mode::Dark => Some(coincube_ui::theme::palette::ThemeMode::Dark),
                    iced::theme::Mode::None => None,
                };
                self.publish_appearance();
                Task::none()
            }
            Message::PreferencesSheet(message) => self.preferences_sheet_message(message),
            Message::Tick => {
                let system_reduces_motion =
                    crate::app::settings::system_accessibility::reduces_motion();
                self.system_reduces_motion = system_reduces_motion;
                for (_, pane) in self.panes.iter_mut() {
                    for tab in &mut pane.tabs {
                        tab.set_system_reduces_motion(system_reduces_motion);
                    }
                }
                let privacy_refresh = self
                    .preferences
                    .begin_refresh_if_due()
                    .map(|request| self.run_preferences(request));
                if privacy_refresh.is_some() {
                    self.publish_balance_privacy();
                }
                let mut tasks = vec![];
                if let Some(refresh) = privacy_refresh {
                    tasks.push(refresh);
                }

                // These are the required (source, currency) pairs for which the global price is stale.
                let mut stale_pairs = HashSet::<(PriceSource, Currency)>::new();
                // These are the tabs that need the cached global price.
                let mut need_cached = HashMap::<(pane_grid::Pane, usize), FiatPrice>::new();
                // Tabs that need a fresh USD price for USDt→sats conversion.
                let mut need_usd_cached: Vec<(pane_grid::Pane, usize, FiatPrice)> = Vec::new();
                for (&pane_id, pane) in self.panes.iter() {
                    for tab in pane.tabs.iter() {
                        // BTCB2 has an authenticated, generation-bound poll in App.
                        // Never insert its requests into the shared Bitcoin cache.
                        if tab
                            .cube_settings()
                            .is_some_and(|cube| cube.network.is_blake2b())
                        {
                            continue;
                        }
                        let fiat_sett = tab.cube_settings().and_then(|cs| cs.fiat_price.as_ref());

                        // When fiat display is enabled, fetch the user's selected currency.
                        if let Some(sett) = fiat_sett.filter(|s| s.is_enabled) {
                            if let Some(fresh_price) = self
                                .global_cache
                                .fresh_fiat_price(sett.source, sett.currency)
                            {
                                if !tab.cache().and_then(|c| c.fiat_price.as_ref()).is_some_and(
                                    |tab_price| tab_price.request == fresh_price.request,
                                ) {
                                    need_cached.insert((pane_id, tab.id), fresh_price.clone());
                                }
                            } else if self
                                .global_cache
                                .pending_fiat_price_request(sett.source, sett.currency)
                                .is_none()
                            {
                                stale_pairs.insert((sett.source, sett.currency));
                            }
                        }

                        // Always fetch BTC/USD for USDt→sats conversion, even when
                        // fiat display is disabled. Use the configured source if
                        // available, otherwise fall back to the default source.
                        let usd_source = fiat_sett
                            .map(|s| s.source)
                            .unwrap_or(PriceSource::default());
                        if let Some(fresh_usd) = self
                            .global_cache
                            .fresh_fiat_price(usd_source, Currency::USD)
                        {
                            if tab.cache().and_then(|c| c.btc_usd_price).is_none() {
                                need_usd_cached.push((pane_id, tab.id, fresh_usd.clone()));
                            }
                        } else if self
                            .global_cache
                            .pending_fiat_price_request(usd_source, Currency::USD)
                            .is_none()
                        {
                            stale_pairs.insert((usd_source, Currency::USD));
                        }
                    }
                }
                for (source, currency) in stale_pairs {
                    let request = FiatPriceRequest::new(source, currency);
                    // Store request immediately to avoid multiple requests for the same pair.
                    self.global_cache.insert_fiat_price_request(request);
                    tasks.push(Task::perform(request.send_default(), |res| {
                        FiatMessage::GetPriceResult(res).into()
                    }));
                }
                for ((pane_id, tab_id), global_price) in need_cached {
                    if let Some(pane) = self.panes.get_mut(pane_id) {
                        // Return the cached global price to the tab.
                        tracing::debug!(
                            "Updating tab {} in pane {:?} with cached fiat price {:?}",
                            tab_id,
                            pane_id,
                            global_price
                        );
                        tasks.push(
                            pane.update_tab_with_app_msg(
                                tab_id,
                                AppFiatMessage::GetPriceResult(global_price),
                                &self.config,
                            )
                            .map(move |msg| Message::Pane(pane_id, msg)),
                        );
                    }
                }
                // Distribute fresh USD prices for USDt→sats conversion.
                for (pane_id, tab_id, usd_price) in need_usd_cached {
                    if let Some(pane) = self.panes.get_mut(pane_id) {
                        tasks.push(
                            pane.update_tab_with_app_msg(
                                tab_id,
                                AppFiatMessage::GetPriceResult(usd_price),
                                &self.config,
                            )
                            .map(move |msg| Message::Pane(pane_id, msg)),
                        );
                    }
                }
                tasks.extend(
                    self.panes
                        .iter_mut()
                        .map(|(&id, p)| p.on_tick().map(move |msg| Message::Pane(id, msg))),
                );

                Task::batch(tasks)
            }
            _ => Task::none(),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut vec = vec![
            iced::time::every(Duration::from_secs(1)).map(|_| Message::Tick),
            iced::system::theme_changes().map(Message::SystemTheme),
            iced::event::listen_with(|event, status, _| {
                // The idle auto-lock's only source of truth for "someone is at
                // the machine". It must come from real input: message traffic
                // does not work, because the 1 Hz tick spawns follow-up
                // messages continuously and would defer the lock forever.
                //
                // Done as a side effect rather than by emitting a message —
                // cursor movement alone would flood the queue at hundreds of
                // messages a second for a value nothing else reads.
                if is_user_input(&event) {
                    crate::app::session::touch();
                }
                if let Some(key) = ignored_shell_key(&event, status) {
                    return Some(Message::KeyPressed(key));
                }
                match (&event, status) {
                    (
                        iced::Event::Window(iced::window::Event::CloseRequested),
                        event::Status::Ignored,
                    ) => Some(Message::Event(event)),
                    (iced::Event::Window(iced::window::Event::Resized(size)), _) => {
                        Some(Message::WindowSize(*size))
                    }
                    _ => None,
                }
            }),
        ];
        let visible_pane = self
            .panes
            .iter()
            .find(|(id, _)| Some(**id) == self.focus)
            .or_else(|| self.panes.iter().next())
            .map(|(id, _)| *id);
        for (id, pane) in self.panes.iter() {
            vec.push(
                pane.subscription(Some(*id) == visible_pane)
                    .with(*id)
                    .map(|(id, msg)| Message::Pane(id, msg)),
            );
        }
        Subscription::batch(vec)
    }

    pub fn view(&self) -> Element<Message> {
        if privacy_render_needs_cover(
            &self.preferences,
            &GlobalSettings::path(&self.config.coincube_directory),
        ) {
            return iced::widget::container(iced::widget::text("Updating balance privacy…"))
                .width(iced::Length::Fill)
                .height(iced::Length::Fill)
                .into();
        }
        let (&id, pane) = self
            .panes
            .iter()
            .find(|(id, _)| Some(**id) == self.focus)
            .or_else(|| self.panes.iter().next())
            .expect("GUI always owns its primary pane");
        let background = pane.view().map(move |message| Message::Pane(id, message));
        if let Some(sheet) = &self.preferences_sheet {
            if self.overlay_origin_is_current(&sheet.origin) {
                let foreground = preferences_view::view(&sheet.page, self.preferences_view_model())
                    .map(Message::PreferencesSheet);
                return compose_overlay(background, foreground);
            }
        }
        background
    }

    pub fn scale_factor(&self) -> f32 {
        1.0
    }

    pub fn theme(&self) -> coincube_ui::theme::Theme {
        coincube_ui::theme::Theme::from_mode(self.theme_mode)
    }
}

#[derive(Clone)]
pub struct Config {
    pub coincube_directory: CoincubeDirectory,
    network: Option<bitcoin::Network>,
}

impl Config {
    pub fn new(coincube_directory: CoincubeDirectory, network: Option<bitcoin::Network>) -> Self {
        Self {
            coincube_directory,
            network,
        }
    }
}

/// Where an overlay (App Preferences, Connect) was opened from: the exact
/// retained pane and tab, the App generation when that tab is a Cube, and the
/// control to refocus when the overlay closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayOrigin {
    pub pane: pane_grid::Pane,
    pub tab_id: usize,
    pub source: tab::PreferencesSource,
    pub focus: Option<iced::advanced::widget::Id>,
}

struct PreferencesSheet {
    origin: OverlayOrigin,
    page: PreferencesPageState,
}

impl GUI {
    /// The pane `view` renders: the focused one, else the first.
    fn visible_pane(&self) -> Option<(pane_grid::Pane, &pane::Pane)> {
        self.panes
            .iter()
            .find(|(id, _)| Some(**id) == self.focus)
            .or_else(|| self.panes.iter().next())
            .map(|(id, pane)| (*id, pane))
    }

    /// Records what is visible now as an overlay origin.
    pub fn overlay_origin(
        &self,
        focus: Option<iced::advanced::widget::Id>,
    ) -> Option<OverlayOrigin> {
        let (pane, state) = self.visible_pane()?;
        let tab = state.tabs.get(state.focused_tab)?;
        Some(OverlayOrigin {
            pane,
            tab_id: tab.id,
            source: tab.preferences_source()?,
            focus,
        })
    }

    /// Whether `origin` still names the same visible state. Lock, sign-out,
    /// deletion, duress or a replaced Cube change the visible tab or its App
    /// generation, so closing the overlay must not restore into it.
    pub fn overlay_origin_is_current(&self, origin: &OverlayOrigin) -> bool {
        self.visible_pane()
            .filter(|(pane, _)| *pane == origin.pane)
            .and_then(|(_, state)| state.tabs.get(state.focused_tab))
            .is_some_and(|tab| {
                tab.id == origin.tab_id && tab.preferences_source() == Some(origin.source)
            })
    }
}

/// The real retained background, drawn but unreachable while an overlay owns
/// input: it receives no pointer or keyboard event and no focus or other
/// operation, so Tab cannot leave the sheet and background controls cannot
/// activate. Its own overlays (tooltips, menus) are not shown and its
/// animations pause. Nothing is cloned or screenshotted.
struct InertBackground<'a, M> {
    content: Element<'a, M>,
}

impl<'a, M> iced::advanced::Widget<M, coincube_ui::theme::Theme, iced::Renderer>
    for InertBackground<'a, M>
{
    fn size(&self) -> Size<iced::Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        renderer: &iced::Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        tree: &iced::advanced::widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &coincube_ui::theme::Theme,
        style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        _cursor: iced::mouse::Cursor,
        viewport: &iced::Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            iced::mouse::Cursor::Unavailable,
            viewport,
        );
    }

    fn children(&self) -> Vec<iced::advanced::widget::Tree> {
        vec![iced::advanced::widget::Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut iced::advanced::widget::Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }
}

/// Pure overlay composition: `background` is the real view of the retained
/// origin, rendered inert under a scrim; `sheet` owns all input and focus.
pub fn compose_overlay<'a, M: 'a>(
    background: Element<'a, M>,
    sheet: Element<'a, M>,
) -> Element<'a, M> {
    iced::widget::stack![
        Element::new(InertBackground {
            content: background
        }),
        iced::widget::container(
            iced::widget::container(sheet)
                .max_width(1040)
                .height(iced::Length::Fill),
        )
        .width(iced::Length::Fill)
        .height(iced::Length::Fill)
        .center_x(iced::Length::Fill)
        .padding(24)
        .style(|theme: &coincube_ui::theme::Theme| {
            iced::widget::container::Style {
                background: Some(iced::Background::Color(theme.colors.shell.shadow)),
                ..Default::default()
            }
        }),
    ]
    .into()
}

#[cfg(test)]
mod app_preferences_sheet_tests {
    use super::*;
    use crate::app::settings::preferences::{LauncherStyle, MotionPreference, PreferenceChange};

    fn launcher_gui(root: &CoincubeDirectory) -> GUI {
        let (home, startup) = home::Home::new_for_chain(root.clone(), None);
        drop(startup);
        let pane = pane::Pane::new_with_tab(tab::State::Home(home));
        let (panes, focus) = pane_grid::State::new(pane);
        let receipt = GlobalSettings::load_or_initialize_preferences(root);
        GUI {
            panes,
            focus: Some(focus),
            config: Config::new(root.clone(), None),
            window_id: None,
            window_init: None,
            window_config: None,
            global_cache: GlobalCache::default(),
            theme_mode: Default::default(),
            system_theme: None,
            system_reduces_motion: None,
            preferences_sheet: None,
            preferences: PreferencesOwner::new(GlobalSettings::path(root), &receipt),
        }
    }

    fn pin_entry(root: &CoincubeDirectory) -> tab::State {
        let pin = crate::pin_entry::PinEntry::new(
            crate::app::settings::CubeSettings::new(
                "Locked Cube".into(),
                crate::chain::ChainId::Bitcoin,
            ),
            root.path().to_path_buf(),
            crate::pin_entry::PinEntrySuccess::LoadApp {
                datadir: root.clone(),
                config: crate::app::Config::new(false),
                network: bitcoin::Network::Bitcoin,
                internal_bitcoind: None,
                backup: None,
                wallet_settings: None,
                connect_client: None,
            },
            None,
        );
        tab::State::PinEntry(pin)
    }

    #[test]
    fn launcher_sheet_keeps_its_tab_and_routes_a_device_change() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let path = std::env::temp_dir().join(format!(
            "tenshu-app-preferences-sheet-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        let root = CoincubeDirectory::new(path.clone());
        let mut gui = launcher_gui(&root);
        let pane_id = gui.focus.unwrap();
        let tab_id = gui.panes.get(pane_id).unwrap().tabs[0].id;

        drop(gui.update(Message::Pane(
            pane_id,
            pane::Message::tab(
                tab_id,
                tab::Message::OpenAppPreferences(tab::PreferencesSource::Home),
            ),
        )));
        assert!(gui.preferences_sheet.is_some());
        assert!(gui.overlay_origin_is_current(&gui.preferences_sheet.as_ref().unwrap().origin));
        assert_eq!(gui.panes.get(pane_id).unwrap().tabs[0].id, tab_id);

        drop(gui.update(Message::PreferencesSheet(
            preferences_view::Message::Change(PreferenceChange::LauncherStyle(LauncherStyle::List)),
        )));
        assert!(gui.preferences.is_busy());

        drop(gui.update(Message::KeyPressed(Key::Escape)));
        assert!(gui.preferences_sheet.is_none());
        assert_eq!(gui.panes.get(pane_id).unwrap().tabs[0].id, tab_id);
        drop(gui);
        std::fs::remove_dir_all(path).ok();
    }

    #[test]
    fn home_overlay_and_queued_intent_expire_when_unlock_replaces_home() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let path = std::env::temp_dir().join(format!(
            "tenshu-preferences-stale-home-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        let root = CoincubeDirectory::new(path.clone());
        let mut gui = launcher_gui(&root);
        let pane_id = gui.focus.unwrap();
        let tab_id = gui.panes.get(pane_id).unwrap().tabs[0].id;
        let open = || {
            Message::Pane(
                pane_id,
                pane::Message::tab(
                    tab_id,
                    tab::Message::OpenAppPreferences(tab::PreferencesSource::Home),
                ),
            )
        };

        drop(gui.update(open()));
        let origin = gui.preferences_sheet.as_ref().unwrap().origin.clone();
        gui.panes.get_mut(pane_id).unwrap().tabs[0].state = pin_entry(&root);
        assert!(!gui.overlay_origin_is_current(&origin));
        assert_eq!(
            gui.update(Message::PreferencesSheet(
                preferences_view::Message::Action(preferences_view::Action::Close,)
            ))
            .units(),
            0,
            "a stale close must not focus the Launcher behind PIN entry"
        );
        assert!(gui.preferences_sheet.is_none());
        assert_eq!(gui.update(open()).units(), 0);
        assert!(gui.preferences_sheet.is_none());
        drop(gui);
        std::fs::remove_dir_all(path).ok();
    }

    #[test]
    fn ordinary_refresh_and_display_edits_preserve_home_split_flow() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let path = std::env::temp_dir().join(format!(
            "tenshu-preferences-refresh-split-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        let root = CoincubeDirectory::new(path.clone());
        let mut gui = launcher_gui(&root);
        let pane_id = gui.focus.unwrap();
        let tab = &mut gui.panes.get_mut(pane_id).unwrap().tabs[0];
        let tab::State::Home(home) = &mut tab.state else {
            panic!("the launcher starts as Home");
        };
        home.active_section = home::HomeSection::SplitWallet;

        let request = gui.preferences.begin_initialize().unwrap();
        let receipt = GlobalSettings::load_or_initialize_preferences(&root).unwrap();
        drop(gui.update(Message::Preferences(request, Ok(receipt))));
        let tab::State::Home(home) = &gui.panes.get(pane_id).unwrap().tabs[0].state else {
            panic!("refresh replaced Home");
        };
        assert_eq!(home.active_section, home::HomeSection::SplitWallet);

        for change in [
            PreferenceChange::LauncherStyle(LauncherStyle::List),
            PreferenceChange::Motion(MotionPreference::Reduced),
        ] {
            let request = gui.preferences.begin_save(change).unwrap();
            let receipt = GlobalSettings::save_preference(&request.root, change).unwrap();
            drop(gui.update(Message::Preferences(request, Ok(receipt))));
            let tab::State::Home(home) = &gui.panes.get(pane_id).unwrap().tabs[0].state else {
                panic!("display edit replaced Home");
            };
            assert_eq!(home.active_section, home::HomeSection::SplitWallet);
        }
        drop(gui);
        std::fs::remove_dir_all(path).ok();
    }
}

#[cfg(test)]
mod idle_activity_tests {
    use super::*;

    /// The idle auto-lock is only as good as its definition of "activity".
    ///
    /// This previously counted *every non-`Tick` message* as user activity,
    /// which meant it never fired: the 1 Hz tick spawns `UpdateDaemonCache` and
    /// `BitcoindNetStats`, whose results arrive as non-`Tick` messages, so the
    /// timer reset once a second forever. The bug was invisible — the code read
    /// correctly, compiled, and shipped a security control that did nothing.
    #[test]
    fn only_real_input_counts_as_activity() {
        use iced::keyboard;
        use iced::mouse;

        assert!(is_user_input(&Event::Keyboard(
            keyboard::Event::KeyReleased {
                key: keyboard::Key::Named(keyboard::key::Named::Enter),
                modified_key: keyboard::Key::Named(keyboard::key::Named::Enter),
                physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Enter),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
            }
        )));
        assert!(is_user_input(&Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left
        ))));
        assert!(is_user_input(&Event::Mouse(mouse::Event::CursorEntered)));

        // Window events are not a person. A resize or a focus change can come
        // from the OS, another app, or a monitor being plugged in — counting
        // them would let an unattended machine defer the lock indefinitely.
        assert!(!is_user_input(&Event::Window(iced::window::Event::Focused)));
        assert!(!is_user_input(&Event::Window(
            iced::window::Event::CloseRequested
        )));
    }
}

#[cfg(test)]
mod fork_auth_dispatch_tests {
    use super::*;
    use crate::{
        installer,
        services::coincube::{LoginResponse, User},
    };
    use coincube_core::chain::ChainId;
    use iced::futures::StreamExt;

    fn installer_state(root: &CoincubeDirectory) -> tab::State {
        let (mut installer, _) = installer::Installer::new(
            root.clone(),
            bitcoin::Network::Bitcoin,
            None,
            installer::UserFlow::CreateWallet,
            false,
            None,
            None,
            None,
            false,
            None,
        )
        .expect("a Bitcoin fixture installer");
        installer.context.bitcoin_config.chain = ChainId::BitcoinBlake2b;
        tab::State::Installer(Box::new(installer))
    }

    #[test]
    fn new_home_reuses_connect_client_while_sibling_is_being_admitted() {
        let root_path =
            std::env::temp_dir().join(format!("connect-home-reuse-{}", uuid::Uuid::new_v4()));
        let root = CoincubeDirectory::new(root_path.clone());
        let (mut first, first_startup) = home::Home::new_for_chain(root.clone(), None);
        drop(first_startup);
        let mut client = crate::services::coincube::CoincubeClient::new();
        client.set_token("shared-bearer");
        drop(first.adopt_connect_client(client));
        let (second, second_startup) = home::Home::new_for_chain(root.clone(), None);
        drop(second_startup);
        let mut pane = pane::Pane::new_with_tab(tab::State::Home(first));
        pane.tabs.push(tab::Tab::new(2, tab::State::Home(second)));
        let (panes, pane_id) = pane_grid::State::new(pane);
        let mut gui = GUI {
            panes,
            focus: Some(pane_id),
            config: Config::new(root, None),
            window_id: None,
            window_init: None,
            window_config: None,
            global_cache: GlobalCache::default(),
            theme_mode: Default::default(),
            system_theme: None,
            system_reduces_motion: None,
            preferences_sheet: None,
            preferences: unresolved_test_preferences(),
        };
        drop(gui.update(Message::Pane(
            pane_id,
            pane::Message::tab(
                2,
                tab::Message::Launch(home::Message::View(home::ViewMessage::ConnectAccount(
                    crate::app::view::ConnectAccountMessage::Init,
                ))),
            ),
        )));
        let tab::State::Home(second) = &gui.panes.get(pane_id).unwrap().tabs[1].state else {
            panic!("second tab should remain Home");
        };
        assert_eq!(
            second
                .connect_account
                .authenticated_client()
                .and_then(|client| client.token().map(str::to_owned)),
            Some("shared-bearer".into()),
            "opening Home should use its sibling's session instead of refreshing it"
        );
        let _ = std::fs::remove_dir_all(root_path);
    }

    #[test]
    fn accepted_home_login_reaches_an_already_open_fork_pin() {
        let result = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let _enter = runtime.enter();
                let root_path = std::env::temp_dir()
                    .join(format!("pin-session-handoff-{}", uuid::Uuid::new_v4()));
                let root = CoincubeDirectory::new(root_path.clone());
                std::fs::create_dir_all(&root_path).unwrap();
                let (mut home, startup) = home::Home::new_for_chain(root.clone(), None);
                drop(startup);
                let mut client =
                    crate::services::coincube::CoincubeClient::for_test("http://127.0.0.1:1");
                client.set_token("old-session");
                home.connect_account.client = client.clone();
                let pin = crate::pin_entry::PinEntry::new(
                    crate::app::settings::CubeSettings::new(
                        "Pending fork".into(),
                        ChainId::BitcoinBlake2b,
                    ),
                    root_path.clone(),
                    crate::pin_entry::PinEntrySuccess::LoadApp {
                        datadir: root.clone(),
                        config: crate::app::Config::new(false),
                        network: bitcoin::Network::Bitcoin,
                        internal_bitcoind: None,
                        backup: None,
                        wallet_settings: None,
                        connect_client: Some(client),
                    },
                    None,
                );
                let mut pane = pane::Pane::new_with_tab(tab::State::Home(home));
                pane.tabs.push(tab::Tab::new(2, tab::State::PinEntry(pin)));
                let (panes, pane_id) = pane_grid::State::new(pane);
                let mut gui = GUI {
                    panes,
                    focus: Some(pane_id),
                    config: Config::new(root, None),
                    window_id: None,
                    window_init: None,
                    window_config: None,
                    global_cache: GlobalCache::default(),
                    theme_mode: Default::default(),
                    system_theme: None,
                    system_reduces_motion: None,
                    preferences_sheet: None,
                    preferences: unresolved_test_preferences(),
                };
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        1,
                        tab::Message::Launch(home::Message::View(
                            home::ViewMessage::ConnectAccount(
                                crate::app::view::ConnectAccountMessage::SetSession(
                                    LoginResponse {
                                        requires_2fa: false,
                                        token: "new-session".into(),
                                        refresh_token: "new-refresh".into(),
                                        user: User {
                                            id: 42,
                                            email: "fixture@example.invalid".into(),
                                            email_verified: Some(true),
                                        },
                                    },
                                    0,
                                ),
                            ),
                        )),
                    ),
                )));
                let account = |message| {
                    Message::Pane(
                        pane_id,
                        pane::Message::tab(
                            1,
                            tab::Message::Launch(home::Message::View(
                                home::ViewMessage::ConnectAccount(message),
                            )),
                        ),
                    )
                };
                let ready = || {
                    Message::Pane(
                        pane_id,
                        pane::Message::tab(
                            1,
                            tab::Message::Launch(home::Message::ConnectSignedInBubble),
                        ),
                    )
                };
                // A token response is not admission: the account gate has not run.
                drop(gui.update(ready()));
                let tab::State::PinEntry(pin) = &gui.panes.get(pane_id).unwrap().tabs[1].state
                else {
                    unreachable!()
                };
                let crate::pin_entry::PinEntrySuccess::LoadApp { connect_client, .. } =
                    &pin.on_success;
                assert!(
                    connect_client.is_none(),
                    "login token alone grants no PIN candidate"
                );
                drop(gui.update(account(
                    crate::app::view::ConnectAccountMessage::SessionLoaded {
                        user: User {
                            id: 42,
                            email: "fixture@example.invalid".into(),
                            email_verified: Some(true),
                        },
                        plan: None,
                        epoch: 0,
                    },
                )));
                let tab::State::Home(home) = &gui.panes.get(pane_id).unwrap().tabs[0].state else {
                    unreachable!()
                };
                let generation = home.connect_account.session_generation();
                drop(gui.update(ready()));
                let tab::State::PinEntry(pin) = &gui.panes.get(pane_id).unwrap().tabs[1].state
                else {
                    unreachable!()
                };
                let crate::pin_entry::PinEntrySuccess::LoadApp { connect_client, .. } =
                    &pin.on_success;
                assert!(
                    connect_client.is_none(),
                    "checking account grants no PIN candidate"
                );
                for outcome in [
                    crate::services::coincube::DuressCheckOutcome::Unreachable,
                    crate::services::coincube::DuressCheckOutcome::Ok(
                        crate::services::coincube::DuressState {
                            active: true,
                            unlock_at: None,
                            enrolled: false,
                            this_device_registered: false,
                        },
                    ),
                ] {
                    drop(gui.update(account(
                        crate::app::view::ConnectAccountMessage::DuressStateChecked(
                            outcome, generation, 2,
                        ),
                    )));
                    drop(gui.update(ready()));
                    let tab::State::PinEntry(pin) = &gui.panes.get(pane_id).unwrap().tabs[1].state
                    else {
                        unreachable!()
                    };
                    let crate::pin_entry::PinEntrySuccess::LoadApp { connect_client, .. } =
                        &pin.on_success;
                    assert!(
                        connect_client.is_none(),
                        "closed account gate grants no PIN candidate"
                    );
                }
                drop(gui.update(account(
                    crate::app::view::ConnectAccountMessage::DuressStateChecked(
                        crate::services::coincube::DuressCheckOutcome::Ok(
                            crate::services::coincube::DuressState {
                                active: false,
                                unlock_at: None,
                                enrolled: false,
                                this_device_registered: false,
                            },
                        ),
                        generation,
                        0,
                    ),
                )));
                drop(gui.update(ready()));
                let pending = gui
                    .panes
                    .get(pane_id)
                    .unwrap()
                    .tabs
                    .iter()
                    .find(|tab| tab.id == 2)
                    .unwrap();
                let tab::State::PinEntry(pin) = &pending.state else {
                    panic!("unlock must remain at PIN")
                };
                let crate::pin_entry::PinEntrySuccess::LoadApp { connect_client, .. } =
                    &pin.on_success;
                assert_eq!(
                    connect_client.as_ref().and_then(|client| client.token()),
                    Some("new-session"),
                    "a completed Home login must not strand the pending fork PIN signed out"
                );
                assert!(
                    pin.take_fork_signer().is_none(),
                    "session propagation must not unlock the seed"
                );
                let source = gui
                    .panes
                    .get_mut(pane_id)
                    .unwrap()
                    .tabs
                    .iter_mut()
                    .find(|tab| tab.id == 1)
                    .unwrap();
                let tab::State::Home(home) = &mut source.state else {
                    unreachable!()
                };
                home.connect_account.invalidate_auth();
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        1,
                        tab::Message::Launch(home::Message::View(
                            home::ViewMessage::ConnectAccount(
                                crate::app::view::ConnectAccountMessage::SetSession(
                                    LoginResponse {
                                        requires_2fa: false,
                                        token: "stale-session".into(),
                                        refresh_token: "stale-refresh".into(),
                                        user: User {
                                            id: 99,
                                            email: "stale@example.invalid".into(),
                                            email_verified: Some(true),
                                        },
                                    },
                                    0,
                                ),
                            ),
                        )),
                    ),
                )));
                let pending = &gui.panes.get(pane_id).unwrap().tabs[1];
                let tab::State::PinEntry(pin) = &pending.state else {
                    unreachable!()
                };
                let crate::pin_entry::PinEntrySuccess::LoadApp { connect_client, .. } =
                    &pin.on_success;
                assert_eq!(
                    connect_client.as_ref().and_then(|client| client.token()),
                    Some("new-session"),
                    "stale login must not replace the candidate"
                );
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        1,
                        tab::Message::Launch(home::Message::View(
                            home::ViewMessage::ConnectAccount(
                                crate::app::view::ConnectAccountMessage::LogOut,
                            ),
                        )),
                    ),
                )));
                let pending = &gui.panes.get(pane_id).unwrap().tabs[1];
                let tab::State::PinEntry(pin) = &pending.state else {
                    unreachable!()
                };
                let crate::pin_entry::PinEntrySuccess::LoadApp { connect_client, .. } =
                    &pin.on_success;
                assert!(
                    connect_client.is_none(),
                    "logout must withdraw the candidate"
                );
                assert!(pin.take_fork_signer().is_none());
                drop(gui);
                std::fs::remove_dir_all(root_path).unwrap();
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn home_open_only_adopts_completed_scoped_identity() {
        let result = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let _enter = runtime.enter();
                let server = httpmock::MockServer::start();
                let user = server.mock(|when, then| {
                    when.method(httpmock::Method::GET).path("/api/v1/user")
                        .header("authorization", "Bearer direct-session");
                    then.status(200).json_body(serde_json::json!({
                        "id":8,"email":"other@example.invalid","email_verified":true
                    }));
                });
                let feature = server.mock(|when, then| {
                    when.method(httpmock::Method::GET).path("/api/v1/connect/features");
                    then.status(200).json_body(serde_json::json!({"data":{"plans":[],"bitcoin_blake2b_enabled":true}}));
                });
                let provider_url = server.base_url();
                for case in [
                    "direct", "checking", "active", "same",
                    "wrong-root",
                    "wrong-provider",
                    "account-conflict",
                    "token-conflict",
                ] {
                    let path = std::env::temp_dir()
                        .join(format!("cold-home-pin-{}", uuid::Uuid::new_v4()));
                    let root = CoincubeDirectory::new(path.clone());
                    std::fs::create_dir_all(&path).unwrap();
                    crate::app::settings::global::GlobalSettings::update_bitcoin_blake2b_beta(
                        &crate::app::settings::global::GlobalSettings::path(&root),
                        true,
                    )
                    .unwrap();
                    let provider = provider_url.as_str();
                    let ready =
                        |source_root: CoincubeDirectory, provider: &str, id: u32, token: &str| {
                            let (mut home, task) = home::Home::new_for_chain(source_root, None);
                            drop(task);
                            home.connect_account.client =
                                crate::services::coincube::CoincubeClient::for_test(provider);
                            home.connect_account.client.set_token(token);
                            home.connect_account.user = Some(User {
                                id,
                                email: "fixture@example.invalid".into(),
                                email_verified: Some(true),
                            });
                            home.connect_account.step =
                                crate::app::state::connect::account::ConnectFlowStep::Dashboard;
                            home
                        };
                    let source_root = if case == "wrong-root" {
                        CoincubeDirectory::new(path.join("other"))
                    } else {
                        root.clone()
                    };
                    let source_provider = if case == "wrong-provider" {
                        "http://127.0.0.1:2"
                    } else {
                        provider
                    };
                    let first = ready(source_root, source_provider, 7, "current-session");
                    let (mut cold, task) =
                        home::Home::new_for_chain(root.clone(), Some(ChainId::BitcoinBlake2b));
                    drop(task);
                    cold.connect_account.client =
                        crate::services::coincube::CoincubeClient::for_test(provider);
                    drop(cold.update(home::Message::GlobalFeatureLoaded(0, true)));
                    if matches!(case, "direct" | "checking" | "active") {
                        cold.connect_account.client.set_token("direct-session");
                        cold.connect_account.user = Some(User {
                            id:7,email:"fixture@example.invalid".into(),email_verified:Some(true),
                        });
                        cold.connect_account.step = match case {
                            "direct" => crate::app::state::connect::account::ConnectFlowStep::Dashboard,
                            "checking" => crate::app::state::connect::account::ConnectFlowStep::CheckingDuress {
                                status: crate::app::state::connect::account::DuressGateStatus::Checking,
                            },
                            _ => crate::app::state::connect::account::ConnectFlowStep::DuressRecovery {
                                unlock_at:None,passphrase:String::new(),submitting:false,cleared:false,
                            },
                        };
                        assert!(cold.connect_account.authenticated_client().is_some(),
                            "raw client presence is not completed account admission");
                    }

                    let mut pane = pane::Pane::new_with_tab(tab::State::Home(first));
                    pane.tabs.push(tab::Tab::new(2, tab::State::Home(cold)));
                    if case.ends_with("conflict") {
                        pane.tabs.push(tab::Tab::new(
                            3,
                            tab::State::Home(ready(
                                root.clone(),
                                provider,
                                if case == "account-conflict" { 8 } else { 7 },
                                "other-session",
                            )),
                        ));
                    }
                    let (panes, pane_id) = pane_grid::State::new(pane);
                    let mut gui = GUI {
                        panes,
                        focus: Some(pane_id),
                        config: Config::new(root.clone(), None),
                        window_id: None,
                        window_init: None,
                        window_config: None,
                        global_cache: GlobalCache::default(),
                        theme_mode: Default::default(),
            system_theme: None,
            system_reduces_motion: None,
            preferences_sheet: None,
            preferences: unresolved_test_preferences(),
                    };
                    let mut cube = crate::app::settings::CubeSettings::new(
                        "Cold fork".into(),
                        ChainId::BitcoinBlake2b,
                    );
                    cube.created_at = 0;
                    drop(gui.update(Message::Pane(
                        pane_id,
                        pane::Message::tab(
                            2,
                            tab::Message::Launch(home::Message::Run(
                                root,
                                crate::app::Config::new(false),
                                ChainId::BitcoinBlake2b,
                                cube,
                            )),
                        ),
                    )));
                    let pending = &mut gui.panes.get_mut(pane_id).unwrap().tabs[1];
                    let tab::State::PinEntry(pin) = &mut pending.state else {
                        panic!("{} must reach PIN", case)
                    };
                    let crate::pin_entry::PinEntrySuccess::LoadApp { connect_client, .. } =
                        &pin.on_success;
                    assert_eq!(
                        connect_client.as_ref().and_then(|client| client.token()),
                        if case == "direct" {
                            Some("direct-session")
                        } else if case == "same" {
                            Some("current-session")
                        } else {
                            None
                        },
                        "{}",
                        case
                    );
                    assert!(pin.take_fork_signer().is_none());
                    if case == "direct" {
                        // Exercise the actual Home -> PIN candidate. A changed
                        // server identity must be refused before feature/decrypt work.
                        assert_eq!(user.hits(), 0);
                        for (index, digit) in "1234".chars().enumerate() {
                            drop(pin.update(crate::pin_entry::Message::PinInput(
                                crate::pin_input::Message::DigitChanged(index,digit.to_string()))));
                        }
                        let task = pin.update(crate::pin_entry::Message::Submit);
                        let messages = runtime.block_on(async {
                            use iced::futures::StreamExt;
                            let mut messages = Vec::new();
                            if let Some(mut stream) = iced_runtime::task::into_stream(task) {
                                while let Some(action) = stream.next().await {
                                    if let iced_runtime::Action::Output(message) = action {
                                        messages.push(message);
                                    }
                                }
                            }
                            messages
                        });
                        for message in messages { drop(pin.update(message)); }
                        user.assert_hits(1);
                        feature.assert_hits(0);
                        assert!(pin.take_fork_signer().is_none());
                    }
                    drop(gui);
                    std::fs::remove_dir_all(path).unwrap();
                }
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn shared_home_restore_ready_reaches_pending_pin_without_set_session() {
        let result = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let _enter = runtime.enter();
                let path = std::env::temp_dir()
                    .join(format!("restored-home-pin-{}", uuid::Uuid::new_v4()));
                let root = CoincubeDirectory::new(path.clone());
                std::fs::create_dir_all(&path).unwrap();
                let provider = "http://127.0.0.1:1";
                let mut client = crate::services::coincube::CoincubeClient::for_test(provider);
                client.set_token("restored-session");
                let (mut home, startup) = home::Home::new_for_chain(root.clone(), None);
                drop(startup);
                drop(home.adopt_connect_client(client));
                let generation = home.connect_account.session_generation();
                let epoch = home.connect_account.auth_epoch();
                let mut pin = crate::pin_entry::PinEntry::new(
                    crate::app::settings::CubeSettings::new(
                        "Pending fork".into(),
                        ChainId::BitcoinBlake2b,
                    ),
                    path.clone(),
                    crate::pin_entry::PinEntrySuccess::LoadApp {
                        datadir: root.clone(),
                        config: crate::app::Config::new(false),
                        network: bitcoin::Network::Bitcoin,
                        internal_bitcoind: None,
                        backup: None,
                        wallet_settings: None,
                        connect_client: None,
                    },
                    None,
                );
                pin.set_connect_provider(provider);
                let mut pane = pane::Pane::new_with_tab(tab::State::Home(home));
                pane.tabs.push(tab::Tab::new(2, tab::State::PinEntry(pin)));
                let (panes, pane_id) = pane_grid::State::new(pane);
                let mut gui = GUI {
                    panes,
                    focus: Some(pane_id),
                    config: Config::new(root, None),
                    window_id: None,
                    window_init: None,
                    window_config: None,
                    global_cache: GlobalCache::default(),
                    theme_mode: Default::default(),
                    system_theme: None,
                    system_reduces_motion: None,
                    preferences_sheet: None,
                    preferences: unresolved_test_preferences(),
                };
                let account = |message| {
                    Message::Pane(
                        pane_id,
                        pane::Message::tab(
                            1,
                            tab::Message::Launch(home::Message::View(
                                home::ViewMessage::ConnectAccount(message),
                            )),
                        ),
                    )
                };
                // Run the real restore completion: AdmittedUserLoaded dispatches
                // SessionLoaded synchronously, which waits for the duress check.
                // Drop its network tasks; this fixture supplies only their typed outcomes.
                drop(gui.update(account(
                    crate::app::view::ConnectAccountMessage::AdmittedUserLoaded {
                        user: Ok(User {
                            id: 7,
                            email: "fixture@example.invalid".into(),
                            email_verified: Some(true),
                        }),
                        generation,
                        epoch,
                    },
                )));
                let tab::State::Home(home) = &gui.panes.get(pane_id).unwrap().tabs[0].state else {
                    unreachable!()
                };
                assert!(!home.connect_account.is_authenticated());
                let generation = home.connect_account.session_generation();
                let ready = || {
                    Message::Pane(
                        pane_id,
                        pane::Message::tab(
                            1,
                            tab::Message::Launch(home::Message::ConnectSignedInBubble),
                        ),
                    )
                };
                drop(gui.update(ready()));
                let tab::State::PinEntry(pin) = &gui.panes.get(pane_id).unwrap().tabs[1].state
                else {
                    unreachable!()
                };
                let crate::pin_entry::PinEntrySuccess::LoadApp { connect_client, .. } =
                    &pin.on_success;
                assert!(
                    connect_client.is_none(),
                    "checking origin grants no candidate"
                );
                drop(gui.update(account(
                    crate::app::view::ConnectAccountMessage::DuressStateChecked(
                        crate::services::coincube::DuressCheckOutcome::Ok(
                            crate::services::coincube::DuressState {
                                active: false,
                                unlock_at: None,
                                enrolled: false,
                                this_device_registered: false,
                            },
                        ),
                        generation,
                        0,
                    ),
                )));
                let tab::State::Home(home) = &gui.panes.get(pane_id).unwrap().tabs[0].state else {
                    unreachable!()
                };
                assert!(home.connect_account.is_authenticated());
                // The exact-origin bubble emitted on that ready edge repairs the PIN.
                drop(gui.update(ready()));
                let tab::State::PinEntry(pin) = &gui.panes.get(pane_id).unwrap().tabs[1].state
                else {
                    unreachable!()
                };
                let crate::pin_entry::PinEntrySuccess::LoadApp { connect_client, .. } =
                    &pin.on_success;
                assert_eq!(
                    connect_client.as_ref().and_then(|client| client.token()),
                    Some("restored-session")
                );
                assert!(pin.take_fork_signer().is_none());
                drop(gui.update(account(crate::app::view::ConnectAccountMessage::LogOut)));
                drop(gui.update(ready()));
                let tab::State::PinEntry(pin) = &gui.panes.get(pane_id).unwrap().tabs[1].state
                else {
                    unreachable!()
                };
                let crate::pin_entry::PinEntrySuccess::LoadApp { connect_client, .. } =
                    &pin.on_success;
                assert!(
                    connect_client.is_none(),
                    "stale queued readiness cannot revive logout"
                );
                drop(gui);
                std::fs::remove_dir_all(path).unwrap();
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn app_auth_dispatch_runs_home_startup_for_every_invalidated_installer() {
        // Like the completion fixture, keep large inline GUI states off the
        // default libtest stack without changing the runner configuration.
        let result = std::thread::Builder::new()
            .name("fork-auth-dispatch".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(check_auth_dispatch())
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    async fn check_auth_dispatch() {
        use crate::app::view::ConnectAccountMessage;
        for auth in [
            ConnectAccountMessage::LogOut,
            ConnectAccountMessage::SetSession(
                LoginResponse {
                    requires_2fa: false,
                    token: "synthetic".into(),
                    refresh_token: "synthetic".into(),
                    user: User {
                        id: 0,
                        email: "synthetic@example.invalid".into(),
                        email_verified: None,
                    },
                },
                0,
            ),
        ] {
            let root_path =
                std::env::temp_dir().join(format!("fork-auth-dispatch-{}", uuid::Uuid::new_v4()));
            let root = CoincubeDirectory::new(root_path.clone());
            root.network_directory(ChainId::BitcoinBlake2b)
                .init()
                .unwrap();
            // Inject the App message at the GUI seam. A Home source ignores
            // Run, isolating dispatch from account storage and network calls.
            let (home, _) = home::Home::new_for_chain(root.clone(), Some(ChainId::BitcoinBlake2b));
            let mut pane = pane::Pane::new_with_tab(tab::State::Home(home));
            pane.tabs.push(tab::Tab::new(2, installer_state(&root)));
            pane.tabs.push(tab::Tab::new(3, installer_state(&root)));
            let (mut panes, source) = pane_grid::State::new(pane);
            panes
                .split(
                    pane_grid::Axis::Vertical,
                    source,
                    pane::Pane::new_with_tab(installer_state(&root)),
                )
                .unwrap();
            let mut gui = GUI {
                panes,
                focus: Some(source),
                config: Config::new(root.clone(), None),
                window_id: None,
                window_init: None,
                window_config: None,
                global_cache: GlobalCache::default(),
                theme_mode: Default::default(),
                system_theme: None,
                system_reduces_motion: None,
                preferences_sheet: None,
                preferences: unresolved_test_preferences(),
            };
            let task = gui.update(Message::Pane(
                source,
                pane::Message::tab(
                    1,
                    tab::Message::Run(AppMessage::View(crate::app::view::Message::ConnectAccount(
                        auth,
                    ))),
                ),
            ));
            let mut checked = HashSet::new();
            if let Some(mut stream) = iced_runtime::task::into_stream(task) {
                while let Some(action) = stream.next().await {
                    if let iced_runtime::Action::Output(message) = action {
                        if !matches!(&message, Message::Pane(_, pane::Message::Tab(_, inner))
                            if matches!(inner.as_ref(), tab::Message::Launch(home::Message::Checked { .. })))
                        {
                            continue;
                        }
                        if let Message::Pane(pane_id, pane::Message::Tab(tab_id, _)) = &message {
                            checked.insert((*pane_id, *tab_id));
                        }
                        // Apply the real directory-probe completion through the
                        // same GUI seam. Do not execute emitted account Init.
                        let _ = gui.update(message);
                    }
                }
            }
            assert_eq!(
                checked.len(),
                3,
                "every invalidated installer must receive its startup result"
            );
            for (pane_id, tab_id) in checked {
                let tab = gui
                    .panes
                    .get(pane_id)
                    .unwrap()
                    .tabs
                    .iter()
                    .find(|tab| tab.id == tab_id)
                    .unwrap();
                assert!(matches!(&tab.state, tab::State::Home(home) if home.is_checked_for_test()));
            }
            // Ordinary pane dispatch still returns its task through the wrapper.
            let task = gui.update(Message::Pane(
                source,
                pane::Message::View(pane::ViewMessage::OpenConnectSignIn),
            ));
            let mut stream = iced_runtime::task::into_stream(task).expect("pane task retained");
            assert!(matches!(stream.next().await,
                Some(iced_runtime::Action::Output(Message::Pane(id,
                    pane::Message::Tab(1, inner)))) if id == source && matches!(inner.as_ref(), tab::Message::Launch(_))));
            std::fs::remove_dir_all(root_path).unwrap();
        }
    }
}

#[cfg(test)]
mod fork_auth_revocation_tests {
    use super::*;
    use crate::app::{
        self,
        menu::{Menu, VaultSubMenu},
        view::{self, ConnectAccountMessage},
        wallet::Wallet,
    };
    use crate::chain::ChainId;
    use std::sync::Arc;

    async fn outputs<T: Send + 'static>(task: Task<T>) -> Vec<T> {
        use iced::futures::StreamExt;
        match iced_runtime::task::into_stream(task) {
            Some(stream) => {
                stream
                    .filter_map(|action| async move {
                        match action {
                            iced_runtime::Action::Output(msg) => Some(msg),
                            _ => None,
                        }
                    })
                    .collect()
                    .await
            }
            None => Vec::new(),
        }
    }

    #[test]
    fn gui_auth_replacement_drops_open_signer_and_rejects_pending_signatures() {
        // A full GUI + Vault App state in a debug frame exceeds libtest's
        // default thread stack (the same shape as `fork_completion_tests` and
        // `fork_auth_dispatch_tests`), so run the body on a thread with the
        // 8 MiB the real main thread has, without changing the runner.
        let result = std::thread::Builder::new()
            .name("fork-auth-revocation".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(check_auth_replacement())
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[allow(clippy::await_holding_lock)] // serialize synthetic process-global PIN sessions
    async fn check_auth_replacement() {
        let _guard = app::session::test_guard();
        for replace in [false, true] {
            let root_path =
                std::env::temp_dir().join(format!("fork-auth-revoke-{}", uuid::Uuid::new_v4()));
            let root = CoincubeDirectory::new(root_path.clone());
            let chain = ChainId::BitcoinBlake2b;
            let fixture = app::state::vault::test_support::unified::fixture();
            let wallet = Arc::new(
                Wallet::new(fixture.descriptor.clone())
                    .with_chain(chain)
                    .with_signer(crate::signer::Signer::new(
                        app::state::vault::test_support::unified::signer(21),
                    )),
            );
            let weak_wallet = Arc::downgrade(&wallet);
            let weak_signer = Arc::downgrade(wallet.signer.as_ref().unwrap());
            let mut client = crate::services::coincube::CoincubeClient::new();
            client.base_url = "http://127.0.0.1:9".into();
            client.set_token("synthetic-login");
            // Build the daemon config as a TOML value tree rather than a
            // formatted document: the unified fixture descriptor carries
            // hardened-derivation apostrophes (`48'/0'`), which a TOML literal
            // string cannot contain, and a value tree needs no quoting rule.
            let table = |entries: &[(&str, String)]| {
                toml::Value::Table(
                    entries
                        .iter()
                        .map(|(k, v)| (k.to_string(), toml::Value::String(v.clone())))
                        .collect(),
                )
            };
            let cfg: coincubed::config::Config = toml::Value::Table(
                vec![
                    (
                        "main_descriptor".to_string(),
                        toml::Value::String(fixture.descriptor.to_string()),
                    ),
                    (
                        "data_directory".to_string(),
                        toml::Value::String(root_path.display().to_string()),
                    ),
                    (
                        "bitcoin_config".to_string(),
                        table(&[("network", chain.api_str().to_string())]),
                    ),
                    (
                        "esplora_config".to_string(),
                        table(&[(
                            "addr",
                            format!("{}/api/v1/esplora/bitcoin-blake2b/mainnet", client.base_url),
                        )]),
                    ),
                ]
                .into_iter()
                .collect(),
            )
            .try_into()
            .unwrap();
            // GUI-only fixture: no running daemon, HTTP, native keystore or SDK.
            let daemon = Arc::new(crate::daemon::embedded::EmbeddedDaemon::unstarted_for_test(
                cfg, None,
            ));
            let cube = app::settings::CubeSettings::new("fixture".into(), chain);
            let cube_id = cube.id.clone();
            let (mut app, startup) = app::App::new_for_chain(
                app::cache::Cache {
                    fiat_chain: chain,
                    network: bitcoin::Network::Bitcoin,
                    ..Default::default()
                },
                wallet,
                None,
                client,
                app::Config::new(false),
                daemon,
                root.clone(),
                cube,
            )
            .unwrap();
            drop(startup);
            drop(app.update(AppMessage::View(view::Message::Menu(Menu::Vault(
                VaultSubMenu::PSBTs(None),
            )))));
            let tx = crate::daemon::model::SpendTx::new(
                None,
                fixture.psbt,
                Vec::new(),
                &fixture.descriptor,
                &bitcoin::secp256k1::Secp256k1::new(),
                bitcoin::Network::Bitcoin,
            );
            drop(app.update(AppMessage::SpendTxs(Ok(vec![tx]))));
            drop(app.update(AppMessage::View(view::Message::Select(0))));
            drop(app.update(AppMessage::View(view::Message::Spend(
                view::SpendTxMessage::Sign,
            ))));
            let pane = pane::Pane::new_with_tab(tab::State::App(app));
            let (panes, pane_id) = pane_grid::State::new(pane);
            let mut gui = GUI {
                panes,
                focus: Some(pane_id),
                config: Config::new(root, None),
                window_id: None,
                window_init: None,
                window_config: None,
                global_cache: GlobalCache::default(),
                theme_mode: Default::default(),
                system_theme: None,
                system_reduces_motion: None,
                preferences_sheet: None,
                preferences: unresolved_test_preferences(),
            };
            let (home, home_startup) =
                home::Home::new_for_chain(gui.config.coincube_directory.clone(), None);
            drop(home_startup);
            gui.panes
                .get_mut(pane_id)
                .unwrap()
                .tabs
                .push(tab::Tab::new(2, tab::State::Home(home)));
            drop(gui.update(Message::Pane(
                pane_id,
                pane::Message::tab(
                    2,
                    tab::Message::Launch(home::Message::View(home::ViewMessage::ConnectAccount(
                        ConnectAccountMessage::Init,
                    ))),
                ),
            )));
            assert!(matches!(
                gui.panes.get(pane_id).unwrap().tabs[0].state,
                tab::State::App(_)
            ));
            let tab::State::Home(home) = &gui.panes.get(pane_id).unwrap().tabs[1].state else {
                unreachable!()
            };
            assert_eq!(home.connect_account.client.token(), Some("synthetic-login"));
            gui.panes.get_mut(pane_id).unwrap().tabs.pop();
            app::session::open(cube_id.clone(), zeroize::Zeroizing::new("2468".into()));
            let select = || {
                tab::Message::Run(AppMessage::View(view::Message::Spend(
                    view::SpendTxMessage::SelectMasterSigner,
                )))
            };
            let tab = &mut gui.panes.get_mut(pane_id).unwrap().tabs[0];
            let signed = outputs(tab.update(select())).await;
            assert!(signed.iter().any(|msg| matches!(msg,
                tab::Message::ForkAsync(_, inner) if matches!(inner.as_ref(), tab::Message::Run(AppMessage::Signed(_, Ok(_)))))));
            let pending = tab.update(select());
            let auth = if replace {
                ConnectAccountMessage::SetSession(
                    serde_json::from_value(serde_json::json!({
                        "requires_2fa":false,"token":"replacement","refresh_token":"replacement-refresh",
                        "user":{"id":8,"email":"fixture@example.invalid","email_verified":true}
                    }))
                    .unwrap(),
                    0,
                )
            } else {
                ConnectAccountMessage::LogOut
            };
            // Actual GUI auth broadcaster, with the signing picker still open.
            drop(gui.update(Message::Pane(
                pane_id,
                pane::Message::tab(
                    1,
                    tab::Message::Run(AppMessage::View(view::Message::ConnectAccount(auth))),
                ),
            )));
            assert!(outputs(pending).await.is_empty());
            let tab = &mut gui.panes.get_mut(pane_id).unwrap().tabs[0];
            assert!(matches!(tab.state, tab::State::Home(_)));
            if replace {
                let tab::State::Home(home) = &tab.state else {
                    unreachable!()
                };
                assert_eq!(
                    home.connect_account.client.token(),
                    Some("replacement"),
                    "the replacement Home must receive the completed login"
                );
            }
            assert!(tab.wallet().is_none());
            assert!(app::session::pin_for(&cube_id).is_none());
            assert!(weak_wallet.upgrade().is_none());
            assert!(weak_signer.upgrade().is_none());
            assert!(outputs(tab.update(select())).await.is_empty());
            for stale in signed {
                assert!(outputs(tab.update(stale)).await.is_empty());
            }
            if root_path.exists() {
                std::fs::remove_dir_all(root_path).unwrap();
            }
        }
        app::session::close();
    }
}

#[cfg(test)]
mod shared_adoption_generation_tests {
    use super::*;
    use crate::{app, loader};
    use iced::futures::StreamExt;
    use std::sync::Arc;

    async fn first_output(task: Task<Message>) -> Message {
        let mut stream =
            iced_runtime::task::into_stream(task).expect("a real shared-client request");
        while let Some(action) = stream.next().await {
            if let iced_runtime::Action::Output(message) = action {
                return message;
            }
        }
        panic!("shared-client Init produced no account result");
    }

    #[test]
    fn held_shared_client_success_or_auth_refusal_cannot_cross_same_tab_reopen() {
        std::thread::Builder::new()
            .name("shared-app-generation".into())
            .stack_size(16 * 1024 * 1024)
            .spawn(run_held_shared_client_reopen)
            .unwrap()
            .join()
            .unwrap();
    }

    fn run_held_shared_client_reopen() {
        let _guard = app::session::test_guard();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        for old_status in [200, 401] {
            let root_path = std::env::temp_dir().join(format!(
                "shared-app-generation-{old_status}-{}",
                uuid::Uuid::new_v4()
            ));
            let root = CoincubeDirectory::new(root_path.clone());
            let old_server = httpmock::MockServer::start();
            let old_request = old_server.mock(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/user")
                    .header("authorization", "Bearer old-session");
                then.status(old_status).json_body(serde_json::json!({
                    "id":11,"email":"old@example.invalid","email_verified":true
                }));
            });
            let current_server = httpmock::MockServer::start();
            let current_request = current_server.mock(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/user")
                    .header("authorization", "Bearer current-session");
                then.status(200).json_body(serde_json::json!({
                    "id":22,"email":"current@example.invalid","email_verified":true
                }));
            });
            let client = |url: String, token: &str| {
                let mut client = crate::services::coincube::CoincubeClient::for_test(url);
                client.set_token(token);
                client
            };
            let (source, wallet) = app::claim_step1_tests::bitcoin_app(&root_path);
            let cache = source.cache().clone();
            let settings = source.cube_settings().clone();
            let daemon = app::claim_step1_tests::fixture_daemon(&source);
            let (mut home, startup) = home::Home::new_for_chain(root.clone(), None);
            drop(startup);
            drop(home.adopt_connect_client(client(old_server.base_url(), "old-session")));
            let mut pane = pane::Pane::new_with_tab(tab::State::App(source));
            pane.tabs.push(tab::Tab::new(2, tab::State::Home(home)));
            pane.focused_tab = 0;
            let (panes, pane_id) = pane_grid::State::new(pane);
            let mut gui = GUI {
                panes,
                focus: Some(pane_id),
                config: Config::new(root.clone(), None),
                window_id: None,
                window_init: None,
                window_config: None,
                global_cache: GlobalCache::default(),
                theme_mode: Default::default(),
                system_theme: None,
                system_reduces_motion: None,
                preferences_sheet: None,
                preferences: unresolved_test_preferences(),
            };
            let init = || {
                Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        1,
                        tab::Message::Run(AppMessage::View(
                            crate::app::view::Message::ConnectAccount(
                                crate::app::view::ConnectAccountMessage::Init,
                            ),
                        )),
                    ),
                )
            };
            let old_generation = gui.panes.get(pane_id).unwrap().tabs[0]
                .app_generation_token()
                .unwrap();
            // FocusTab's deferred Init is produced by Pane, before the App
            // can start any account work. Hold this actual Pane output across
            // the same-ID Lock/reopen transaction as well.
            let held_focus = runtime.block_on(first_output(gui.update(Message::Pane(
                pane_id,
                pane::Message::View(pane::ViewMessage::FocusTab(0)),
            ))));
            let Message::Pane(_, pane::Message::Tab(1, focus_inner)) = &held_focus else {
                panic!("FocusTab did not return the originating App task");
            };
            let tab::Message::AppAsync(focus_generation, focus_result) = focus_inner.as_ref()
            else {
                panic!("FocusTab task was not generation guarded");
            };
            assert_eq!(*focus_generation, old_generation);
            assert!(matches!(
                focus_result.as_ref(),
                tab::Message::Run(AppMessage::View(crate::app::view::Message::ConnectAccount(
                    crate::app::view::ConnectAccountMessage::Init
                )))
            ));
            let old_result = runtime.block_on(first_output(gui.update(init())));
            old_request.assert();
            let Message::Pane(_, pane::Message::Tab(1, old_inner)) = &old_result else {
                panic!("the actual adoption task must route to the originating tab");
            };
            let tab::Message::AppAsync(generation, result) = old_inner.as_ref() else {
                panic!("the actual adoption task must capture its App generation");
            };
            assert_eq!(*generation, old_generation);
            assert!(
                matches!(result.as_ref(), tab::Message::Run(AppMessage::View(
                crate::app::view::Message::ConnectAccount(
                    crate::app::view::ConnectAccountMessage::AdmittedUserLoaded { user, .. }
                )
            )) if (old_status == 200 && user.as_ref().is_ok_and(|u| u.id == 11))
                || (old_status == 401 && user.as_ref().is_err_and(|(_, auth)| *auth)))
            );

            // The ordinary same-tab lock invalidates the old App. A synthetic
            // successful Loader completion admits a new App without PIN,
            // keyring, node, or payment work in this disposable fixture.
            drop(gui.update(Message::Pane(
                pane_id,
                pane::Message::tab(1, tab::Message::LockCube),
            )));
            assert!(matches!(
                gui.panes.get(pane_id).unwrap().tabs[0].state,
                tab::State::Home(_)
            ));
            let breez = Arc::new(app::breez_liquid::BreezClient::disconnected(
                settings.network.bitcoin_network(),
            ));
            let (loader, startup) = loader::Loader::new(
                root.clone(),
                app::Config::new(false),
                settings.network.bitcoin_network(),
                None,
                None,
                None,
                settings.clone(),
                Some(breez.clone()),
                None,
            );
            drop(startup);
            {
                let pane = gui.panes.get_mut(pane_id).unwrap();
                let tab::State::Home(home) = &mut pane.tabs[1].state else {
                    unreachable!()
                };
                home.connect_account
                    .install_shared_client(client(current_server.base_url(), "current-session"));
                pane.tabs[0].state = tab::State::Loader(loader);
            }
            drop(gui.update(Message::Pane(
                pane_id,
                pane::Message::tab(
                    1,
                    tab::Message::Load(loader::Message::BreezLoaded {
                        breez,
                        spark_backend: None,
                        cache,
                        wallet,
                        config: app::Config::new(false),
                        daemon,
                        datadir: root.clone(),
                        bitcoind: None,
                        cube_settings: settings,
                    }),
                ),
            )));
            let current_generation = gui.panes.get(pane_id).unwrap().tabs[0]
                .app_generation_token()
                .unwrap();
            assert_ne!(old_generation, current_generation);
            let current_result = runtime.block_on(first_output(gui.update(init())));
            current_request.assert();
            assert!(matches!(&current_result,
                Message::Pane(_, pane::Message::Tab(1, inner))
                    if matches!(inner.as_ref(), tab::Message::AppAsync(generation, _)
                        if *generation == current_generation)));
            drop(gui.update(current_result));
            assert_eq!(gui.reviewer_app_mut(0).connect_user_id_for_test(), Some(22));
            let menu = gui.reviewer_app_mut(0).current_menu_for_test().clone();
            let had_claim_context = gui.reviewer_app_mut(0).has_claim_pair_context();
            assert_eq!(gui.update(old_result).units(), 0);
            let app = gui.reviewer_app_mut(0);
            assert_eq!(app.connect_user_id_for_test(), Some(22));
            assert_eq!(app.current_menu_for_test(), &menu);
            assert_eq!(app.has_claim_pair_context(), had_claim_context);
            assert_eq!(gui.update(held_focus).units(), 0);
            assert_eq!(gui.reviewer_app_mut(0).connect_user_id_for_test(), Some(22));
            drop(gui);
            std::fs::remove_dir_all(root_path).ok();
        }
    }
}

#[cfg(test)]
mod shell_escape_dispatch_tests {
    use super::*;
    use crate::app::{self, view};

    fn escape_event(repeat: bool) -> Event {
        let key = keyboard::Key::Named(keyboard::key::Named::Escape);
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Escape),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat,
        })
    }

    #[test]
    fn only_ignored_nonrepeating_escape_reaches_the_shell_listener() {
        assert_eq!(
            ignored_shell_key(&escape_event(false), event::Status::Ignored),
            Some(Key::Escape)
        );
        assert_eq!(
            ignored_shell_key(&escape_event(false), event::Status::Captured),
            None
        );
        assert_eq!(
            ignored_shell_key(&escape_event(true), event::Status::Ignored),
            None
        );
    }

    #[test]
    fn real_gui_escape_dismisses_only_the_focused_app_shell() {
        std::thread::Builder::new()
            .name("gui-shell-escape".into())
            .stack_size(16 * 1024 * 1024)
            .spawn(run_gui_shell_escape)
            .unwrap()
            .join()
            .unwrap();
    }

    fn run_gui_shell_escape() {
        let _guard = app::session::test_guard();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let path = std::env::temp_dir().join(format!("cube-escape-{}", uuid::Uuid::new_v4()));
        let root = CoincubeDirectory::new(path.clone());
        let (source, _) = app::claim_step1_tests::bitcoin_app(&path);
        let cube_id = source.cube_settings().id.clone();
        let owner = source.cache().app_generation;
        app::session::open_without_pin(cube_id.clone());
        let (mut home, startup) = home::Home::new_for_chain(root.clone(), None);
        drop(startup);
        home.set_claim_owner(owner);
        let mut pane = pane::Pane::new_with_tab(tab::State::App(source));
        pane.tabs.push(tab::Tab::new(2, tab::State::Home(home)));
        let handoff =
            app::claim_intent::ForkHandoff::new(&root, cube_id.clone(), "paired-fixture".into())
                .unwrap();
        pane.tabs[1].set_claim_open(home::ClaimOpenIntent::new(
            owner,
            &root,
            cube_id.clone(),
            "paired-fixture".into(),
            home::ClaimRoute::Begin(handoff),
        ));
        pane.focused_tab = 0;
        let (panes, pane_id) = pane_grid::State::new(pane);
        let mut gui = GUI {
            panes,
            focus: Some(pane_id),
            config: Config::new(root, None),
            window_id: None,
            window_init: None,
            window_config: None,
            global_cache: GlobalCache::default(),
            theme_mode: Default::default(),
            system_theme: None,
            system_reduces_motion: None,
            preferences_sheet: None,
            preferences: unresolved_test_preferences(),
        };
        let route = |intent| {
            Message::Pane(
                pane_id,
                pane::Message::tab(1, tab::Message::Run(AppMessage::View(intent))),
            )
        };
        for intent in [
            view::Message::ToggleVaultTools,
            view::Message::ToggleCubeSwitcher,
            view::Message::AskCubeLock,
        ] {
            drop(gui.update(route(intent)));
        }
        let menu = gui.reviewer_app_mut(0).current_menu_for_test().clone();
        let shell = |gui: &mut GUI| {
            let cache = gui.reviewer_app_mut(0).cache();
            (
                cache.cube_lock_confirmation,
                cache.cube_switcher_open,
                cache.vault_tools_open,
            )
        };
        assert_eq!(shell(&mut gui), (true, true, true));
        for expected in [
            (false, true, true),
            (false, false, true),
            (false, false, false),
        ] {
            let focus = gui.update(Message::KeyPressed(Key::Escape));
            assert!(
                focus.units() > 0,
                "dismissal returns its trigger focus task"
            );
            drop(focus);
            assert_eq!(shell(&mut gui), expected);
            assert_eq!(gui.reviewer_app_mut(0).current_menu_for_test(), &menu);
            assert_eq!(
                app::session::current_cube_id().as_deref(),
                Some(cube_id.as_str())
            );
            assert_eq!(
                gui.panes.get(pane_id).unwrap().tabs[1].claim_open_owner(),
                Some(owner)
            );
        }
        assert_eq!(gui.update(Message::KeyPressed(Key::Escape)).units(), 0);
        {
            let pane = gui.panes.get_mut(pane_id).unwrap();
            let tab::State::App(app) = &mut pane.tabs[0].state else {
                unreachable!()
            };
            app.cache_mut().vault_tools_open = true;
            pane.focused_tab = 1;
        }
        assert_eq!(gui.update(Message::KeyPressed(Key::Escape)).units(), 0);
        assert!(gui.reviewer_app_mut(0).cache().vault_tools_open);
        assert_eq!(
            app::session::current_cube_id().as_deref(),
            Some(cube_id.as_str())
        );
        app::session::close();
        drop(gui);
        std::fs::remove_dir_all(path).ok();
    }
}

#[cfg(test)]
mod lock_cube_dispatch_tests {
    use super::*;
    use crate::app;

    #[test]
    fn locking_source_app_closes_owned_home_claim_but_keeps_unrelated_home() {
        let _guard = crate::app::session::test_guard();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let path =
            std::env::temp_dir().join(format!("cube-lock-home-claim-{}", uuid::Uuid::new_v4()));
        let root = CoincubeDirectory::new(path.clone());
        let (source, _) = app::claim_step1_tests::bitcoin_app(&path);
        let owner = source.cache().app_generation;
        let source_id = source.cube_settings().id.clone();
        let (unrelated, startup) = home::Home::new_for_chain(root.clone(), None);
        drop(startup);
        let (mut claim, startup) = home::Home::new_for_chain(root, None);
        drop(startup);
        claim.set_claim_owner(owner);
        let mut gui = GUI::reviewer_app_home_gui(source, unrelated);
        let pane_id = gui.focus.unwrap();
        gui.panes
            .get_mut(pane_id)
            .unwrap()
            .tabs
            .push(tab::Tab::new(3, tab::State::Home(claim)));
        app::session::open(source_id, zeroize::Zeroizing::new("2468".into()));

        drop(gui.update(Message::Pane(
            pane_id,
            pane::Message::tab(1, tab::Message::LockCubeToLauncher),
        )));
        let pane = gui.panes.get(pane_id).unwrap();
        assert!(pane.tabs.iter().any(|tab| tab.id == 2));
        assert!(
            pane.tabs.iter().all(|tab| tab.id != 3),
            "owned Claim Home closed"
        );
        assert!(pane
            .tabs
            .iter()
            .all(|tab| tab.claim_open_owner() != Some(owner)));
        drop(gui);
        std::fs::remove_dir_all(path).ok();
    }

    #[test]
    fn admitted_destination_closes_orphaned_process_session() {
        let _guard = crate::app::session::test_guard();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let path =
            std::env::temp_dir().join(format!("cube-orphaned-session-{}", uuid::Uuid::new_v4()));
        let root = CoincubeDirectory::new(path.clone());
        let (home, startup) = home::Home::new_for_chain(root.clone(), None);
        drop(startup);
        let pane = pane::Pane::new_with_tab(tab::State::Home(home));
        let (panes, pane_id) = pane_grid::State::new(pane);
        let mut gui = GUI {
            panes,
            focus: Some(pane_id),
            config: Config::new(root.clone(), None),
            window_id: None,
            window_init: None,
            window_config: None,
            global_cache: GlobalCache::default(),
            theme_mode: Default::default(),
            system_theme: None,
            system_reduces_motion: None,
            preferences_sheet: None,
            preferences: unresolved_test_preferences(),
        };
        crate::app::session::open("orphaned", zeroize::Zeroizing::new("2468".into()));
        let destination = crate::app::settings::CubeSettings::new_with_raw_id(
            "next-cube".into(),
            "Next Cube".into(),
            crate::chain::ChainId::Bitcoin,
        );
        drop(gui.update(Message::Pane(
            pane_id,
            pane::Message::tab(
                1,
                tab::Message::Launch(home::Message::Run(
                    root,
                    crate::app::Config::new(false),
                    crate::chain::ChainId::Bitcoin,
                    destination,
                )),
            ),
        )));
        assert!(!crate::app::session::is_open());
        drop(gui);
        std::fs::remove_dir_all(path).ok();
    }

    #[test]
    fn refused_public_destination_keeps_the_current_app_and_session() {
        let _guard = crate::app::session::test_guard();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let path =
            std::env::temp_dir().join(format!("cube-refused-switch-{}", uuid::Uuid::new_v4()));
        let root = CoincubeDirectory::new(path.clone());
        let (source, _) = crate::app::claim_step1_tests::bitcoin_app(&path);
        let source_id = source.cube_settings().id.clone();
        let (home, startup) = home::Home::new_for_chain(root.clone(), None);
        drop(startup);
        let mut gui = GUI::reviewer_app_home_gui(source, home);
        let pane_id = gui.focus.unwrap();
        crate::app::session::open(source_id.clone(), zeroize::Zeroizing::new("2468".into()));

        let mismatched = crate::app::settings::CubeSettings::new_with_raw_id(
            "wrong-chain".into(),
            "Wrong chain".into(),
            crate::chain::ChainId::BitcoinBlake2b,
        );
        drop(gui.update(Message::Pane(
            pane_id,
            pane::Message::tab(
                2,
                tab::Message::Launch(home::Message::Run(
                    root.clone(),
                    crate::app::Config::new(false),
                    crate::chain::ChainId::Bitcoin,
                    mismatched,
                )),
            ),
        )));
        assert!(matches!(
            &gui.panes.get(pane_id).unwrap().tabs[0].state,
            tab::State::App(app) if app.cube_settings().id == source_id
        ));
        assert_eq!(
            crate::app::session::current_cube_id(),
            Some(source_id.clone())
        );
        let tab::State::Home(home) = &gui.panes.get(pane_id).unwrap().tabs[1].state else {
            panic!("the Launcher must report the refusal");
        };
        assert!(home.error().is_some_and(|error| error.contains("folder")));
        // A delayed floor result carries its original generation even after
        // Home has cleared the floor selection. Admission must reject it
        // before the public App and its process session are retired.
        let stale_floor_cube = crate::app::settings::CubeSettings::new_with_raw_id(
            "stale-floor".into(),
            "Stale floor".into(),
            crate::chain::ChainId::Bitcoin,
        );
        drop(gui.update(Message::Pane(
            pane_id,
            pane::Message::tab(
                2,
                tab::Message::Launch(home::Message::RunFromFloor(
                    root,
                    crate::app::Config::new(false),
                    crate::chain::ChainId::Bitcoin,
                    stale_floor_cube,
                    1,
                )),
            ),
        )));
        assert!(matches!(
            &gui.panes.get(pane_id).unwrap().tabs[0].state,
            tab::State::App(app) if app.cube_settings().id == source_id
        ));
        assert_eq!(crate::app::session::current_cube_id(), Some(source_id));
        let tab::State::Home(home) = &gui.panes.get(pane_id).unwrap().tabs[1].state else {
            panic!("a stale floor result must remain in the Launcher");
        };
        assert!(home
            .error()
            .is_some_and(|error| error.contains("changed while opening")));
        drop(gui);
        crate::app::session::close();
        std::fs::remove_dir_all(path).ok();
    }

    #[test]
    fn ordinary_switch_survives_recursive_removal_of_an_earlier_claim_loader() {
        let _guard = crate::app::session::test_guard();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let path =
            std::env::temp_dir().join(format!("cube-switch-cascade-{}", uuid::Uuid::new_v4()));
        let root = CoincubeDirectory::new(path.clone());
        let (source, _) = crate::app::claim_step1_tests::bitcoin_app(&path);
        let source_settings = source.cube_settings().clone();
        let owner = source.cache().app_generation;
        let (loader, startup) = crate::loader::Loader::new(
            root.clone(),
            crate::app::Config::new(false),
            source_settings.network.bitcoin_network(),
            None,
            None,
            None,
            source_settings.clone(),
            None,
            None,
        );
        drop(startup);
        let handoff = crate::app::claim_intent::ForkHandoff::new(
            &root,
            source_settings.id.clone(),
            "paired".into(),
        )
        .unwrap();
        let (home, home_startup) = home::Home::new_for_chain(root.clone(), None);
        drop(home_startup);
        let mut pane = pane::Pane::new_with_tab(tab::State::Loader(loader));
        pane.tabs[0].set_claim_open(home::ClaimOpenIntent::new(
            owner,
            &root,
            source_settings.id.clone(),
            "paired".into(),
            home::ClaimRoute::Begin(handoff),
        ));
        pane.tabs.push(tab::Tab::new(2, tab::State::Home(home)));
        pane.tabs.push(tab::Tab::new(3, tab::State::App(source)));
        pane.focused_tab = 1;
        let (panes, pane_id) = pane_grid::State::new(pane);
        let mut gui = GUI {
            panes,
            focus: Some(pane_id),
            config: Config::new(root.clone(), None),
            window_id: None,
            window_init: None,
            window_config: None,
            global_cache: GlobalCache::default(),
            theme_mode: Default::default(),
            system_theme: None,
            system_reduces_motion: None,
            preferences_sheet: None,
            preferences: unresolved_test_preferences(),
        };
        let destination = crate::app::settings::CubeSettings::new_with_raw_id(
            "other".into(),
            "Other".into(),
            source_settings.network,
        );
        drop(gui.update(Message::Pane(
            pane_id,
            pane::Message::tab(
                2,
                tab::Message::Launch(home::Message::Run(
                    root.clone(),
                    crate::app::Config::new(false),
                    source_settings.network,
                    destination,
                )),
            ),
        )));
        let pane = gui.panes.get(pane_id).expect("Launcher survives switch");
        assert_eq!(pane.tabs.len(), 1, "owned Loader and App were both closed");
        assert_eq!(pane.tabs[0].id, 2, "unrelated Launcher retains identity");
        assert!(!matches!(pane.tabs[0].state, tab::State::App(_)));
        drop(gui);
        std::fs::remove_dir_all(path).ok();
    }

    #[test]
    fn lock_retires_every_secret_owner_before_clearing_the_shared_session() {
        let _guard = crate::app::session::test_guard();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let root_path =
            std::env::temp_dir().join(format!("cube-lock-siblings-{}", uuid::Uuid::new_v4()));
        let root = CoincubeDirectory::new(root_path.clone());
        let (first, _) = crate::app::claim_step1_tests::bitcoin_app(&root_path);
        let same_cube_id = first.cube_settings().id.clone();
        let same_settings = first.cube_settings().clone();
        let sibling = crate::app::claim_step1_tests::bitcoin_app_with_id(&root_path, &same_cube_id);
        let same_pane_sibling =
            crate::app::claim_step1_tests::bitcoin_app_with_id(&root_path, &same_cube_id);
        let (unrelated, _) = crate::app::claim_step1_tests::bitcoin_app(&root_path);
        let mut first_pane = pane::Pane::new_with_tab(tab::State::App(first));
        first_pane
            .tabs
            .push(tab::Tab::new(2, tab::State::App(unrelated)));
        first_pane
            .tabs
            .push(tab::Tab::new(3, tab::State::App(same_pane_sibling)));
        let (loading, loading_task) = crate::loader::Loader::new(
            root.clone(),
            crate::app::Config::new(false),
            same_settings.network.bitcoin_network(),
            None,
            None,
            None,
            same_settings.clone(),
            None,
            None,
        );
        drop(loading_task);
        first_pane
            .tabs
            .push(tab::Tab::new(4, tab::State::Loader(loading)));
        let (installing, install_task) = crate::installer::Installer::try_new_for_chain(
            root.clone(),
            same_settings.network,
            None,
            crate::installer::UserFlow::CreateWallet,
            true,
            Some(same_settings),
            None,
            None,
            false,
            None,
        )
        .unwrap();
        drop(install_task);
        first_pane.tabs.push(tab::Tab::new(
            5,
            tab::State::Installer(Box::new(installing)),
        ));
        let (mut panes, origin) = pane_grid::State::new(first_pane);
        let (other, _) = panes
            .split(
                pane_grid::Axis::Vertical,
                origin,
                pane::Pane::new_with_tab(tab::State::App(sibling)),
            )
            .unwrap();
        let mut gui = GUI {
            panes,
            focus: Some(origin),
            config: Config::new(root, None),
            window_id: None,
            window_init: None,
            window_config: None,
            global_cache: GlobalCache::default(),
            theme_mode: Default::default(),
            system_theme: None,
            system_reduces_motion: None,
            preferences_sheet: None,
            preferences: unresolved_test_preferences(),
        };
        crate::app::session::open(same_cube_id.clone(), zeroize::Zeroizing::new("2468".into()));
        assert_eq!(crate::app::session::current_cube_id(), Some(same_cube_id));

        drop(gui.update(Message::Pane(
            origin,
            pane::Message::tab(1, tab::Message::LockCubeToLauncher),
        )));
        let pane = gui.panes.get(origin).expect("origin remains on Launcher");
        assert!(matches!(pane.tabs[0].state, tab::State::Home(_)));
        assert_eq!(
            pane.tabs.len(),
            1,
            "no App, Loader or Installer may retain a cleared global session"
        );
        assert!(gui.panes.get(other).is_none(), "empty sibling pane closed");
        assert_eq!(gui.focus, Some(origin));
        assert!(!crate::app::session::is_open());
        drop(gui.update(Message::Pane(
            origin,
            pane::Message::tab(4, tab::Message::Load(crate::loader::Message::None)),
        )));
        assert_eq!(gui.panes.get(origin).unwrap().tabs.len(), 1);

        drop(gui);
        let _ = std::fs::remove_dir_all(root_path);
    }

    #[test]
    fn a_removed_claim_loader_stops_detached_node_before_missing_tab_rejection() {
        let _guard = crate::app::session::test_guard();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let path =
            std::env::temp_dir().join(format!("closed-claim-loader-{}", uuid::Uuid::new_v4()));
        let root = CoincubeDirectory::new(path.clone());
        let (source, _) = crate::app::claim_step1_tests::bitcoin_app(&path);
        let source_cube = source.cube_settings().id.clone();
        let source_generation = source.cache().app_generation;
        let (target, wallet) = crate::app::claim_step1_tests::bitcoin_app(&path.join("target"));
        let target_daemon = crate::app::claim_step1_tests::fixture_daemon(&target);
        let target_cache = target.cache().clone();
        let target_settings = target.cube_settings().clone();
        let node = crate::node::bitcoind::Bitcoind::for_test(
            coincubed::config::BitcoindConfig {
                rpc_auth: coincubed::config::BitcoindRpcAuth::UserPass(
                    "fixture".into(),
                    "fixture".into(),
                ),
                addr: "127.0.0.1:1".parse().unwrap(),
            },
            &root,
            bitcoin::Network::Bitcoin,
        );
        let lock_dir = root.bitcoind_directory().path().join("locks/bitcoin");
        assert!(lock_dir.exists());
        let handoff =
            crate::app::claim_intent::ForkHandoff::new(&root, source_cube.clone(), "paired".into())
                .unwrap();
        let intent = home::ClaimOpenIntent::new(
            source_generation,
            &root,
            source_cube.clone(),
            "paired".into(),
            home::ClaimRoute::Begin(handoff),
        );
        let pane = pane::Pane::new_with_tab(tab::State::App(source));
        let (panes, pane_id) = pane_grid::State::new(pane);
        let mut gui = GUI {
            panes,
            focus: Some(pane_id),
            config: Config::new(root.clone(), None),
            window_id: None,
            window_init: None,
            window_config: None,
            global_cache: GlobalCache::default(),
            theme_mode: Default::default(),
            system_theme: None,
            system_reduces_motion: None,
            preferences_sheet: None,
            preferences: unresolved_test_preferences(),
        };
        let resources = tab::ClaimResources {
            intent,
            wallet,
            cache: target_cache,
            daemon: target_daemon,
            bitcoind: Some(node),
            settings: target_settings,
            client: None,
            verified_account: None,
        };
        drop(gui.update(Message::Pane(
            pane_id,
            pane::Message::tab(999, tab::Message::ClaimResourcesLoaded(Box::new(resources))),
        )));
        assert!(crate::node::bitcoind::wait_for_managed_node_cleanup(
            std::time::Duration::from_secs(120)
        ));
        assert!(!lock_dir.exists(), "the detached node lock was released");
        let surviving = &gui.panes.get(pane_id).unwrap().tabs[0];
        assert!(matches!(&surviving.state, tab::State::App(app)
            if app.cube_settings().id == source_cube && !app.has_claim_pair_context()));

        // A late Synced payload uses the same rejection path, but has not
        // detached into a Claim context yet.
        let node = crate::node::bitcoind::Bitcoind::for_test(
            coincubed::config::BitcoindConfig {
                rpc_auth: coincubed::config::BitcoindRpcAuth::UserPass(
                    "fixture".into(),
                    "fixture".into(),
                ),
                addr: "127.0.0.1:1".parse().unwrap(),
            },
            &root,
            bitcoin::Network::Bitcoin,
        );
        assert!(lock_dir.exists());
        let late_wallet = std::sync::Arc::new(surviving.wallet().unwrap().clone());
        let late_cache = surviving.cache().unwrap().clone();
        drop(gui.update(Message::Pane(
            pane_id,
            pane::Message::tab(
                999,
                tab::Message::Load(crate::loader::Message::Synced(Ok((
                    late_wallet,
                    late_cache,
                    crate::app::claim_step1_tests::fixture_daemon(&target),
                    Some(node),
                    None,
                    target.cube_settings().clone(),
                )))),
            ),
        )));
        assert!(crate::node::bitcoind::wait_for_managed_node_cleanup(
            std::time::Duration::from_secs(120)
        ));
        assert!(!lock_dir.exists());

        let (closed_home, startup) = home::Home::new_for_chain(root.clone(), None);
        drop(startup);
        let (closed_pane, _) = gui
            .panes
            .split(
                pane_grid::Axis::Vertical,
                pane_id,
                pane::Pane::new_with_tab(tab::State::Home(closed_home)),
            )
            .unwrap();
        gui.panes.close(closed_pane);
        let node = crate::node::bitcoind::Bitcoind::for_test(
            coincubed::config::BitcoindConfig {
                rpc_auth: coincubed::config::BitcoindRpcAuth::UserPass(
                    "fixture".into(),
                    "fixture".into(),
                ),
                addr: "127.0.0.1:1".parse().unwrap(),
            },
            &root,
            bitcoin::Network::Bitcoin,
        );
        assert!(lock_dir.exists());
        let handoff =
            crate::app::claim_intent::ForkHandoff::new(&root, source_cube.clone(), "paired".into())
                .unwrap();
        let stale = tab::ClaimResources {
            intent: home::ClaimOpenIntent::new(
                source_generation,
                &root,
                source_cube.clone(),
                "paired".into(),
                home::ClaimRoute::Begin(handoff),
            ),
            wallet: std::sync::Arc::new(target.wallet().unwrap().clone()),
            cache: target.cache().clone(),
            daemon: crate::app::claim_step1_tests::fixture_daemon(&target),
            bitcoind: Some(node),
            settings: target.cube_settings().clone(),
            client: None,
            verified_account: None,
        };
        drop(gui.update(Message::Pane(
            closed_pane,
            pane::Message::tab(1, tab::Message::ClaimResourcesLoaded(Box::new(stale))),
        )));
        assert!(crate::node::bitcoind::wait_for_managed_node_cleanup(
            std::time::Duration::from_secs(120)
        ));
        assert!(
            !lock_dir.exists(),
            "missing-pane delivery released its node"
        );
        let node = crate::node::bitcoind::Bitcoind::for_test(
            coincubed::config::BitcoindConfig {
                rpc_auth: coincubed::config::BitcoindRpcAuth::UserPass(
                    "fixture".into(),
                    "fixture".into(),
                ),
                addr: "127.0.0.1:1".parse().unwrap(),
            },
            &root,
            bitcoin::Network::Bitcoin,
        );
        let handoff =
            crate::app::claim_intent::ForkHandoff::new(&root, source_cube.clone(), "paired".into())
                .unwrap();
        let stale = tab::ClaimResources {
            intent: home::ClaimOpenIntent::new(
                source_generation,
                &root,
                source_cube.clone(),
                "paired".into(),
                home::ClaimRoute::Begin(handoff),
            ),
            wallet: std::sync::Arc::new(target.wallet().unwrap().clone()),
            cache: target.cache().clone(),
            daemon: crate::app::claim_step1_tests::fixture_daemon(&target),
            bitcoind: Some(node),
            settings: target.cube_settings().clone(),
            client: None,
            verified_account: None,
        };
        drop(gui.update(Message::Pane(
            pane_id,
            pane::Message::tab(
                1,
                tab::Message::AppAsync(
                    u64::MAX,
                    Box::new(tab::Message::ClaimResourcesLoaded(Box::new(stale))),
                ),
            ),
        )));
        assert!(crate::node::bitcoind::wait_for_managed_node_cleanup(
            std::time::Duration::from_secs(120)
        ));
        assert!(
            !lock_dir.exists(),
            "stale-generation delivery released its node"
        );
        drop(gui);
        drop(target);
        std::fs::remove_dir_all(path).ok();
    }
}

// Gandalf's round-3 reviewer seam (#518 issuecomment-5824706295), adopted:
// a real GUI with two Bitcoin App tabs, its real auth broadcaster and
// originating-tab dispatch. Test-only.
#[cfg(test)]
impl GUI {
    pub(crate) fn reviewer_load_preferences(&mut self) {
        let root = &self.config.coincube_directory;
        let receipt = GlobalSettings::load_or_initialize_preferences(root).unwrap();
        self.preferences = PreferencesOwner::new(GlobalSettings::path(root), &Ok(receipt));
        self.publish_balance_privacy();
        self.publish_appearance();
    }

    pub(crate) fn reviewer_claim_gui(first: crate::app::App, second: crate::app::App) -> Self {
        let root = first.datadir().clone();
        let mut pane = pane::Pane::new_with_tab(tab::State::App(first));
        pane.tabs.push(tab::Tab::new(2, tab::State::App(second)));
        let (panes, pane_id) = pane_grid::State::new(pane);
        Self {
            panes,
            focus: Some(pane_id),
            config: Config::new(root, None),
            window_id: None,
            window_init: None,
            window_config: None,
            global_cache: GlobalCache::default(),
            theme_mode: Default::default(),
            system_theme: None,
            system_reduces_motion: None,
            preferences_sheet: None,
            preferences: unresolved_test_preferences(),
        }
    }
    /// An already-open Bitcoin Cube (tab 1) beside a Home tab (tab 2), in one pane.
    pub(crate) fn reviewer_app_home_gui(app: crate::app::App, home: home::Home) -> Self {
        let root = app.datadir().clone();
        let mut pane = pane::Pane::new_with_tab(tab::State::App(app));
        pane.tabs.push(tab::Tab::new(2, tab::State::Home(home)));
        let (panes, pane_id) = pane_grid::State::new(pane);
        Self {
            panes,
            focus: Some(pane_id),
            config: Config::new(root, None),
            window_id: None,
            window_init: None,
            window_config: None,
            global_cache: GlobalCache::default(),
            theme_mode: Default::default(),
            system_theme: None,
            system_reduces_motion: None,
            preferences_sheet: None,
            preferences: unresolved_test_preferences(),
        }
    }
    /// As [`Self::reviewer_app_home_gui`], with the Cube moved to its own
    /// split pane. Returns the Cube's pane; Home keeps the focused pane.
    pub(crate) fn reviewer_split_app_home_gui(
        app: crate::app::App,
        home: home::Home,
    ) -> (Self, pane_grid::Pane) {
        let mut gui = Self::reviewer_app_home_gui(app, home);
        let home_pane = gui.focus.unwrap();
        let cube_tab = gui.panes.get_mut(home_pane).unwrap().tabs.remove(0);
        let mut cube_pane = pane::Pane::new_with_tab(tab::State::Home(
            home::Home::new_for_chain(gui.config.coincube_directory.clone(), None).0,
        ));
        cube_pane.tabs[0] = cube_tab;
        let (cube_pane_id, _) = gui
            .panes
            .split(pane_grid::Axis::Vertical, home_pane, cube_pane)
            .unwrap();
        (gui, cube_pane_id)
    }
    pub(crate) fn reviewer_app_in(&mut self, pane: pane_grid::Pane) -> &mut crate::app::App {
        let tab::State::App(app) = &mut self.panes.get_mut(pane).unwrap().tabs[0].state else {
            panic!("Bitcoin App closed")
        };
        app
    }
    pub(crate) fn reviewer_account_in(
        &mut self,
        pane: pane_grid::Pane,
        id: usize,
        account: crate::app::view::ConnectAccountMessage,
    ) -> Task<Message> {
        self.update(Message::Pane(
            pane,
            pane::Message::tab(
                id,
                tab::Message::Run(AppMessage::View(crate::app::view::Message::ConnectAccount(
                    account,
                ))),
            ),
        ))
    }
    pub(crate) fn reviewer_home_mut(&mut self, index: usize) -> &mut home::Home {
        let pane = self.panes.get_mut(self.focus.unwrap()).unwrap();
        let tab::State::Home(home) = &mut pane.tabs[index].state else {
            panic!("Home closed")
        };
        home
    }
    pub(crate) fn reviewer_home_account(
        &mut self,
        id: usize,
        account: crate::app::view::ConnectAccountMessage,
    ) -> Task<Message> {
        self.update(Message::Pane(
            self.focus.unwrap(),
            pane::Message::tab(
                id,
                tab::Message::Launch(home::Message::View(home::ViewMessage::ConnectAccount(
                    account,
                ))),
            ),
        ))
    }
    /// Account traffic of any tab plus the Home sign-in bubble and its pane
    /// rebroadcast: the messages that carry a sign-in to other open Cubes.
    pub(crate) fn reviewer_is_session_propagation(message: &Message) -> bool {
        match message {
            Message::Pane(_, pane::Message::View(pane::ViewMessage::ConnectSignedIn)) => true,
            Message::Pane(_, pane::Message::Tab(_, inner)) => matches!(
                Self::reviewer_unwrap_guarded_message(inner),
                tab::Message::Run(AppMessage::View(crate::app::view::Message::ConnectAccount(
                    _
                ))) | tab::Message::Launch(home::Message::View(home::ViewMessage::ConnectAccount(
                    _
                ))) | tab::Message::Launch(home::Message::ConnectSignedInBubble)
                    | tab::Message::ConnectSignedIn
            ),
            _ => false,
        }
    }
    pub(crate) fn reviewer_app_mut(&mut self, index: usize) -> &mut crate::app::App {
        let pane = self.panes.get_mut(self.focus.unwrap()).unwrap();
        let tab::State::App(app) = &mut pane.tabs[index].state else {
            panic!("Bitcoin App closed")
        };
        app
    }
    pub(crate) fn reviewer_theme_toggle(&mut self, tab_id: usize) -> Task<Message> {
        self.update(Message::Pane(
            self.focus.unwrap(),
            pane::Message::tab(
                tab_id,
                tab::Message::Run(AppMessage::View(crate::app::view::Message::ToggleTheme)),
            ),
        ))
    }
    pub(crate) fn reviewer_account(
        &mut self,
        id: usize,
        account: crate::app::view::ConnectAccountMessage,
    ) -> Task<Message> {
        self.update(Message::Pane(
            self.focus.unwrap(),
            pane::Message::tab(
                id,
                tab::Message::Run(AppMessage::View(crate::app::view::Message::ConnectAccount(
                    account,
                ))),
            ),
        ))
    }
    /// Whether a GUI message is a tab's Connect account message of any
    /// kind (a completion, a re-dispatched `LogOut`), so a test can drive
    /// exactly those through the real GUI and leave network bootstrap alone.
    pub(crate) fn reviewer_is_account_message(message: &Message) -> bool {
        matches!(message, Message::Pane(_, pane::Message::Tab(_, inner))
            if matches!(Self::reviewer_unwrap_guarded_message(inner), tab::Message::Run(AppMessage::View(
                crate::app::view::Message::ConnectAccount(_)))))
    }
    /// Whether a GUI message is a tab's `SetSession` completion — what a
    /// real refresh or login operation hands back — so a test can route the
    /// real completion through the real broadcaster.
    pub(crate) fn reviewer_is_set_session(message: &Message) -> bool {
        matches!(message, Message::Pane(_, pane::Message::Tab(_, inner))
            if matches!(Self::reviewer_unwrap_guarded_message(inner), tab::Message::Run(AppMessage::View(
                crate::app::view::Message::ConnectAccount(
                    crate::app::view::ConnectAccountMessage::SetSession(..))))))
    }

    #[cfg(test)]
    pub(crate) fn reviewer_is_claim_or_session_loaded(message: &Message) -> bool {
        matches!(message, Message::Pane(_, pane::Message::Tab(_, inner))
            if matches!(Self::reviewer_unwrap_guarded_message(inner),
                tab::Message::Run(AppMessage::Claim(_)
                    | AppMessage::View(crate::app::view::Message::ConnectAccount(
                        crate::app::view::ConnectAccountMessage::SessionLoaded { .. })))))
    }

    #[cfg(test)]
    pub(crate) fn reviewer_is_session_loaded(message: &Message) -> bool {
        matches!(message, Message::Pane(_, pane::Message::Tab(_, inner))
            if matches!(Self::reviewer_unwrap_guarded_message(inner),
                tab::Message::Run(AppMessage::View(crate::app::view::Message::ConnectAccount(
                    crate::app::view::ConnectAccountMessage::SessionLoaded { .. })))))
    }
}

#[cfg(test)]
mod claim_return_routing_tests {
    use super::*;
    use crate::app::{
        self,
        menu::{Menu, VaultSubMenu},
        view,
    };
    use iced::futures::StreamExt;

    #[test]
    fn missing_imported_credentials_error_cannot_cross_same_tab_reopen() {
        let result = std::thread::Builder::new()
            .name("held-imported-credentials".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let _enter = runtime.enter();
                let _guard = app::session::test_guard();
                let path = std::env::temp_dir().join(format!(
                    "held-imported-credentials-{}",
                    uuid::Uuid::new_v4()
                ));
                let source = app::claim_step1_tests::imported_app_ready_for_claim(&path);
                let source_id = source.cube_settings().id.clone();
                let root = source.datadir().clone();
                let (home, startup) = home::Home::new_for_chain(root.clone(), None);
                drop(startup);
                let mut gui = GUI::reviewer_app_home_gui(source, home);
                let pane_id = gui.focus.unwrap();
                let old_generation = gui.panes.get(pane_id).unwrap().tabs[0]
                    .app_generation_token()
                    .unwrap();
                let held = runtime.block_on(async {
                    let task = gui.update(Message::Pane(
                        pane_id,
                        pane::Message::tab(
                            1,
                            tab::Message::Run(AppMessage::View(view::Message::StartImportedClaim)),
                        ),
                    ));
                    let mut stream = iced_runtime::task::into_stream(task)
                        .expect("missing credentials emit a Pane error task");
                    while let Some(action) = stream.next().await {
                        if let iced_runtime::Action::Output(message) = action {
                            return message;
                        }
                    }
                    panic!("missing-credential error was not emitted")
                });
                assert!(
                    matches!(&held, Message::Pane(_, pane::Message::Tab(1, inner))
                    if matches!(inner.as_ref(), tab::Message::AppAsync(generation, nested)
                        if *generation == old_generation
                            && matches!(nested.as_ref(), tab::Message::Run(
                                AppMessage::View(view::Message::ShowError(error))
                            ) if error.contains("Unlock the source Cube"))))
                );
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(1, tab::Message::LockCube),
                )));
                assert!(matches!(
                    gui.panes.get(pane_id).unwrap().tabs[0].state,
                    tab::State::Home(_)
                ));
                let replacement_path = std::env::temp_dir().join(format!(
                    "imported-credentials-new-owner-{}",
                    uuid::Uuid::new_v4()
                ));
                let replacement =
                    app::claim_step1_tests::bitcoin_app_with_id(&replacement_path, &source_id);
                let cache = replacement.cache().clone();
                let wallet = std::sync::Arc::new(replacement.wallet().unwrap().clone());
                let daemon = app::claim_step1_tests::fixture_daemon(&replacement);
                let settings = replacement.cube_settings().clone();
                drop(replacement);
                let breez = std::sync::Arc::new(app::breez_liquid::BreezClient::disconnected(
                    settings.network.bitcoin_network(),
                ));
                let (loader, startup) = crate::loader::Loader::new(
                    CoincubeDirectory::new(replacement_path.clone()),
                    app::Config::new(false),
                    settings.network.bitcoin_network(),
                    None,
                    None,
                    None,
                    settings.clone(),
                    Some(breez.clone()),
                    None,
                );
                drop(startup);
                gui.panes.get_mut(pane_id).unwrap().tabs[0].state = tab::State::Loader(loader);
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        1,
                        tab::Message::Load(crate::loader::Message::BreezLoaded {
                            breez,
                            spark_backend: None,
                            cache,
                            wallet,
                            config: app::Config::new(false),
                            daemon,
                            datadir: CoincubeDirectory::new(replacement_path.clone()),
                            bitcoind: None,
                            cube_settings: settings,
                        }),
                    ),
                )));
                assert_ne!(
                    gui.panes.get(pane_id).unwrap().tabs[0]
                        .app_generation_token()
                        .unwrap(),
                    old_generation
                );
                assert_eq!(gui.update(held).units(), 0);
                assert!(matches!(
                    gui.panes.get(pane_id).unwrap().tabs[0].state,
                    tab::State::App(_)
                ));
                drop(gui);
                std::fs::remove_dir_all(path).ok();
                std::fs::remove_dir_all(replacement_path).ok();
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn automatic_deposit_checks_refresh_the_paired_cube_binding() {
        use crate::app::state::vault::claim::{ClaimEvent, DepositStatus};
        use crate::services::foreign_scan::AddressFunds;
        assert!(refreshes_claim_pair(&AppMessage::Claim(
            ClaimEvent::DepositPoll
        )));
        assert!(refreshes_claim_pair(&AppMessage::Claim(
            ClaimEvent::DepositStatusChecked(
                1,
                1,
                0,
                DepositStatus {
                    bitcoin: Ok(AddressFunds::default()),
                    btcb2: Ok(AddressFunds::default())
                },
            )
        )));
        assert!(refreshes_claim_pair(&AppMessage::View(
            view::Message::Claim(view::ClaimMessage::SignPair)
        )));
        assert!(!refreshes_claim_pair(&AppMessage::View(
            view::Message::Claim(view::ClaimMessage::Cancel)
        )));
    }

    #[test]
    fn claim_return_routes_to_existing_tab_across_panes_and_refuses_wrong_wallet() {
        // Real GUI/App values need the same stack allowance as the app thread.
        let result = std::thread::Builder::new()
            .name("claim-return-routing".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(check_routes())
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn held_direct_return_cannot_navigate_a_same_id_reopened_app() {
        let result = std::thread::Builder::new()
            .name("held-direct-return".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let _enter = runtime.enter();
                let _guard = app::session::test_guard();
                let (source, handoff, path) =
                    app::claim_step1_tests::returning_claim_fixture(false);
                let source_id = source.cube_settings().id.clone();
                let root = source.datadir().clone();
                let (home, startup) = home::Home::new_for_chain(root.clone(), None);
                drop(startup);
                let mut gui = GUI::reviewer_app_home_gui(source, home);
                let pane_id = gui.focus.unwrap();
                let old_generation = gui.panes.get(pane_id).unwrap().tabs[0]
                    .app_generation_token()
                    .unwrap();
                app::session::open(source_id.clone(), zeroize::Zeroizing::new("2468".into()));
                let held = runtime.block_on(async {
                    let task = gui.update(Message::Pane(
                        pane_id,
                        pane::Message::View(pane::ViewMessage::OpenBitcoinClaim(handoff)),
                    ));
                    let mut stream = iced_runtime::task::into_stream(task).unwrap();
                    while let Some(action) = stream.next().await {
                        if let iced_runtime::Action::Output(message) = action {
                            return message;
                        }
                    }
                    panic!("direct Return must emit an App route")
                });
                assert!(
                    matches!(&held, Message::Pane(_, pane::Message::Tab(1, inner))
                    if matches!(inner.as_ref(), tab::Message::AppAsync(generation, _)
                        if *generation == old_generation))
                );
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(1, tab::Message::LockCube),
                )));
                assert!(matches!(
                    gui.panes.get(pane_id).unwrap().tabs[0].state,
                    tab::State::Home(_)
                ));

                // The ordinary synthetic Loader completion reuses the tab ID
                // and creates a distinct App owner, as in the shared-client
                // hold regression. No keychain or daemon process starts.
                let replacement = app::claim_step1_tests::bitcoin_app_with_id(&path, &source_id);
                let cache = replacement.cache().clone();
                let wallet = std::sync::Arc::new(replacement.wallet().unwrap().clone());
                let daemon = app::claim_step1_tests::fixture_daemon(&replacement);
                let settings = replacement.cube_settings().clone();
                drop(replacement);
                let breez = std::sync::Arc::new(app::breez_liquid::BreezClient::disconnected(
                    settings.network.bitcoin_network(),
                ));
                let (loader, startup) = crate::loader::Loader::new(
                    root.clone(),
                    app::Config::new(false),
                    settings.network.bitcoin_network(),
                    None,
                    None,
                    None,
                    settings.clone(),
                    Some(breez.clone()),
                    None,
                );
                drop(startup);
                gui.panes.get_mut(pane_id).unwrap().tabs[0].state = tab::State::Loader(loader);
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        1,
                        tab::Message::Load(crate::loader::Message::BreezLoaded {
                            breez,
                            spark_backend: None,
                            cache,
                            wallet,
                            config: app::Config::new(false),
                            daemon,
                            datadir: root,
                            bitcoind: None,
                            cube_settings: settings,
                        }),
                    ),
                )));
                let current_generation = gui.panes.get(pane_id).unwrap().tabs[0]
                    .app_generation_token()
                    .unwrap();
                assert_ne!(old_generation, current_generation);
                let menu = gui.reviewer_app_mut(0).current_menu_for_test().clone();
                assert_eq!(gui.update(held).units(), 0);
                assert_eq!(gui.reviewer_app_mut(0).current_menu_for_test(), &menu);
                drop(gui);
                app::session::close();
                std::fs::remove_dir_all(path).ok();
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[allow(clippy::await_holding_lock)] // serialize process-global Claim intent and PIN sessions
    async fn check_routes() {
        let _guard = app::session::test_guard();
        for cross_pane in [false, true] {
            for wrong_wallet in [false, true] {
                let (app, handoff, path) =
                    app::claim_step1_tests::returning_claim_fixture(wrong_wallet);
                let root = app.datadir().clone();
                let (home, startup) = home::Home::new_for_chain(root.clone(), None);
                drop(startup);
                let origin_pane = pane::Pane::new_with_tab(tab::State::Home(home));
                let (mut panes, origin) = pane_grid::State::new(origin_pane);
                let (target, index, tab_id) = if cross_pane {
                    let (target, _) = panes
                        .split(
                            pane_grid::Axis::Vertical,
                            origin,
                            pane::Pane::new_with_tab(tab::State::App(app)),
                        )
                        .unwrap();
                    (target, 0, 1)
                } else {
                    panes
                        .get_mut(origin)
                        .unwrap()
                        .tabs
                        .push(tab::Tab::new(2, tab::State::App(app)));
                    (origin, 1, 2)
                };
                let mut gui = GUI {
                    panes,
                    focus: Some(origin),
                    config: Config::new(root, None),
                    window_id: None,
                    window_init: None,
                    window_config: None,
                    global_cache: GlobalCache::default(),
                    theme_mode: Default::default(),
                    system_theme: None,
                    system_reduces_motion: None,
                    preferences_sheet: None,
                    preferences: unresolved_test_preferences(),
                };
                let source_id = match &gui.panes.get(target).unwrap().tabs[index].state {
                    tab::State::App(app) => app.cube_settings().id.clone(),
                    _ => unreachable!(),
                };
                app::session::open(source_id, zeroize::Zeroizing::new("2468".into()));
                let task = gui.update(Message::Pane(
                    origin,
                    pane::Message::View(pane::ViewMessage::OpenBitcoinClaim(handoff)),
                ));
                assert_eq!(gui.focus, Some(target));
                assert_eq!(gui.panes.get(target).unwrap().focused_tab, index);
                assert_eq!(
                    gui.panes.iter().map(|(_, p)| p.tabs.len()).sum::<usize>(),
                    2,
                    "return must not create a second wallet owner"
                );
                assert!(matches!(
                    gui.panes.get(target).unwrap().tabs[index].state,
                    tab::State::App(_)
                ));
                let mut stream =
                    iced_runtime::task::into_stream(task).expect("navigation or refusal");
                let mut outputs = 0;
                while let Some(action) = stream.next().await {
                    if let iced_runtime::Action::Output(message) = action {
                        outputs += 1;
                        match message {
                            Message::Pane(pane_id, pane::Message::Tab(id, inner)) => {
                                let tab::Message::Run(message) =
                                    GUI::reviewer_unwrap_guarded_message(&inner)
                                else {
                                    panic!("unexpected tab route")
                                };
                                assert_eq!(
                                    (pane_id, id),
                                    (target, tab_id),
                                    "route back to the selected owner"
                                );
                                if wrong_wallet {
                                    assert!(
                                        matches!(message, AppMessage::View(view::Message::ShowError(error))
                                        if error.contains("does not match"))
                                    );
                                } else {
                                    assert!(matches!(
                                        message,
                                        AppMessage::View(view::Message::Menu(Menu::Vault(
                                            VaultSubMenu::Claim
                                        )))
                                    ));
                                }
                                // Deliver the actual guarded task result. A
                                // route that only *looks* correct before
                                // generation validation is not acceptance.
                                drop(
                                    gui.update(Message::Pane(
                                        pane_id,
                                        pane::Message::Tab(id, inner),
                                    )),
                                );
                            }
                            other => panic!("unexpected return route: {:?}", other),
                        }
                    }
                }
                assert_eq!(
                    outputs, 1,
                    "exactly one navigation or refusal, no duplicate startup"
                );
                drop(gui);
                app::session::close();
                std::fs::remove_dir_all(path).unwrap();
            }
        }
    }
}

#[cfg(test)]
mod single_open_lifecycle_tests {
    use super::*;
    use crate::{app, chain::ChainId};

    #[test]
    fn rejected_return_preserves_both_contexts_and_accepted_return_retires_only_the_legacy_owner() {
        let result = std::thread::Builder::new()
            .name("return-counted-owners".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let _enter = runtime.enter();
                let _guard = app::session::test_guard();
                let (mut source, handoff, path) =
                    app::claim_step1_tests::returning_claim_fixture(false);
                let root = source.datadir().clone();
                let source_settings = source.cube_settings().clone();
                let source_id = source_settings.id.clone();
                let owner = source.cache().app_generation;
                let source_stops = app::tests::reviewer_attach_counted_claim_context(&mut source);
                let (mut legacy, _) = app::claim_step1_tests::bitcoin_app(&path);
                let legacy_stops = app::tests::reviewer_attach_counted_claim_context(&mut legacy);
                let (home, startup) = home::Home::new_for_chain(root.clone(), None);
                drop(startup);
                let mut gui = GUI::reviewer_claim_gui(source, legacy);
                let pane_id = gui.focus.unwrap();
                gui.panes
                    .get_mut(pane_id)
                    .unwrap()
                    .tabs
                    .push(tab::Tab::new(3, tab::State::Home(home)));
                app::session::open(source_id.clone(), zeroize::Zeroizing::new("2468".into()));
                let rejected = home::ClaimOpenIntent::new(
                    app::cache::AppGeneration::next(),
                    &root,
                    source_id.clone(),
                    handoff.fork_cube().to_owned(),
                    home::ClaimRoute::Return(handoff.clone()),
                );
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        3,
                        tab::Message::Launch(home::Message::RunClaim(
                            root.clone(),
                            app::Config::new(false),
                            ChainId::Bitcoin,
                            source_settings.clone(),
                            rejected,
                        )),
                    ),
                )));
                assert_eq!(
                    gui.panes
                        .get(pane_id)
                        .unwrap()
                        .tabs
                        .iter()
                        .filter(|tab| matches!(tab.state, tab::State::App(_)))
                        .count(),
                    2,
                );
                assert_eq!(source_stops.load(std::sync::atomic::Ordering::SeqCst), 0);
                assert_eq!(legacy_stops.load(std::sync::atomic::Ordering::SeqCst), 0);

                app::claim_intent::arm(source_id.clone());
                let accepted = home::ClaimOpenIntent::new(
                    owner,
                    &root,
                    source_id.clone(),
                    handoff.fork_cube().to_owned(),
                    home::ClaimRoute::Return(handoff),
                );
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        3,
                        tab::Message::Launch(home::Message::RunClaim(
                            root.clone(),
                            app::Config::new(false),
                            ChainId::Bitcoin,
                            source_settings,
                            accepted,
                        )),
                    ),
                )));
                assert_eq!(source_stops.load(std::sync::atomic::Ordering::SeqCst), 0);
                assert_eq!(legacy_stops.load(std::sync::atomic::Ordering::SeqCst), 1);
                assert_eq!(
                    gui.panes
                        .get(pane_id)
                        .unwrap()
                        .tabs
                        .iter()
                        .filter(|tab| matches!(tab.state, tab::State::App(_)))
                        .count(),
                    1,
                );
                assert_eq!(app::session::current_cube_id(), Some(source_id));
                drop(gui);
                app::session::close();
                std::fs::remove_dir_all(path).ok();
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn accepted_create_import_and_restore_retire_the_old_app_before_installer() {
        let result = std::thread::Builder::new()
            .name("installer-admission-owner".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let _enter = runtime.enter();
                let _guard = app::session::test_guard();
                for flow in [
                    crate::installer::UserFlow::CreateWallet,
                    crate::installer::UserFlow::AddWallet,
                    crate::installer::UserFlow::RestoreFromRecoveryKit { cube_uuid: None },
                ] {
                    let path = std::env::temp_dir()
                        .join(format!("installer-admission-{}", uuid::Uuid::new_v4()));
                    let (source, _) = app::claim_step1_tests::bitcoin_app(&path);
                    let source_id = source.cube_settings().id.clone();
                    let root = source.datadir().clone();
                    let (home, startup) = home::Home::new_for_chain(root.clone(), None);
                    drop(startup);
                    let mut gui = GUI::reviewer_app_home_gui(source, home);
                    let pane_id = gui.focus.unwrap();
                    let login_settings = app::settings::WalletSettings {
                        name: "Synthetic remote shell".into(),
                        alias: None,
                        descriptor_checksum: "remote-shell".into(),
                        pinned_at: None,
                        keys: Vec::new(),
                        hardware_wallets: Vec::new(),
                        remote_backend_auth: Some(app::settings::AuthConfig::new(
                            "test@example.invalid".into(),
                            "remote-wallet".into(),
                        )),
                        start_internal_bitcoind: None,
                        pending_rescan: None,
                        keychain_keys_recorded: false,
                    };
                    let (login, startup) = crate::services::connect::login::CoincubeLiteLogin::new(
                        root.clone(),
                        bitcoin::Network::Bitcoin,
                        login_settings,
                        None,
                        None,
                    );
                    drop(startup);
                    gui.panes
                        .get_mut(pane_id)
                        .unwrap()
                        .tabs
                        .push(tab::Tab::new(3, tab::State::Login(login)));
                    app::session::open(source_id, zeroize::Zeroizing::new("2468".into()));
                    drop(gui.update(Message::Pane(
                        pane_id,
                        pane::Message::tab(
                            2,
                            tab::Message::Launch(home::Message::Install(
                                root,
                                ChainId::Bitcoin,
                                flow,
                                None,
                            )),
                        ),
                    )));
                    let pane = gui.panes.get(pane_id).unwrap();
                    assert!(pane
                        .tabs
                        .iter()
                        .all(|tab| !matches!(tab.state, tab::State::App(_))));
                    assert!(pane
                        .tabs
                        .iter()
                        .all(|tab| !matches!(tab.state, tab::State::Login(_))));
                    assert!(pane
                        .tabs
                        .iter()
                        .any(|tab| matches!(tab.state, tab::State::Installer(_))));
                    assert!(!app::session::is_open());
                    drop(gui);
                    std::fs::remove_dir_all(path).ok();
                }

                let path = std::env::temp_dir().join(format!(
                    "split-installer-admission-{}",
                    uuid::Uuid::new_v4()
                ));
                let (source, _) = app::claim_step1_tests::bitcoin_app(&path);
                let source_id = source.cube_settings().id.clone();
                let root = source.datadir().clone();
                let (mut home, startup) = home::Home::new_for_chain(root.clone(), None);
                drop(startup);
                drop(home.update(home::Message::GlobalFeatureLoaded(0, true)));
                let mut client =
                    crate::services::coincube::CoincubeClient::for_test("https://connect.example/");
                client.set_token("synthetic-split-session");
                home.connect_account.install_admitted_client(client.clone());
                home.connect_account.user = Some(crate::services::coincube::User {
                    id: 7,
                    email: "synthetic@example.invalid".into(),
                    email_verified: Some(true),
                });
                home.connect_account.step =
                    crate::app::state::connect::account::ConnectFlowStep::Dashboard;
                home.connect_account.bitcoin_blake2b_opt_in = true;
                home.connect_account.features = Some(
                    serde_json::from_value(serde_json::json!({
                        "plans": [], "bitcoinBlake2bEnabled": true,
                    }))
                    .unwrap(),
                );
                let mut gui = GUI::reviewer_app_home_gui(source, home);
                let pane_id = gui.focus.unwrap();
                app::session::open(source_id, zeroize::Zeroizing::new("2468".into()));
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        2,
                        tab::Message::Launch(home::Message::InstallForSplit(root, client)),
                    ),
                )));
                let pane = gui.panes.get(pane_id).unwrap();
                assert!(pane
                    .tabs
                    .iter()
                    .all(|tab| !matches!(tab.state, tab::State::App(_))));
                assert!(pane
                    .tabs
                    .iter()
                    .any(|tab| matches!(tab.state, tab::State::Installer(_))));
                assert!(!app::session::is_open());
                drop(gui);
                std::fs::remove_dir_all(path).ok();
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn accepted_remote_cube_open_retires_old_owner_before_login_screen() {
        let result = std::thread::Builder::new()
            .name("remote-login-admission".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let _enter = runtime.enter();
                let _guard = app::session::test_guard();
                let path = std::env::temp_dir()
                    .join(format!("remote-login-admission-{}", uuid::Uuid::new_v4()));
                let (mut source, wallet) = app::claim_step1_tests::bitcoin_app(&path);
                let stopped = app::tests::reviewer_attach_counted_claim_context(&mut source);
                let source_id = source.cube_settings().id.clone();
                let root = source.datadir().clone();
                let target =
                    app::settings::CubeSettings::new("Remote Cube".into(), ChainId::Bitcoin)
                        .with_vault(app::settings::VaultIdentity::new(
                            wallet.id(),
                            wallet.main_descriptor.as_vault(),
                        ));
                let remote_settings = app::settings::WalletSettings {
                    name: "Remote Vault".into(),
                    alias: None,
                    descriptor_checksum: wallet.descriptor_checksum.clone(),
                    pinned_at: wallet.pinned_at,
                    keys: Vec::new(),
                    hardware_wallets: Vec::new(),
                    remote_backend_auth: Some(app::settings::AuthConfig::new(
                        "test@example.invalid".into(),
                        "remote-wallet".into(),
                    )),
                    start_internal_bitcoind: None,
                    pending_rescan: None,
                    keychain_keys_recorded: false,
                };
                let directory = root.network_directory(ChainId::Bitcoin);
                directory.init().unwrap();
                std::fs::write(
                    directory.path().join(app::settings::SETTINGS_FILE_NAME),
                    serde_json::to_vec(&app::settings::Settings {
                        cubes: vec![target.clone()],
                        wallets: vec![remote_settings.clone()],
                        ..Default::default()
                    })
                    .unwrap(),
                )
                .unwrap();
                let (home, startup) = home::Home::new_for_chain(root.clone(), None);
                drop(startup);
                let mut gui = GUI::reviewer_app_home_gui(source, home);
                let pane_id = gui.focus.unwrap();
                app::session::open(source_id, zeroize::Zeroizing::new("2468".into()));
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        2,
                        tab::Message::Launch(home::Message::Run(
                            root.clone(),
                            app::Config::new(false),
                            ChainId::Bitcoin,
                            target.clone(),
                        )),
                    ),
                )));
                assert_eq!(stopped.load(std::sync::atomic::Ordering::SeqCst), 1);
                assert!(!app::session::is_open());
                assert!(matches!(
                    gui.panes.get(pane_id).unwrap().tabs[0].state,
                    tab::State::PinEntry(_)
                ));
                // The public unlock callback is synthetic; the real Tab
                // completion chooses Login from the saved remote wallet type.
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        2,
                        tab::Message::BreezClientLoadedAfterPin {
                            connect_client: None,
                            migration_error: None,
                            enc_key_error: None,
                            breez_client: Err(app::breez_liquid::BreezError::SignerError(
                                "fixture".into(),
                            )),
                            spark_backend: None,
                            config: app::Config::new(false),
                            datadir: root.clone(),
                            network: bitcoin::Network::Bitcoin,
                            cube: target,
                            wallet_settings: Some(remote_settings),
                            internal_bitcoind: None,
                            backup: None,
                        },
                    ),
                )));
                assert!(matches!(
                    gui.panes.get(pane_id).unwrap().tabs[0].state,
                    tab::State::Login(_)
                ));
                assert_eq!(stopped.load(std::sync::atomic::Ordering::SeqCst), 1);
                drop(gui);
                std::fs::remove_dir_all(path).ok();
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn deleting_a_cube_retires_its_live_app_and_process_session_first() {
        let result = std::thread::Builder::new()
            .name("delete-admission-owner".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let _enter = runtime.enter();
                let _guard = app::session::test_guard();
                let path =
                    std::env::temp_dir().join(format!("delete-admission-{}", uuid::Uuid::new_v4()));
                let (source, _) = app::claim_step1_tests::bitcoin_app(&path);
                let cube_id = source.cube_settings().id.clone();
                let (home, startup) = home::Home::new_for_chain(source.datadir().clone(), None);
                drop(startup);
                let mut gui = GUI::reviewer_app_home_gui(source, home);
                let pane_id = gui.focus.unwrap();
                app::session::open(cube_id.clone(), zeroize::Zeroizing::new("2468".into()));
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        2,
                        tab::Message::Launch(home::Message::View(home::ViewMessage::DeleteCube(
                            home::DeleteCubeMessage::Confirm(cube_id),
                        ))),
                    ),
                )));
                assert!(!app::session::is_open());
                assert!(gui
                    .panes
                    .iter()
                    .flat_map(|(_, pane)| pane.tabs.iter())
                    .all(|tab| !matches!(tab.state, tab::State::App(_))));
                drop(gui);
                std::fs::remove_dir_all(path).ok();
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn cold_local_and_connect_fork_constructors_consume_gui_staged_foreign_split() {
        let result = std::thread::Builder::new()
            .name("cold-fork-split-handoff".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let _enter = runtime.enter();
                let _guard = app::session::test_guard();
                for connect in [false, true] {
                    let path = std::env::temp_dir()
                        .join(format!("cold-fork-split-{}", uuid::Uuid::new_v4()));
                    let root = CoincubeDirectory::new(path.clone());
                    std::fs::create_dir_all(root.path()).unwrap();
                    crate::app::settings::global::GlobalSettings::update_bitcoin_blake2b_beta(
                        &crate::app::settings::global::GlobalSettings::path(&root),
                        true,
                    )
                    .unwrap();
                    let (mut config, wallet_settings) =
                        crate::chain::managed_local_fixture(&root, ChainId::BitcoinBlake2b);
                    let mut client = crate::services::coincube::CoincubeClient::for_test(
                        "https://connect.example/",
                    );
                    client.set_token("synthetic-target-session");
                    if connect {
                        config.bitcoin_backend = Some(coincubed::config::BitcoinBackend::Esplora(
                            coincubed::config::EsploraConfig {
                                addr: format!(
                                    "{}/api/v1/esplora/{}",
                                    client.base_url.trim_end_matches('/'),
                                    crate::installer::connect_esplora_path(ChainId::BitcoinBlake2b),
                                ),
                                token: client.token().map(str::to_owned),
                                fallback_addr: None,
                                fallback_token: None,
                                secondary_fallback_addr: None,
                                secondary_fallback_token: None,
                            },
                        ));
                        let config_path = root
                            .network_directory(ChainId::BitcoinBlake2b)
                            .coincubed_data_directory(&wallet_settings.wallet_id())
                            .path()
                            .join("daemon.toml");
                        std::fs::write(config_path, toml::to_string(&config).unwrap()).unwrap();
                    }
                    let wallet = std::sync::Arc::new(
                        crate::app::wallet::Wallet::new(
                            config.main_descriptor.as_vault().unwrap().clone(),
                        )
                        .with_chain(ChainId::BitcoinBlake2b)
                        .load_from_settings(wallet_settings.clone())
                        .unwrap(),
                    );
                    let daemon = std::sync::Arc::new(
                        crate::daemon::embedded::EmbeddedDaemon::unstarted_for_test(config, None),
                    );
                    let signer =
                        coincube_core::signer::MasterSigner::generate(bitcoin::Network::Bitcoin)
                            .unwrap();
                    let cek = std::sync::Arc::new(
                        crate::services::connect::crypto::CubeEncryptionKey::derive(
                            &signer,
                            bitcoin::Network::Bitcoin,
                        ),
                    );
                    let cube = app::settings::CubeSettings::new(
                        "Foreign target".into(),
                        ChainId::BitcoinBlake2b,
                    )
                    .with_vault(app::settings::VaultIdentity::new(
                        wallet.id(),
                        wallet.main_descriptor.as_vault(),
                    ));
                    std::fs::write(
                        root.network_directory(ChainId::BitcoinBlake2b)
                            .path()
                            .join(app::settings::SETTINGS_FILE_NAME),
                        serde_json::to_vec(&app::settings::Settings {
                            cubes: vec![cube.clone()],
                            wallets: vec![wallet_settings.clone()],
                            ..Default::default()
                        })
                        .unwrap(),
                    )
                    .unwrap();
                    let target_id = cube.id.clone();
                    let (mut home, startup) = home::Home::new_for_chain(root.clone(), None);
                    drop(startup);
                    drop(home.update(home::Message::GlobalFeatureLoaded(0, true)));
                    home.connect_account.bitcoin_blake2b_opt_in = true;
                    if connect {
                        home.connect_account.install_admitted_client(client.clone());
                        home.connect_account.user = Some(crate::services::coincube::User {
                            id: 7,
                            email: "synthetic@example.invalid".into(),
                            email_verified: Some(true),
                        });
                        home.connect_account.step =
                            crate::app::state::connect::account::ConnectFlowStep::Dashboard;
                        home.connect_account.bitcoin_blake2b_opt_in = true;
                        home.connect_account.features = Some(
                            serde_json::from_value(serde_json::json!({
                                "plans": [], "bitcoinBlake2bEnabled": true,
                            }))
                            .unwrap(),
                        );
                    }
                    let pane = pane::Pane::new_with_tab(tab::State::Home(home));
                    let (panes, pane_id) = pane_grid::State::new(pane);
                    let mut gui = GUI {
                        panes,
                        focus: Some(pane_id),
                        config: Config::new(root.clone(), None),
                        window_id: None,
                        window_init: None,
                        window_config: None,
                        global_cache: GlobalCache::default(),
                        theme_mode: Default::default(),
            system_theme: None,
            system_reduces_motion: None,
            preferences_sheet: None,
            preferences: unresolved_test_preferences(),
                    };
                    if connect {
                        // Return from an accepted ordinary Split prerequisite
                        // install before continuing the same scan through the
                        // cold fork Loader::Synced constructor.
                        drop(gui.update(Message::Pane(
                            pane_id,
                            pane::Message::tab(
                                1,
                                tab::Message::Launch(home::Message::InstallForSplit(
                                    root.clone(),
                                    client.clone(),
                                )),
                            ),
                        )));
                        assert!(matches!(
                            gui.panes.get(pane_id).unwrap().tabs[0].state,
                            tab::State::Installer(_)
                        ));
                        let install_generation = gui.panes.get(pane_id).unwrap().tabs[0]
                            .reviewer_fork_session_generation();
                        drop(gui.update(Message::Pane(
                            pane_id,
                            pane::Message::tab(
                                1,
                                tab::Message::ForkInstallCompleted(
                                    install_generation,
                                    crate::installer::Message::CubeSaved(
                                        Ok((cube.clone(), None, None)),
                                        None,
                                        None,
                                    ),
                                ),
                            ),
                        )));
                        let tab::State::Home(returned) =
                            &mut gui.panes.get_mut(pane_id).unwrap().tabs[0].state
                        else {
                            panic!("saved Split prerequisite must return to the scanner")
                        };
                        assert!(
                            returned.reviewer_split_resume_pending(),
                            "accepted install must arm a fresh scanner resume"
                        );
                        drop(returned.update(home::Message::GlobalFeatureLoaded(0, true)));
                        assert!(returned.reviewer_split_resume_pending());
                        returned
                            .connect_account
                            .install_admitted_client(client.clone());
                        returned.connect_account.user = Some(crate::services::coincube::User {
                            id: 7,
                            email: "synthetic@example.invalid".into(),
                            email_verified: Some(true),
                        });
                        returned.connect_account.step =
                            crate::app::state::connect::account::ConnectFlowStep::Dashboard;
                        returned.connect_account.bitcoin_blake2b_opt_in = true;
                        let generation = returned.connect_account.session_generation();
                        drop(
                            returned.update(home::Message::View(
                                home::ViewMessage::ConnectAccount(
                                    crate::app::view::ConnectAccountMessage::FeaturesLoaded(
                                        Some(
                                            serde_json::from_value(serde_json::json!({
                                                "plans": [], "bitcoinBlake2bEnabled": true,
                                            }))
                                            .unwrap(),
                                        ),
                                        generation,
                                    ),
                                ),
                            )),
                        );
                        assert!(
                            returned.reviewer_split_wallet_active(),
                            "returned scanner did not resume Split: error={:?} pending={} authenticated={} features={} availability={:?}",
                            returned.error(),
                            returned.reviewer_split_resume_pending(),
                            returned.connect_account.is_authenticated(),
                            returned.connect_account.features.is_some(),
                            returned.connect_chain_availability(ChainId::BitcoinBlake2b),
                        );
                    }
                    app::session::open("former-source", zeroize::Zeroizing::new("2468".into()));
                    app::split_intent::arm_fresh_for_test(&target_id);
                    drop(gui.update(Message::Pane(
                        pane_id,
                        pane::Message::tab(
                            1,
                            tab::Message::Launch(home::Message::Run(
                                root.clone(),
                                app::Config::new(false),
                                ChainId::BitcoinBlake2b,
                                cube.clone(),
                            )),
                        ),
                    )));
                    assert!(!app::session::is_open(), "the former session was retired");
                    assert!(matches!(
                        gui.panes.get(pane_id).unwrap().tabs[0].state,
                        tab::State::PinEntry(_)
                    ));
                    assert!(
                        app::split_intent::take_for_open(&target_id, ChainId::BitcoinBlake2b)
                            .is_none(),
                        "scan is privately staged through unlock"
                    );

                    // Supply the accepted unlock/daemon result without
                    // invoking a keychain, native node, or network. The event
                    // still enters through GUI and Tab's real Loader::Synced arm.
                    let (loader, startup) = if connect {
                        crate::loader::Loader::new_for_chain(
                            root.clone(),
                            app::Config::new(false),
                            wallet_settings,
                            cube.clone(),
                            client,
                            cek,
                        )
                    } else {
                        crate::loader::Loader::new_local_fork(
                            root.clone(),
                            app::Config::new(false),
                            wallet_settings,
                            cube.clone(),
                            cek,
                        )
                    };
                    drop(startup);
                    gui.panes.get_mut(pane_id).unwrap().tabs[0].state = tab::State::Loader(loader);
                    let reopened_cube = cube.clone();
                    drop(gui.update(Message::Pane(
                        pane_id,
                        pane::Message::tab(
                            1,
                            tab::Message::Load(crate::loader::Message::Synced(Ok((
                                wallet,
                                app::cache::Cache {
                                    fiat_chain: ChainId::BitcoinBlake2b,
                                    ..Default::default()
                                },
                                daemon,
                                None,
                                None,
                                cube,
                            )))),
                        ),
                    )));
                    let tab::State::App(opened) = &gui.panes.get(pane_id).unwrap().tabs[0].state
                    else {
                        panic!(
                            "accepted fork must construct the selected App; connect={}",
                            connect
                        )
                    };
                    assert_eq!(
                        opened.reviewer_pending_split_target(),
                        Some(target_id.as_str())
                    );
                    assert!(
                        app::split_intent::take_for_open(&target_id, ChainId::BitcoinBlake2b)
                            .is_none(),
                        "constructor consumed the one-use scan"
                    );
                    let owner = opened.cache().app_generation;
                    assert_eq!(opened.reviewer_pending_split_generation(), Some(1));

                    let (mut home, startup) = home::Home::new_for_chain(root.clone(), None);
                    drop(startup);
                    drop(home.update(home::Message::GlobalFeatureLoaded(0, true)));
                    home.connect_account.bitcoin_blake2b_opt_in = true;
                    let pane = gui.panes.get_mut(pane_id).unwrap();
                    pane.tabs.push(tab::Tab::new(2, tab::State::Home(home)));
                    pane.focused_tab = 1;
                    app::session::open(target_id.clone(), zeroize::Zeroizing::new("2468".into()));
                    app::split_intent::arm_fresh_for_test_generation(&target_id, 2);
                    drop(gui.update(Message::Pane(
                        pane_id,
                        pane::Message::tab(
                            2,
                            tab::Message::Launch(home::Message::Run(
                                root.clone(),
                                app::Config::new(false),
                                ChainId::BitcoinBlake2b,
                                reopened_cube,
                            )),
                        ),
                    )));
                    let tab::State::App(resumed) = &gui.panes.get(pane_id).unwrap().tabs[0].state
                    else {
                        panic!("already-open target must keep its App")
                    };
                    assert_eq!(resumed.cache().app_generation, owner);
                    assert_eq!(resumed.reviewer_pending_split_generation(), Some(2));
                    drop(gui);
                    app::session::close();
                    std::fs::remove_dir_all(path).ok();
                }
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn current_remote_duress_closes_the_retained_home_but_stale_duress_cannot_close_a_new_owner() {
        let result = std::thread::Builder::new()
            .name("remote-duress-owner-barrier".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let _enter = runtime.enter();
                let _guard = app::session::test_guard();
                let path = std::env::temp_dir()
                    .join(format!("remote-duress-owner-{}", uuid::Uuid::new_v4()));
                let (mut source, _) = app::claim_step1_tests::bitcoin_app(&path);
                let stopped = app::tests::reviewer_attach_counted_claim_context(&mut source);
                let source_id = source.cube_settings().id.clone();
                let (home, startup) = home::Home::new_for_chain(source.datadir().clone(), None);
                drop(startup);
                let mut gui = GUI::reviewer_app_home_gui(source, home);
                let pane_id = gui.focus.unwrap();
                let old_generation = gui.panes.get(pane_id).unwrap().tabs[0]
                    .app_generation_token()
                    .unwrap();
                gui.panes.get_mut(pane_id).unwrap().focused_tab = 1;
                app::session::open(source_id, zeroize::Zeroizing::new("2468".into()));
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        1,
                        tab::Message::AppAsync(
                            old_generation,
                            Box::new(tab::Message::Run(AppMessage::View(
                                crate::app::view::Message::DuressLockRemote,
                            ))),
                        ),
                    ),
                )));
                let pane = gui.panes.get(pane_id).unwrap();
                assert_eq!(pane.tabs.len(), 1, "the retained Launcher is closed");
                assert!(matches!(pane.tabs[0].state, tab::State::DuressActive(_)));
                assert_eq!(pane.focused_tab, 0);
                assert!(!app::session::is_open());
                assert_eq!(stopped.load(std::sync::atomic::Ordering::SeqCst), 1);

                let new_path = std::env::temp_dir()
                    .join(format!("remote-duress-new-owner-{}", uuid::Uuid::new_v4()));
                let (new_owner, _) = app::claim_step1_tests::bitcoin_app(&new_path);
                let new_id = new_owner.cube_settings().id.clone();
                // Test-only replacement on the same tab identity models a
                // later admitted owner. The real App→Duress transition already
                // advanced Tab's generation before this stale result arrives.
                gui.panes.get_mut(pane_id).unwrap().tabs[0].state = tab::State::App(new_owner);
                assert_ne!(
                    gui.panes.get(pane_id).unwrap().tabs[0]
                        .app_generation_token()
                        .unwrap(),
                    old_generation
                );
                app::session::open(new_id.clone(), zeroize::Zeroizing::new("2468".into()));
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        1,
                        tab::Message::AppAsync(
                            old_generation,
                            Box::new(tab::Message::Run(AppMessage::View(
                                crate::app::view::Message::DuressLockRemote,
                            ))),
                        ),
                    ),
                )));
                assert!(matches!(
                    gui.panes.get(pane_id).unwrap().tabs[0].state,
                    tab::State::App(_)
                ));
                assert_eq!(app::session::current_cube_id(), Some(new_id));
                assert_eq!(stopped.load(std::sync::atomic::Ordering::SeqCst), 1);
                drop(gui);
                app::session::close();
                std::fs::remove_dir_all(path).ok();
                std::fs::remove_dir_all(new_path).ok();
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn local_duress_completion_uses_the_same_public_owner_barrier() {
        let result = std::thread::Builder::new()
            .name("local-duress-owner-barrier".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let _enter = runtime.enter();
                let _guard = app::session::test_guard();
                let path = std::env::temp_dir()
                    .join(format!("local-duress-owner-{}", uuid::Uuid::new_v4()));
                let (mut source, _) = app::claim_step1_tests::bitcoin_app(&path);
                let stopped = app::tests::reviewer_attach_counted_claim_context(&mut source);
                let source_id = source.cube_settings().id.clone();
                let root = source.datadir().clone();
                let (home, startup) = home::Home::new_for_chain(root.clone(), None);
                drop(startup);
                let mut gui = GUI::reviewer_app_home_gui(source, home);
                let pane_id = gui.focus.unwrap();
                gui.panes.get_mut(pane_id).unwrap().focused_tab = 1;
                app::session::open(source_id, zeroize::Zeroizing::new("2468".into()));
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        1,
                        tab::Message::DuressActivated {
                            datadir: root,
                            network: bitcoin::Network::Bitcoin,
                            queue_pending: false,
                        },
                    ),
                )));
                let pane = gui.panes.get(pane_id).unwrap();
                assert_eq!(pane.tabs.len(), 1);
                assert_eq!(pane.focused_tab, 0);
                assert!(matches!(pane.tabs[0].state, tab::State::DuressActive(_)));
                assert!(!app::session::is_open());
                assert_eq!(stopped.load(std::sync::atomic::Ordering::SeqCst), 1);
                drop(gui);
                std::fs::remove_dir_all(path).ok();
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn launcher_resumes_one_app_and_switch_closes_it_before_unlock() {
        let result = std::thread::Builder::new()
            .name("single-open-lifecycle".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let _enter = runtime.enter();
                let _guard = app::session::test_guard();
                let (source, handoff, path) =
                    app::claim_step1_tests::returning_claim_fixture(false);
                let root = source.datadir().clone();
                let source_settings = source.cube_settings().clone();
                let source_id = source_settings.id.clone();
                let generation = source.cache().app_generation;
                let (home, startup) = home::Home::new_for_chain(root.clone(), None);
                drop(startup);
                let mut gui = GUI::reviewer_app_home_gui(source, home);
                let pane_id = gui.focus.unwrap();
                app::session::open(
                    source_id.clone(),
                    zeroize::Zeroizing::new("2468".into()),
                );
                let app_count = |gui: &GUI| {
                    gui.panes
                        .iter()
                        .flat_map(|(_, pane)| pane.tabs.iter())
                        .filter(|tab| matches!(tab.state, tab::State::App(_)))
                        .count()
                };
                assert_eq!(app_count(&gui), 1);

                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::View(pane::ViewMessage::OpenLauncher),
                )));
                assert_eq!(app_count(&gui), 1);
                assert!(matches!(
                    gui.panes.get(pane_id).unwrap().tabs[0].state,
                    tab::State::App(_)
                ));
                assert_eq!(gui.panes.get(pane_id).unwrap().focused_tab, 1);

                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        2,
                        tab::Message::Launch(home::Message::Run(
                            root.clone(),
                            app::Config::new(false),
                            ChainId::Bitcoin,
                            source_settings.clone(),
                        )),
                    ),
                )));
                assert_eq!(app_count(&gui), 1);
                assert_eq!(gui.panes.get(pane_id).unwrap().focused_tab, 0);
                let tab::State::App(resumed) = &gui.panes.get(pane_id).unwrap().tabs[0].state
                else {
                    panic!("same Cube must resume the retained App");
                };
                assert_eq!(resumed.cache().app_generation, generation);

                // A typed Return route must also resume the retained source
                // panel, rather than treating it as an ordinary focus-only
                // reopen and leaving its Claim intent armed.
                app::claim_intent::arm(source_id.clone());
                let intent = home::ClaimOpenIntent::new(
                    generation,
                    &root,
                    source_id.clone(),
                    handoff.fork_cube().to_owned(),
                    home::ClaimRoute::Return(handoff),
                );
                let return_task = gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        2,
                        tab::Message::Launch(home::Message::RunClaim(
                            root.clone(),
                            app::Config::new(false),
                            ChainId::Bitcoin,
                            source_settings.clone(),
                            intent,
                        )),
                    ),
                ));
                use iced_runtime::futures::futures::StreamExt;
                let returned = runtime.block_on(async {
                    let mut stream = iced_runtime::task::into_stream(return_task)
                        .expect("typed Return emits its App route");
                    while let Some(action) = stream.next().await {
                        if let iced_runtime::Action::Output(message) = action {
                            if matches!(&message, Message::Pane(_, pane::Message::Tab(_, inner))
                                if matches!(GUI::reviewer_unwrap_guarded_message(inner),
                                    tab::Message::Run(AppMessage::View(
                                        crate::app::view::Message::Menu(
                                            crate::app::menu::Menu::Vault(
                                                crate::app::menu::VaultSubMenu::Claim))))))
                            {
                                return message;
                            }
                        }
                    }
                    panic!("typed Return did not reach the Claim panel")
                });
                drop(gui.update(returned));
                assert!(!app::claim_intent::take(&source_id));
                assert_eq!(app_count(&gui), 1);
                assert_eq!(
                    gui.reviewer_app_mut(0).current_menu_for_test(),
                    &crate::app::menu::Menu::Vault(crate::app::menu::VaultSubMenu::Claim)
                );

                // The Account Recovery action reuses this exact App without
                // another unlock and dispatches the existing management route.
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::View(pane::ViewMessage::OpenLauncher),
                )));
                let route = gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        2,
                        tab::Message::Launch(home::Message::RunToRecovery(
                            root.clone(),
                            app::Config::new(false),
                            ChainId::Bitcoin,
                            source_settings,
                        )),
                    ),
                ));
                let recovery_message = runtime.block_on(async {
                    tokio::time::timeout(std::time::Duration::from_secs(2), async {
                        let mut stream = iced_runtime::task::into_stream(route)
                            .expect("Recovery route emits a menu event");
                        while let Some(action) = stream.next().await {
                            if let iced_runtime::Action::Output(message) = action {
                                if matches!(
                                    &message,
                                    Message::Pane(_, pane::Message::Tab(_, inner))
                                        if matches!(GUI::reviewer_unwrap_guarded_message(inner), tab::Message::Run(
                                            AppMessage::View(crate::app::view::Message::Menu(
                                                crate::app::menu::Menu::Cube(
                                                    crate::app::menu::CubeSubMenu::Settings(
                                                        crate::app::menu::CubeSettingsOption::Recovery
                                                    )
                                                )
                                            ))
                                        ))
                                ) {
                                    return Some(message);
                                }
                            }
                        }
                        None
                    })
                    .await
                    .expect("Recovery menu route completed")
                    .expect("Recovery menu event")
                });
                drop(gui.update(recovery_message));
                assert_eq!(app_count(&gui), 1);

                let other = app::settings::CubeSettings::new_with_raw_id(
                    "other".into(),
                    "Other".into(),
                    ChainId::Bitcoin,
                );
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        2,
                        tab::Message::Launch(home::Message::Run(
                            root,
                            app::Config::new(false),
                            ChainId::Bitcoin,
                            other,
                        )),
                    ),
                )));
                assert_eq!(app_count(&gui), 0, "old App must stop before B unlocks");
                drop(gui);
                std::fs::remove_dir_all(path).unwrap();
            })
            .unwrap()
            .join();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
