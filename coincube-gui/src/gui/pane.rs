use coincube_ui::{
    color,
    component::text::*,
    icon::{cross_icon, plus_icon},
    theme,
    widget::*,
};
use iced::{Length, Subscription, Task};
use iced_aw::ContextMenu;

use crate::{app, gui::Config};

use super::tab;

#[derive(Debug)]
pub enum Message {
    Tab(usize, Box<tab::Message>),
    View(ViewMessage),
    StartClaimInstaller(usize),
    ClaimInstallCancelled(usize),
    ImportedPairReady {
        source_tab: usize,
        request: crate::imported_claim::ImportedClaimRequest,
        result: Result<(), String>,
    },
}

impl Message {
    pub fn tab(id: usize, message: tab::Message) -> Self {
        Self::Tab(id, Box::new(message))
    }
}

#[derive(Debug, Clone)]
pub enum ViewMessage {
    OpenForkPair(app::claim_intent::ForkHandoff),
    OpenForkClaim(app::claim_intent::ForkHandoff),
    OpenBitcoinClaim(app::claim_intent::ForkHandoff),
    FocusTab(usize),
    CloseTab(usize),
    SplitTab(usize),
    AddTab,
    ToggleTheme,
    GlobalSettingsChanged,
    /// Bubbled from a Cube tab whose Connect-requiring feature page
    /// emitted [`crate::app::view::Message::OpenConnectSignIn`]. The
    /// handler focuses an existing Home tab and routes it to its
    /// Connect overview so the user can sign in.
    OpenConnectSignIn,
    /// Return to the Launcher while the originating Cube tab stays open.
    OpenLauncher,
    /// Bubbled from a tab whose paid-feature locked card emitted
    /// [`crate::app::view::Message::OpenPlanBilling`]. The handler focuses an
    /// existing Home tab and routes it to Connect → Plan & Billing.
    OpenPlanBilling,
    /// Bubbled from the Home tab on its auth-success edge. `GUI::update`
    /// broadcasts a `ConnectAccountMessage::Init` to every open Cube tab, in
    /// every pane, so those panels re-check the session and swap out of
    /// their "Sign in" prompts without waiting for the user to interact.
    ConnectSignedIn,
}

pub struct Pane {
    pub tabs: Vec<tab::Tab>,

    // this is an index in the tabs array
    pub focused_tab: usize,

    // used to generate tabs ids.
    tabs_created: usize,

    // current theme mode — applied to new tabs on creation
    theme_mode: coincube_ui::theme::palette::ThemeMode,
}

impl Pane {
    fn show_app_error(&self, source_tab: usize, error: String) -> Task<Message> {
        self.tabs
            .iter()
            .find(|tab| tab.id == source_tab)
            .map_or_else(Task::none, |tab| {
                tab.emit_app_task(Task::done(app::Message::View(
                    app::view::Message::ShowError(error),
                )))
                .map(move |message| Message::tab(source_tab, message))
            })
    }

    fn open_claim_installer(&mut self, source_tab: usize) -> Task<Message> {
        let owner = self
            .tabs
            .iter()
            .find(|tab| tab.id == source_tab)
            .and_then(|tab| {
                let tab::State::App(app) = &tab.state else {
                    return None;
                };
                Some(app.cache().app_generation)
            });
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.claim_open_owner() == owner && owner.is_some())
        {
            self.focused_tab = index;
            return Task::none();
        }
        let prepared = self
            .tabs
            .iter()
            .find(|tab| tab.id == source_tab)
            .and_then(|tab| {
                let tab::State::App(app) = &tab.state else {
                    return None;
                };
                Some((|| {
                    let cube = app.cube_settings().clone();
                    let wallet = app
                        .wallet()
                        .ok_or_else(|| "This Cube has no Vault to claim.".to_string())?;
                    let descriptor = wallet.inheritance_descriptor().ok_or_else(|| {
                        "This imported Bitcoin wallet does not have a Vault claim policy."
                            .to_string()
                    })?;
                    let fingerprint = cube.master_signer_fingerprint.ok_or_else(|| {
                        "This Cube's master key isn't available in this session.".to_string()
                    })?;
                    let signer = app::session::unlocked_signer(&cube.id, fingerprint)
                        .ok_or_else(|| "Unlock this Cube again to start a claim.".to_string())?;
                    let source = crate::installer::ClaimSource {
                        cube: cube.clone(),
                        descriptor: descriptor.clone(),
                        signer: std::sync::Arc::new(crate::signer::Signer::new(signer)),
                    };
                    let root = app.datadir().clone();
                    let (installer, task) = crate::installer::Installer::try_new_for_chain(
                        root.clone(),
                        crate::chain::ChainId::BitcoinBlake2b,
                        None,
                        crate::installer::UserFlow::ClaimBlake2b {
                            from_cube: Box::new(source),
                        },
                        true,
                        None,
                        app.breez_client(),
                        app.spark_backend(),
                        crate::app::settings::global::GlobalSettings::load_developer_mode(
                            &crate::app::settings::global::GlobalSettings::path(app.datadir()),
                        ),
                        app.authenticated_coincube_client(),
                    )
                    .map_err(|error| error.to_string())?;
                    let fork_cube = installer.context.cube_id.clone().ok_or_else(|| {
                        "The Claim installer did not reserve a destination Cube.".to_string()
                    })?;
                    let handoff = app::claim_intent::ForkHandoff::new(
                        &root,
                        cube.id.clone(),
                        fork_cube.clone(),
                    )?;
                    let intent = crate::home::ClaimOpenIntent::new(
                        app.cache().app_generation,
                        &root,
                        cube.id,
                        fork_cube,
                        crate::home::ClaimRoute::Begin(handoff),
                    );
                    Ok((installer, task, intent))
                })())
            });
        let Some(prepared) = prepared else {
            return Task::none();
        };
        let (installer, task, intent) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return self.show_app_error(source_tab, error),
        };
        self.tabs_created += 1;
        let id = self.tabs_created;
        let mut tab = tab::Tab::new(id, tab::State::Installer(Box::new(installer)));
        tab.set_claim_open(intent);
        tab.set_theme_mode(self.theme_mode);
        self.tabs.push(tab);
        self.focused_tab = self.tabs.len() - 1;
        task.map(move |message| Message::tab(id, tab::Message::Install(message)))
    }

    pub fn new(cfg: &Config) -> (Self, Task<Message>) {
        let (state, task) = tab::State::new(cfg.coincube_directory.clone(), cfg.network);
        (
            Self {
                tabs: vec![tab::Tab::new(1, state)],
                focused_tab: 0,
                tabs_created: 1,
                theme_mode: coincube_ui::theme::palette::ThemeMode::default(),
            },
            task.map(|msg| Message::tab(1, msg)),
        )
    }

    pub fn new_with_tab(s: tab::State) -> Self {
        Self {
            tabs: vec![tab::Tab::new(1, s)],
            focused_tab: 0,
            tabs_created: 1,
            theme_mode: coincube_ui::theme::palette::ThemeMode::default(),
        }
    }

    fn add_tab(&mut self, cfg: &Config) -> Task<Message> {
        let (state, task) = tab::State::new(cfg.coincube_directory.clone(), cfg.network);
        self.tabs_created += 1;
        let id = self.tabs_created;
        let mut tab = tab::Tab::new(id, state);
        tab.set_theme_mode(self.theme_mode);
        self.tabs.push(tab);
        self.focused_tab = self.tabs.len() - 1;
        task.map(move |msg| Message::tab(id, msg))
    }

    /// Focus an existing Home tab (or spawn one) and route it to `nav`. Shared
    /// by the OpenConnectSignIn / OpenPlanBilling handlers, which differ only in
    /// the Connect sub-section `nav` targets.
    fn focus_home_tab_with(&mut self, nav: crate::home::Message, cfg: &Config) -> Task<Message> {
        let claim_owner = match &nav {
            crate::home::Message::BeginClaimPair(handoff)
            | crate::home::Message::ContinueForkClaim(handoff)
            | crate::home::Message::ReturnBitcoinClaim(handoff) => {
                self.tabs.iter().find_map(|tab| {
                    let tab::State::App(app) = &tab.state else {
                        return None;
                    };
                    (handoff.matches_root(app.datadir())
                        && app.cube_settings().network == crate::chain::ChainId::Bitcoin
                        && app.cube_settings().id == handoff.bitcoin_cube())
                    .then_some(app.cache().app_generation)
                })
            }
            _ => None,
        };
        let home_tab = self
            .tabs
            .iter()
            .enumerate()
            .find(|(_, t)| matches!(&t.state, tab::State::Home(home) if !home.is_claim_route()))
            .map(|(i, t)| (i, t.id));
        if let Some((idx, tab_id)) = home_tab {
            self.focused_tab = idx;
            if let (Some(owner), tab::State::Home(home)) = (claim_owner, &mut self.tabs[idx].state)
            {
                home.set_claim_owner(owner);
            }
            return Task::done(Message::tab(tab_id, tab::Message::Launch(nav)));
        }
        // No Home tab open — spawn one and queue the navigation against its id
        // so it lands on the target Connect section rather than the default view.
        let add_task = self.add_tab(cfg);
        let new_tab_id = self.tabs.last().map(|t| t.id);
        if let Some(tab_id) = new_tab_id {
            if let (Some(owner), Some(tab::State::Home(home))) =
                (claim_owner, self.tabs.last_mut().map(|tab| &mut tab.state))
            {
                home.set_claim_owner(owner);
            }
            return Task::batch([
                add_task,
                Task::done(Message::tab(tab_id, tab::Message::Launch(nav))),
            ]);
        }
        add_task
    }

    pub(crate) fn focus_launcher(&mut self, cfg: &Config) -> Task<Message> {
        self.focus_home_tab_with(
            crate::home::Message::View(crate::home::ViewMessage::GoToSection(
                crate::home::HomeSection::Cubes,
            )),
            cfg,
        )
    }

    pub fn close_tab(&mut self, i: usize) {
        if let Some(mut tab) = self.remove_tab(i) {
            let owner = match &tab.state {
                tab::State::App(app) => Some(app.cache().app_generation),
                _ => None,
            };
            // Closing a tab mid-open abandons that open. The Split slot is
            // global, so fail closed and drop it rather than let it fire on
            // a later ordinary open.
            if tab.is_opening_cube() {
                crate::app::split_intent::clear();
            }
            tab.stop();
            if let Some(owner) = owner {
                self.close_tabs_matching(|tab| tab.claim_open_owner() == Some(owner));
            }
        }
    }

    /// `close_tab` may recursively remove an App's paired Claim tabs. Resolve
    /// each selected tab by stable ID after every close instead of retaining
    /// indices into a vector whose lower entries may have disappeared.
    pub(crate) fn close_tabs_matching(&mut self, predicate: impl Fn(&tab::Tab) -> bool) -> bool {
        let selected: Vec<usize> = self
            .tabs
            .iter()
            .filter(|tab| predicate(tab))
            .map(|tab| tab.id)
            .collect();
        let mut closed = false;
        for id in selected {
            if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                self.close_tab(index);
                closed = true;
            }
        }
        closed
    }

    pub fn remove_tab(&mut self, i: usize) -> Option<tab::Tab> {
        if i >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(i);

        if self.focused_tab >= i {
            self.focused_tab = self.focused_tab.saturating_sub(1);
        }

        Some(tab)
    }

    pub fn add_tabs(&mut self, tabs: Vec<tab::Tab>, focused_tab: usize) {
        for tab in tabs {
            self.tabs_created += 1;
            let id = self.tabs_created;
            let mut new_tab = tab::Tab::new(id, tab.state);
            new_tab.set_theme_mode(self.theme_mode);
            self.tabs.push(new_tab);
        }
        if self.focused_tab + focused_tab + 1 < self.tabs.len() {
            self.focused_tab += focused_tab + 1;
        }
    }

    pub fn set_theme_mode(&mut self, mode: coincube_ui::theme::palette::ThemeMode) {
        self.theme_mode = mode;
        for tab in &mut self.tabs {
            tab.set_theme_mode(mode);
        }
    }

    pub fn on_tick(&mut self) -> Task<Message> {
        Task::batch(self.tabs.iter_mut().map(|t| {
            let id = t.id;
            t.on_tick().map(move |msg| Message::tab(id, msg))
        }))
    }

    /// Helper to update a tab with an app message.
    pub fn update_tab_with_app_msg(
        &mut self,
        tab_id: usize,
        app_msg: impl Into<app::Message>,
        cfg: &Config,
    ) -> Task<Message> {
        self.update(Message::tab(tab_id, tab::Message::Run(app_msg.into())), cfg)
    }

    fn open_imported_claim(&mut self, source_tab: usize) -> Task<Message> {
        let prepared = self
            .tabs
            .iter()
            .find(|tab| tab.id == source_tab)
            .and_then(|tab| {
                if let tab::State::App(app) = &tab.state {
                    Some(
                        app.prepare_imported_claim()
                            .map(|(request, client)| (app.datadir().clone(), request, client)),
                    )
                } else {
                    None
                }
            });
        let Some(prepared) = prepared else {
            return Task::none();
        };
        let (root, request, client) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return self.show_app_error(source_tab, error),
        };
        let credentials = self.tabs.iter().find(|t| t.id == source_tab).and_then(|t| {
            let tab::State::App(app) = &t.state else {
                return None;
            };
            let cube = app.cube_settings().clone();
            let signer = app::session::unlocked_signer(&cube.id, cube.master_signer_fingerprint?)?;
            let pin = app::session::pin_for(&cube.id)?;
            Some((cube, crate::signer::Signer::new(signer), pin))
        });
        let Some((source, signer, pin)) = credentials else {
            return self.show_app_error(
                source_tab,
                "Unlock the source Cube with its PIN again before claiming.".into(),
            );
        };
        let backend =
            coincubed::config::BitcoinBackend::Esplora(crate::installer::connect_esplora_config(
                crate::chain::ChainId::BitcoinBlake2b,
                client.token().unwrap_or_default(),
            ));
        Task::perform(
            async move {
                crate::services::bitcoin_import::pairing::install_pair(
                    &root, &source, &signer, &pin, backend,
                )
                .await
                .map(|_| ())
            },
            move |result| Message::ImportedPairReady {
                source_tab,
                request: request.clone(),
                result,
            },
        )
    }

    pub fn update(&mut self, message: Message, cfg: &Config) -> Task<Message> {
        match message {
            Message::StartClaimInstaller(source_tab) => {
                return self.open_claim_installer(source_tab);
            }
            Message::ClaimInstallCancelled(temporary_tab) => {
                if let Some(index) = self.tabs.iter().position(|tab| tab.id == temporary_tab) {
                    self.close_tab(index);
                }
                if let Some(index) = self
                    .tabs
                    .iter()
                    .position(|tab| matches!(tab.state, tab::State::App(_)))
                {
                    self.focused_tab = index;
                }
                return Task::none();
            }
            Message::ImportedPairReady {
                source_tab,
                request,
                result,
            } => {
                let prepared = self.tabs.iter().find(|t| t.id == source_tab).and_then(|t| {
                    let tab::State::App(app) = &t.state else {
                        return None;
                    };
                    Some(app.prepare_imported_claim().and_then(|(current, client)| {
                        request.validate_session(&client)?;
                        if current.source != request.source {
                            return Err("The source changed. Open Claim again.".into());
                        }
                        result?;
                        request.validate(app.datadir(), &client)?;
                        Ok((app.datadir().clone(), current, client))
                    }))
                });
                let Some(prepared) = prepared else {
                    return Task::none();
                };
                let (root, request, client) = match prepared {
                    Ok(v) => v,
                    Err(error) => return self.show_app_error(source_tab, error),
                };
                let owner = self.tabs.iter().find(|t| t.id == source_tab).and_then(|t| {
                    let tab::State::App(app) = &t.state else {
                        return None;
                    };
                    Some(app.cache().app_generation)
                });
                let (mut home, task) = crate::home::Home::new_imported_claim(root, request, client);
                if let Some(owner) = owner {
                    home.set_claim_owner(owner);
                }
                self.tabs_created += 1;
                let id = self.tabs_created;
                let mut tab = tab::Tab::new(id, tab::State::Home(home));
                tab.set_theme_mode(self.theme_mode);
                self.tabs.push(tab);
                self.focused_tab = self.tabs.len() - 1;
                return task.map(move |message| Message::tab(id, tab::Message::Launch(message)));
            }
            Message::Tab(id, msg) => {
                if matches!(
                    msg.as_ref(),
                    tab::Message::Run(app::Message::View(app::view::Message::StartImportedClaim))
                ) {
                    return self.open_imported_claim(id);
                }
                let owner = self.tabs.iter().find(|tab| tab.id == id).and_then(|tab| {
                    let tab::State::App(app) = &tab.state else {
                        return None;
                    };
                    Some(app.cache().app_generation)
                });
                let task = self
                    .tabs
                    .iter_mut()
                    .find(|tab| tab.id == id)
                    .map(|tab| tab.update(*msg));
                if let Some(owner) = owner {
                    let retained = self.tabs.iter().any(|tab| {
                        tab.id == id
                            && matches!(&tab.state, tab::State::App(app) if app.cache().app_generation == owner)
                    });
                    if !retained {
                        self.close_tabs_matching(|tab| tab.claim_open_owner() == Some(owner));
                    }
                }
                return task
                    .map(|task| {
                        task.then(move |msg| match msg {
                            // Bubble ToggleTheme up to pane level as a ViewMessage
                            tab::Message::GlobalSettingsChanged => {
                                Task::done(Message::View(ViewMessage::GlobalSettingsChanged))
                            }
                            tab::Message::ToggleTheme => {
                                Task::done(Message::View(ViewMessage::ToggleTheme))
                            }
                            tab::Message::OpenBitcoinClaim(handoff) => {
                                Task::done(Message::View(ViewMessage::OpenBitcoinClaim(handoff)))
                            }
                            tab::Message::OpenForkClaim(handoff) => {
                                Task::done(Message::View(ViewMessage::OpenForkClaim(handoff)))
                            }
                            tab::Message::StartClaimInstaller => {
                                Task::done(Message::StartClaimInstaller(id))
                            }
                            tab::Message::ClaimInstallCancelled => {
                                Task::done(Message::ClaimInstallCancelled(id))
                            }
                            tab::Message::OpenConnectSignIn => {
                                Task::done(Message::View(ViewMessage::OpenConnectSignIn))
                            }
                            tab::Message::OpenLauncher => {
                                Task::done(Message::View(ViewMessage::OpenLauncher))
                            }
                            tab::Message::OpenPlanBilling => {
                                Task::done(Message::View(ViewMessage::OpenPlanBilling))
                            }
                            tab::Message::ConnectSignedIn => {
                                Task::done(Message::View(ViewMessage::ConnectSignedIn))
                            }
                            other => Task::done(Message::tab(id, other)),
                        })
                    })
                    .unwrap_or(Task::none());
            }
            Message::View(ViewMessage::FocusTab(i)) => {
                if i < self.tabs.len() {
                    self.focused_tab = i;
                    let tab = &self.tabs[i];
                    if let tab::State::App(app) = &tab.state {
                        if app.can_adopt_shared_connect_session() {
                            let id = tab.id;
                            return tab
                                .emit_app_task(Task::done(app::Message::View(
                                    app::view::Message::ConnectAccount(
                                        app::view::ConnectAccountMessage::Init,
                                    ),
                                )))
                                .map(move |message| Message::tab(id, message));
                        }
                    }
                    if let tab::State::Home(home) = &tab.state {
                        let id = tab.id;
                        return home
                            .on_focus()
                            .map(move |message| Message::tab(id, tab::Message::Launch(message)));
                    }
                }
            }
            // The public tab strip is gone. Home is created only by the
            // explicit Launcher/Account/Plan routes above.
            Message::View(ViewMessage::AddTab) => {}
            Message::View(ViewMessage::CloseTab(i)) => {
                self.close_tab(i);
            }
            // handle by the pane grid update.
            Message::View(ViewMessage::SplitTab(_)) => {}
            // handled at the GUI level
            // handled at the GUI level, which re-inits every pane's Cube tabs
            Message::View(
                ViewMessage::ToggleTheme
                | ViewMessage::GlobalSettingsChanged
                | ViewMessage::ConnectSignedIn,
            ) => {}
            Message::View(ViewMessage::OpenBitcoinClaim(handoff)) => {
                return self
                    .focus_home_tab_with(crate::home::Message::ReturnBitcoinClaim(handoff), cfg);
            }
            Message::View(ViewMessage::OpenForkClaim(handoff)) => {
                return self
                    .focus_home_tab_with(crate::home::Message::ContinueForkClaim(handoff), cfg);
            }
            Message::View(ViewMessage::OpenForkPair(handoff)) => {
                return self
                    .focus_home_tab_with(crate::home::Message::BeginClaimPair(handoff), cfg);
            }
            Message::View(ViewMessage::OpenConnectSignIn) => {
                // Focus/spawn a Home tab and land on Connect → Overview so the
                // user reaches the login form.
                let nav = crate::home::Message::View(crate::home::ViewMessage::GoToSection(
                    crate::home::HomeSection::Connect(crate::app::menu::ConnectSubMenu::Overview),
                ));
                return self.focus_home_tab_with(nav, cfg);
            }
            Message::View(ViewMessage::OpenLauncher) => {
                for tab in &mut self.tabs {
                    if let tab::State::App(app) = &mut tab.state {
                        app.dismiss_shell_disclosures();
                    }
                }
                return self.focus_launcher(cfg);
            }
            Message::View(ViewMessage::OpenPlanBilling) => {
                // Focus/spawn a Home tab and land on Connect → Plan & Billing,
                // for a paid-feature "View plans" CTA outside the Connect page
                // (e.g. Settings → Vault Recovery Alerts).
                let nav = crate::home::Message::View(crate::home::ViewMessage::GoToSection(
                    crate::home::HomeSection::Connect(
                        crate::app::menu::ConnectSubMenu::PlanBilling,
                    ),
                ));
                return self.focus_home_tab_with(nav, cfg);
            }
        }

        Task::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let subs: Vec<Subscription<Message>> = self
            .tabs
            .iter()
            .map(|t| {
                t.subscription()
                    .with(t.id)
                    .map(|(id, msg)| Message::tab(id, msg))
            })
            .collect();
        Subscription::batch(subs)
    }

    pub fn stop(&mut self) {
        self.tabs.iter_mut().for_each(|t| t.stop());
    }

    pub fn tabs_menu_view(&self) -> Element<Message> {
        let mut menu = Row::new().spacing(3);
        let tabs_len = self.tabs.len();
        for (i, tab) in self.tabs.iter().enumerate() {
            let title = tab.title();
            let title_row = if title.len() < 20 {
                Row::new().push(p1_regular(title)).push(p1_regular(
                    "                     "[..21 - title.len()].to_string(),
                ))
            } else {
                Row::new()
                    .push(p1_regular(&title[..17]))
                    .push(p1_regular("..."))
            };

            let tab_content: Element<ViewMessage> = if tabs_len > 1 {
                // Show close button when more than one tab
                Row::new()
                    .align_y(iced::Alignment::Center)
                    .push(
                        Button::new(title_row)
                            .style(if i == self.focused_tab {
                                theme::button::tab_liquid
                            } else {
                                theme::button::tab
                            })
                            .on_press(ViewMessage::FocusTab(i)),
                    )
                    .push(
                        Button::new(
                            iced::widget::Container::new(cross_icon().size(14))
                                .center_x(iced::Length::Fill)
                                .center_y(iced::Length::Fill),
                        )
                        .width(iced::Length::Fixed(28.0))
                        .height(iced::Length::Fixed(28.0))
                        .style(|_: &coincube_ui::theme::Theme, status| {
                            iced::widget::button::Style {
                                background: Some(iced::Background::Color(color::TRANSPARENT)),
                                text_color: match status {
                                    iced::widget::button::Status::Hovered => color::ORANGE,
                                    _ => color::GREY_3,
                                },
                                ..Default::default()
                            }
                        })
                        .padding(0)
                        .on_press(ViewMessage::CloseTab(i)),
                    )
                    .into()
            } else {
                // Single tab — no close button
                Button::new(title_row)
                    .style(if i == self.focused_tab {
                        theme::button::tab_liquid
                    } else {
                        theme::button::tab
                    })
                    .on_press(ViewMessage::FocusTab(i))
                    .into()
            };

            menu = menu.push(ContextMenu::new(tab_content, move || {
                Column::new()
                    .push(
                        Button::new(p1_regular("Close"))
                            .style(theme::button::secondary)
                            .on_press(ViewMessage::CloseTab(i))
                            .width(100),
                    )
                    .push(if tabs_len > 1 {
                        Some(
                            Button::new(p1_regular("Split"))
                                .style(theme::button::secondary)
                                .on_press(ViewMessage::SplitTab(i))
                                .width(100),
                        )
                    } else {
                        None
                    })
                    .into()
            }));
        }
        menu = menu.push(
            Button::new(plus_icon())
                .style(theme::button::tab)
                .on_press(ViewMessage::AddTab),
        );

        let menu: Element<ViewMessage> = menu.wrap().into();
        menu.map(Message::View)
    }

    pub fn view(&self) -> Element<Message> {
        Container::new(if let Some(t) = self.tabs.get(self.focused_tab) {
            let id = t.id;
            t.view().map(move |msg| Message::tab(id, msg))
        } else {
            Row::new().into()
        })
        .style(theme::container::background)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}
