use iced::{
    event::{self, Event},
    keyboard,
    widget::{
        operation::{focus_next, focus_previous},
        pane_grid,
    },
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
use coincube_ui::widget::Element;

mod cache;
pub mod pane;
pub mod tab;

use crate::{
    app::{
        cache::{FiatPrice, FiatPriceRequest},
        message::{FiatMessage as AppFiatMessage, Message as AppMessage},
        settings::global::{GlobalSettings, WindowConfig},
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
}

#[derive(Debug)]
pub enum Key {
    Tab(bool),
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
        tab::Message::GlobalSettingsChanged => pane::ViewMessage::GlobalSettingsChanged,
        tab::Message::ConnectSignedIn => pane::ViewMessage::ConnectSignedIn,
        tab::Message::OpenBitcoinClaim(handoff) => pane::ViewMessage::OpenBitcoinClaim(handoff),
        tab::Message::OpenForkClaim(handoff) => pane::ViewMessage::OpenForkClaim(handoff),
        message => return Message::Pane(pane_id, pane::Message::tab(tab_id, message)),
    };
    Message::Pane(pane_id, pane::Message::View(view))
}

impl GUI {
    pub fn title(&self) -> String {
        match cfg!(debug_assertions) {
            true => format!("Tenshu v{} (development)", env!("CARGO_PKG_VERSION")),
            false => format!("Tenshu v{}", env!("CARGO_PKG_VERSION")),
        }
    }

    pub fn new(config: Config, log_level: Option<LevelFilter>) -> (GUI, Task<Message>) {
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
        ];
        let (pane, cmd) = pane::Pane::new(&config);
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
            },
            Task::batch(cmds),
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
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
                    return Task::none();
                }
                route_guarded_tab_result(pane_id, tab_id, *message)
            }
            Message::Pane(pane_id, pane::Message::Tab(tab_id, _))
                if !self
                    .panes
                    .get(pane_id)
                    .is_some_and(|pane| pane.tabs.iter().any(|tab| tab.id == tab_id)) =>
            {
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
                        tab::State::App(app) if app.can_adopt_shared_connect_session() => Some(
                            app.adopt_shared_connect_client(client)
                                .map(tab::Message::Run),
                        ),
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
                log::debug!("Tab pressed!");
                if shift {
                    focus_previous()
                } else {
                    focus_next()
                }
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
                            Some((pane_id, tab.id, tab.app_generation_token()?))
                        }
                        _ => None,
                    })
                });
                if let Some((pane_id, tab_id, generation)) = retained {
                    let task = self
                        .panes
                        .get_mut(pane_id)
                        .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == tab_id))
                        .and_then(|tab| match &mut tab.state {
                            tab::State::App(app) => Some(app.begin_claim_pair_phase(handoff)),
                            _ => None,
                        })
                        .unwrap_or_else(|| Err("The Claim source Cube closed.".into()))
                        .unwrap_or_else(|error| {
                            Task::done(AppMessage::View(crate::app::view::Message::ShowError(
                                error,
                            )))
                        });
                    self.focus = Some(pane_id);
                    return task.map(move |message| {
                        Message::Pane(
                            pane_id,
                            pane::Message::tab(
                                tab_id,
                                tab::Message::AppAsync(
                                    generation,
                                    Box::new(tab::Message::Run(message)),
                                ),
                            ),
                        )
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
                            pane.focused_tab = index;
                            self.focus = Some(pane_id);
                            return task.map(move |message| {
                                Message::Pane(
                                    pane_id,
                                    pane::Message::tab(tab_id, tab::Message::Run(message)),
                                )
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
            // A Home sign-in re-checks the Connect session of every open Cube,
            // in every pane: a split-off Cube is still an open Cube. Each Init
            // re-enters `update`, so it may adopt the verified sibling client.
            Message::Pane(_, pane::Message::View(pane::ViewMessage::ConnectSignedIn)) => {
                let mut tasks = Vec::new();
                for (&pane_id, pane) in self.panes.iter() {
                    for tab in &pane.tabs {
                        if matches!(&tab.state, tab::State::App(_)) {
                            tasks.push(Task::done(Message::Pane(
                                pane_id,
                                pane::Message::tab(
                                    tab.id,
                                    tab::Message::Run(AppMessage::View(
                                        crate::app::view::Message::ConnectAccount(
                                            crate::app::view::ConnectAccountMessage::Init,
                                        ),
                                    )),
                                ),
                            )));
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
                if matches!(inner.as_ref(), tab::Message::LockCubeToLauncher)
                    || self.panes.get(pane_id).is_some_and(|pane| {
                        let Some(source) = pane.tabs.iter().find(|tab| tab.id == tab_id) else {
                            return false;
                        };
                        let valid_lock = match inner.as_ref() {
                            tab::Message::LockCube => true,
                            tab::Message::AppAsync(generation, nested) => {
                                source.app_generation_token() == Some(*generation)
                                    && matches!(nested.as_ref(), tab::Message::LockCube)
                            }
                            _ => false,
                        };
                        valid_lock
                            && (matches!(
                                pane.tabs.get(pane.focused_tab).map(|tab| &tab.state),
                                Some(tab::State::Home(_))
                            ) || match &source.state {
                                tab::State::App(app) => pane.tabs.iter().any(|tab| {
                                    tab.claim_open_owner() == Some(app.cache().app_generation)
                                }),
                                _ => false,
                            })
                    }) =>
            {
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
                    for index in (0..pane.tabs.len()).rev() {
                        let sibling = &pane.tabs[index];
                        if other_id == pane_id && sibling.id == tab_id {
                            continue;
                        }
                        let same_cube = matches!(&sibling.state, tab::State::App(app)
                            if crate::chain::same_path(root.path(), app.datadir().path())
                                && app.cube_settings().network == chain
                                && app.cube_settings().id == cube_id);
                        if sibling.claim_open_owner() == Some(owner) || same_cube {
                            pane.close_tab(index);
                        }
                    }
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
                            pane.close_tab(index); // waits for daemon stop
                            crate::app::session::close(); // zeroizes signer/PIN
                            self.focus = Some(pane_id);
                            return pane
                                .focus_launcher(&self.config)
                                .map(move |message| Message::Pane(pane_id, message));
                        }
                    }
                }
                Task::none()
            }
            Message::Pane(pane_id, pane::Message::Tab(tab_id, inner))
                if matches!(inner.as_ref(), tab::Message::DuressActivated { .. }) =>
            {
                let result = {
                    let Some(pane) = self.panes.get_mut(pane_id) else {
                        return Task::none();
                    };
                    let Some(index) = pane.tabs.iter().position(|tab| tab.id == tab_id) else {
                        return Task::none();
                    };
                    pane.tabs[index].stop();
                    pane.update(pane::Message::Tab(tab_id, inner), &self.config)
                };
                // The wipe is complete: do not leave a Launcher or another
                // Cube surface visible above the cryptic duress state.
                for (&other_id, other_pane) in self.panes.iter_mut() {
                    for index in (0..other_pane.tabs.len()).rev() {
                        if other_id != pane_id || other_pane.tabs[index].id != tab_id {
                            other_pane.close_tab(index);
                        }
                    }
                }
                crate::app::session::close();
                if let Some(pane) = self.panes.get_mut(pane_id) {
                    pane.focused_tab = pane
                        .tabs
                        .iter()
                        .position(|tab| tab.id == tab_id)
                        .unwrap_or(0);
                }
                self.focus = Some(pane_id);
                return result.map(move |message| Message::Pane(pane_id, message));
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
                if let Some((root, chain, cube, intent)) = match &msg {
                    home::Message::Run(root, _, chain, cube) => Some((root, chain, cube, None)),
                    home::Message::RunClaim(root, _, chain, cube, intent) => {
                        Some((root, chain, cube, Some(intent)))
                    }
                    _ => None,
                } {
                    let claim_route = intent.is_some_and(|intent| {
                        self.panes.iter().any(|(_, pane)| {
                            pane.tabs.iter().any(|tab| match &tab.state {
                                tab::State::App(app) => intent.validates(app, root, *chain, cube),
                                _ => false,
                            })
                        })
                    });
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
                    }
                    // Opening the same Cube from the Launcher resumes its live
                    // App. In particular this must not ask for its PIN again.
                    let existing = self.panes.iter().find_map(|(&pane_id, pane)| {
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
                    });
                    if let Some((pane_id, index)) = existing {
                        self.focus = Some(pane_id);
                        if let Some(pane) = self.panes.get_mut(pane_id) {
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
                                    pane.focused_tab = index;
                                    return task.map(move |message| {
                                        Message::Pane(
                                            pane_id,
                                            pane::Message::tab(tab_id, tab::Message::Run(message)),
                                        )
                                    });
                                }
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
                    if !claim_route {
                        let mut closed = false;
                        for (_, pane) in self.panes.iter_mut() {
                            for index in (0..pane.tabs.len()).rev() {
                                if matches!(
                                    pane.tabs[index].state,
                                    tab::State::App(_)
                                        | tab::State::Loader(_)
                                        | tab::State::Installer(_)
                                        | tab::State::Login(_)
                                        | tab::State::PinEntry(_)
                                        | tab::State::PasskeyUnlock(_)
                                ) {
                                    pane.close_tab(index);
                                    closed = true;
                                }
                            }
                        }
                        if closed {
                            crate::app::session::close();
                        }
                    }
                }
                if let home::Message::View(home::ViewMessage::DeleteCube(
                    home::DeleteCubeMessage::Confirm(..),
                )) = &msg
                {
                    // When a cube is deleted, close all App and Loader tabs since they won't be valid anymore
                    let mut panes_to_close = Vec::<pane_grid::Pane>::new();
                    for (id, pane) in self.panes.iter_mut() {
                        // Stop and remove tabs - iterate in reverse to maintain valid indices
                        let mut i = pane.tabs.len();
                        while i > 0 {
                            i -= 1;
                            if matches!(
                                pane.tabs[i].state,
                                tab::State::App(_) | tab::State::Loader(_)
                            ) {
                                pane.close_tab(i);
                            }
                        }
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
                            Some((pane_id, tab.id, tab.app_generation_token()?))
                        }
                        _ => None,
                    })
                });
                let Some((source_pane, source_tab, source_generation)) =
                    source.filter(|_| loader_present)
                else {
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
                    return Task::done(Message::Pane(
                        source_pane,
                        pane::Message::tab(
                            source_tab,
                            tab::Message::AppAsync(
                                source_generation,
                                Box::new(tab::Message::Run(AppMessage::View(
                                    crate::app::view::Message::ShowError(error),
                                ))),
                            ),
                        ),
                    ));
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
                    Ok(task) => task.map(move |message| {
                        Message::Pane(
                            source_pane,
                            pane::Message::tab(
                                source_tab,
                                tab::Message::AppAsync(
                                    source_generation,
                                    Box::new(tab::Message::Run(message)),
                                ),
                            ),
                        )
                    }),
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
            Message::ToggleTheme => {
                use coincube_ui::theme::palette::ThemeMode;
                self.theme_mode = match self.theme_mode {
                    ThemeMode::Dark => ThemeMode::Light,
                    ThemeMode::Light => ThemeMode::Dark,
                };
                // Propagate to all pane tabs' caches
                for (_, pane) in self.panes.iter_mut() {
                    pane.set_theme_mode(self.theme_mode);
                }
                // Persist preference
                let path = GlobalSettings::path(&self.config.coincube_directory);
                if let Err(e) = GlobalSettings::update_theme_mode(&path, self.theme_mode) {
                    tracing::error!("Failed to persist theme mode: {e}");
                }
                Task::none()
            }
            Message::Tick => {
                let mut tasks = vec![];

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
                match (&event, status) {
                    (
                        Event::Keyboard(keyboard::Event::KeyPressed {
                            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab),
                            modifiers,
                            ..
                        }),
                        event::Status::Ignored,
                    ) => Some(Message::KeyPressed(Key::Tab(modifiers.shift()))),
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
        for (id, pane) in self.panes.iter() {
            vec.push(
                pane.subscription()
                    .with(*id)
                    .map(|(id, msg)| Message::Pane(id, msg)),
            );
        }
        Subscription::batch(vec)
    }

    pub fn view(&self) -> Element<Message> {
        let (&id, pane) = self
            .panes
            .iter()
            .find(|(id, _)| Some(**id) == self.focus)
            .or_else(|| self.panes.iter().next())
            .expect("GUI always owns its primary pane");
        pane.view().map(move |message| Message::Pane(id, message))
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
mod lock_cube_dispatch_tests {
    use super::*;

    #[test]
    fn lock_closes_same_cube_across_panes_but_preserves_an_unrelated_cube() {
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
        let sibling = crate::app::claim_step1_tests::bitcoin_app_with_id(&root_path, &same_cube_id);
        let same_pane_sibling =
            crate::app::claim_step1_tests::bitcoin_app_with_id(&root_path, &same_cube_id);
        let (unrelated, _) = crate::app::claim_step1_tests::bitcoin_app(&root_path);
        let unrelated_id = unrelated.cube_settings().id.clone();
        let mut first_pane = pane::Pane::new_with_tab(tab::State::App(first));
        first_pane
            .tabs
            .push(tab::Tab::new(2, tab::State::App(unrelated)));
        first_pane
            .tabs
            .push(tab::Tab::new(3, tab::State::App(same_pane_sibling)));
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
        };

        drop(gui.update(Message::Pane(
            origin,
            pane::Message::tab(1, tab::Message::LockCubeToLauncher),
        )));
        let pane = gui.panes.get(origin).expect("origin remains on Launcher");
        assert!(matches!(pane.tabs[0].state, tab::State::Home(_)));
        assert_eq!(
            pane.tabs.len(),
            2,
            "same-pane sibling was stopped and removed"
        );
        assert!(matches!(&pane.tabs[1].state, tab::State::App(app)
            if app.cube_settings().id == unrelated_id));
        assert!(gui.panes.get(other).is_none(), "empty sibling pane closed");
        assert_eq!(gui.focus, Some(origin));

        drop(gui);
        let _ = std::fs::remove_dir_all(root_path);
    }
}

// Gandalf's round-3 reviewer seam (#518 issuecomment-5824706295), adopted:
// a real GUI with two Bitcoin App tabs, its real auth broadcaster and
// originating-tab dispatch. Test-only.
#[cfg(test)]
impl GUI {
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
                inner.as_ref(),
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
            if matches!(inner.as_ref(), tab::Message::Run(AppMessage::View(
                crate::app::view::Message::ConnectAccount(_)))))
    }
    /// Whether a GUI message is a tab's `SetSession` completion — what a
    /// real refresh or login operation hands back — so a test can route the
    /// real completion through the real broadcaster.
    pub(crate) fn reviewer_is_set_session(message: &Message) -> bool {
        matches!(message, Message::Pane(_, pane::Message::Tab(_, inner))
            if matches!(inner.as_ref(), tab::Message::Run(AppMessage::View(
                crate::app::view::Message::ConnectAccount(
                    crate::app::view::ConnectAccountMessage::SetSession(..))))))
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
                };
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
                                let tab::Message::Run(message) = *inner else {
                                    panic!("unexpected tab route")
                                };
                                assert_eq!(
                                    (pane_id, id),
                                    (target, tab_id),
                                    "route back to the selected owner"
                                );
                                if wrong_wallet {
                                    assert!(
                                        matches!(message, AppMessage::View(view::Message::ShowError(ref error))
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
                drop(gui.update(Message::Pane(
                    pane_id,
                    pane::Message::tab(
                        2,
                        tab::Message::Launch(home::Message::RunClaim(
                            root.clone(),
                            app::Config::new(false),
                            ChainId::Bitcoin,
                            source_settings,
                            intent,
                        )),
                    ),
                )));
                assert!(!app::claim_intent::take(&source_id));
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
