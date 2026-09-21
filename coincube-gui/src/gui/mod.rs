use iced::{
    event::{self, Event},
    keyboard,
    widget::{
        operation::{focus_next, focus_previous},
        pane_grid,
    },
    Length, Size, Subscription, Task,
};
use iced_runtime::window;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tracing::{error, info};
use tracing_subscriber::filter::LevelFilter;
extern crate serde;
extern crate serde_json;

use coincube_core::miniscript::bitcoin;
use coincube_ui::widget::{Column, Container, Element};

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
            Message::Pane(
                pane_id,
                pane::Message::Tab(tab_id, tab::Message::ForkAsync(generation, message)),
            ) => {
                let current = self
                    .panes
                    .get(pane_id)
                    .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == tab_id))
                    .is_some_and(|tab| tab.accepts_fork_generation(generation));
                if !current {
                    return Task::none();
                }
                Message::Pane(pane_id, pane::Message::Tab(tab_id, *message))
            }
            message => message,
        };
        let auth_change = match &message {
            Message::Pane(
                _,
                pane::Message::Tab(
                    _,
                    tab::Message::Launch(home::Message::View(home::ViewMessage::ConnectAccount(
                        msg,
                    ))),
                ),
            )
            | Message::Pane(
                _,
                pane::Message::Tab(
                    _,
                    tab::Message::Run(AppMessage::View(crate::app::view::Message::ConnectAccount(
                        msg,
                    ))),
                ),
            ) => matches!(
                msg,
                crate::app::view::ConnectAccountMessage::LogOut
                    | crate::app::view::ConnectAccountMessage::SetSession(_)
            ),
            _ => false,
        };
        let auth_from_fork_app = if auth_change {
            match &message {
                Message::Pane(pane_id, pane::Message::Tab(tab_id, _)) => self.panes.get(*pane_id)
                    .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == *tab_id))
                    .is_some_and(|tab| matches!(&tab.state, tab::State::App(app) if app.cube_settings().network.is_blake2b())),
                _ => false,
            }
        } else {
            false
        };
        let mut auth_tasks = Vec::new();
        if auth_change {
            for (&pane_id, pane) in self.panes.iter_mut() {
                for tab in &mut pane.tabs {
                    let tab_id = tab.id;
                    auth_tasks.push(
                        tab.invalidate_fork_session().map(move |msg| {
                            Message::Pane(pane_id, pane::Message::Tab(tab_id, msg))
                        }),
                    );
                }
            }
        }
        // The old fork App is gone. Apply its auth operation to the replacement
        // Home so logout clears saved login state instead of being discarded.
        let message = if auth_from_fork_app {
            match message {
                Message::Pane(
                    pane_id,
                    pane::Message::Tab(
                        tab_id,
                        tab::Message::Run(AppMessage::View(
                            crate::app::view::Message::ConnectAccount(msg),
                        )),
                    ),
                ) => Message::Pane(
                    pane_id,
                    pane::Message::Tab(
                        tab_id,
                        tab::Message::Launch(home::Message::View(
                            home::ViewMessage::ConnectAccount(msg),
                        )),
                    ),
                ),
                message => message,
            }
        } else {
            message
        };
        // Dispatch may return early (notably for in-App account messages).
        // Keep those returns inside the helper so invalidation tasks always run.
        auth_tasks.push(self.update_message(message));
        Task::batch(auth_tasks)
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
            Message::Pane(_, pane::Message::View(pane::ViewMessage::ToggleTheme)) => {
                self.update(Message::ToggleTheme)
            }
            Message::Pane(pane_id, pane::Message::View(pane::ViewMessage::SplitTab(i))) => {
                if let Some(p) = self.panes.get_mut(pane_id) {
                    if let Some(tab) = p.remove_tab(i) {
                        let mut new_pane = pane::Pane::new_with_tab(tab.state);
                        new_pane.set_theme_mode(self.theme_mode);
                        let result = self
                            .panes
                            .split(pane_grid::Axis::Vertical, pane_id, new_pane);

                        if let Some((pane, _)) = result {
                            self.focus = Some(pane);
                        }
                    }
                }
                Task::none()
            }
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
            Message::Pane(p, pane::Message::Tab(t, tab::Message::Launch(msg))) => {
                let mut tasks = Vec::new();
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
                                        pane::Message::Tab(tab_id, tab::Message::Launch(msg)),
                                    )
                                }));
                            }
                        }
                    }
                }

                if let Some(pane) = self.panes.get_mut(p) {
                    tasks.push(
                        pane.update(
                            pane::Message::Tab(t, tab::Message::Launch(msg)),
                            &self.config,
                        )
                        .map(move |msg| Message::Pane(p, msg)),
                    );
                }

                Task::batch(tasks)
            }
            Message::Pane(i, msg) => {
                match msg {
                    // Handle ListCurrencies requests separately.
                    pane::Message::Tab(tab_id, tab::Message::Run(inner))
                        if matches!(inner, AppMessage::Fiat(AppFiatMessage::ListCurrencies(_))) =>
                    {
                        let AppMessage::Fiat(AppFiatMessage::ListCurrencies(source)) = inner else {
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
            Message::Resized(pane_grid::ResizeEvent { split, ratio }) => {
                self.panes.resize(split, ratio);
                Task::none()
            }
            Message::Dragged(pane_grid::DragEvent::Dropped { pane, target }) => {
                if let pane_grid::Target::Pane(p, pane_grid::Region::Center) = target {
                    let (tabs, focused_tab) = if let Some(origin) = self.panes.get_mut(pane) {
                        (std::mem::take(&mut origin.tabs), origin.focused_tab)
                    } else {
                        (Vec::new(), 0)
                    };

                    if let Some(dest) = self.panes.get_mut(p) {
                        if !tabs.is_empty() {
                            dest.add_tabs(tabs, focused_tab);
                        }
                    }
                    self.panes.close(pane);
                    self.focus = Some(p);
                } else {
                    self.panes.drop(pane, target);
                }
                Task::none()
            }
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
        if self.panes.len() == 1 {
            if let Some((&id, pane)) = self.panes.iter().nth(0) {
                return Column::new()
                    .push(pane.tabs_menu_view().map(move |msg| Message::Pane(id, msg)))
                    .push(pane.view().map(move |msg| Message::Pane(id, msg)))
                    .into();
            }
        }

        let focus = self.focus;
        let pane_grid = pane_grid::PaneGrid::new(&self.panes, |id, pane, _| {
            let _is_focused = focus == Some(id);

            pane_grid::Content::new(pane.view().map(move |msg| Message::Pane(id, msg))).title_bar(
                pane_grid::TitleBar::new(
                    pane.tabs_menu_view().map(move |msg| Message::Pane(id, msg)),
                ),
            )
        })
        .spacing(10)
        .width(Length::Fill)
        .height(Length::Fill)
        .on_click(Message::Clicked)
        .on_drag(Message::Dragged)
        .on_resize(10, Message::Resized);

        Container::new(pane_grid)
            .style(coincube_ui::theme::pane_grid::pane_grid_background)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
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
        );
        installer.context.bitcoin_config.chain = ChainId::BitcoinBlake2b;
        tab::State::Installer(installer)
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
            ConnectAccountMessage::SetSession(LoginResponse {
                requires_2fa: false,
                token: "synthetic".into(),
                refresh_token: "synthetic".into(),
                user: User {
                    id: 0,
                    email: "synthetic@example.invalid".into(),
                    email_verified: None,
                },
            }),
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
                pane::Message::Tab(
                    1,
                    tab::Message::Run(AppMessage::View(crate::app::view::Message::ConnectAccount(
                        auth,
                    ))),
                ),
            ));
            let mut checked = HashSet::new();
            if let Some(mut stream) = iced_runtime::task::into_stream(task) {
                while let Some(action) = stream.next().await {
                    if let iced_runtime::Action::Output(
                        message @ Message::Pane(
                            _,
                            pane::Message::Tab(
                                _,
                                tab::Message::Launch(home::Message::Checked { .. }),
                            ),
                        ),
                    ) = action
                    {
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
                    pane::Message::Tab(1, tab::Message::Launch(_))))) if id == source));
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
                ConnectAccountMessage::SetSession(serde_json::from_value(serde_json::json!({
                    "requires_2fa":false,"token":"replacement","refresh_token":"replacement-refresh",
                    "user":{"id":8,"email":"fixture@example.invalid","email_verified":true}
                })).unwrap())
            } else {
                ConnectAccountMessage::LogOut
            };
            // Actual GUI auth broadcaster, with the signing picker still open.
            drop(gui.update(Message::Pane(
                pane_id,
                pane::Message::Tab(
                    1,
                    tab::Message::Run(AppMessage::View(view::Message::ConnectAccount(auth))),
                ),
            )));
            assert!(outputs(pending).await.is_empty());
            let tab = &mut gui.panes.get_mut(pane_id).unwrap().tabs[0];
            assert!(matches!(tab.state, tab::State::Home(_)));
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
