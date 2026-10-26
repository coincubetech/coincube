//! Route and compact-layout regressions from the PR #805 post-merge review.
use super::*;
use crate::app::{self, view};
use iced::futures::StreamExt;

fn launcher_gui(root: &CoincubeDirectory) -> GUI {
    let (mut home, startup) = home::Home::new_for_chain(root.clone(), None);
    drop(startup);
    // A fresh install's mandatory advisory modal owns focus until dismissed.
    // Connect is reachable only after that queue has been acknowledged.
    for _ in crate::hw_advisory::ADVISORIES {
        drop(home.update(home::Message::View(
            home::ViewMessage::DismissAdvisoryNotice,
        )));
    }
    let pane = pane::Pane::new_with_tab(tab::State::Home(home));
    let (panes, focus) = pane_grid::State::new(pane);
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
        connect_sheet: None,
        preferences: unresolved_test_preferences(),
    }
}

fn sign_in_home(gui: &mut GUI, pane_id: pane_grid::Pane) {
    let tab::State::Home(home) = &mut gui.panes.get_mut(pane_id).unwrap().tabs[0].state else {
        unreachable!()
    };
    home.connect_account.user = Some(crate::services::coincube::User {
        id: 7,
        email: "shell@example.invalid".into(),
        email_verified: Some(true),
    });
    home.connect_account.step = crate::app::state::connect::ConnectFlowStep::Dashboard;
}

fn launcher_connect_entry(gui: &mut GUI, pane_id: pane_grid::Pane, tab_id: usize) {
    drop(gui.update(Message::Pane(
        pane_id,
        pane::Message::tab(
            tab_id,
            tab::Message::Launch(home::Message::View(home::ViewMessage::GoToSection(
                home::HomeSection::Connect(crate::app::menu::ConnectSubMenu::Overview),
            ))),
        ),
    )));
}

fn home_section(gui: &GUI, pane_id: pane_grid::Pane) -> String {
    match &gui.panes.get(pane_id).unwrap().tabs[0].state {
        tab::State::Home(home) => format!("{:?}", home.active_section),
        _ => "not Home".into(),
    }
}

/// Mount the actual post-Close GUI tree and prove that the recorded control
/// accepts whole-tree focus. A queued focus task alone cannot establish that.
fn assert_focus_target_mounted(gui: &GUI, id: iced::advanced::widget::Id) {
    use iced::advanced::widget::operation::Focusable;
    use iced::advanced::widget::operation::{self, focusable, Operation, Outcome};
    use iced::advanced::{layout, renderer::Headless, widget::Tree, Layout};
    use iced::Rectangle;
    for bytes in [
        coincube_ui::font::REGULAR_BYTES,
        coincube_ui::font::MEDIUM_BYTES,
        coincube_ui::font::BOLD_BYTES,
        coincube_ui::font::BOOTSTRAP_ICONS_BYTE,
    ] {
        iced::advanced::graphics::text::font_system()
            .write()
            .unwrap()
            .load_font(std::borrow::Cow::Borrowed(bytes));
    }
    let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
        coincube_ui::font::REGULAR,
        iced::Pixels(14.0),
        Some("tiny-skia"),
    ))
    .expect("software renderer");
    let mut element = gui.view();
    let mut tree = Tree::new(element.as_widget());
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(iced::Size::ZERO, iced::Size::new(1024.0, 768.0)),
    );
    #[derive(Default)]
    struct FocusIds(Vec<iced::advanced::widget::Id>);
    impl Operation for FocusIds {
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
    let mut ids = FocusIds::default();
    element
        .as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut ids);
    assert!(
        ids.0.contains(&id),
        "return ID {id:?} absent from mounted tree: {:?}",
        ids.0
    );
    // `focus` has a unit result, so run it before `find_focused`.
    let mut focus: Box<dyn Operation<()>> = Box::new(focusable::focus(id.clone()));
    loop {
        element.as_widget_mut().operate(
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut operation::black_box(focus.as_mut()),
        );
        match focus.finish() {
            Outcome::Chain(next) => focus = next,
            Outcome::None | Outcome::Some(()) => break,
        }
    }
    let mut find: Box<dyn Operation<iced::advanced::widget::Id>> =
        Box::new(focusable::find_focused());
    let found = loop {
        element.as_widget_mut().operate(
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut operation::black_box(find.as_mut()),
        );
        match find.finish() {
            Outcome::None => break None,
            Outcome::Some(value) => break Some(value),
            Outcome::Chain(next) => find = next,
        }
    };
    assert_eq!(found, Some(id));
}

fn with_runtime(
    name: &str,
    body: impl FnOnce(&std::path::Path, &tokio::runtime::Runtime) + Send + 'static,
) {
    let name = name.to_owned();
    std::thread::Builder::new()
        .name(name.clone())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let _guard = app::session::test_guard();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _enter = runtime.enter();
            let path = std::env::temp_dir().join(format!("{}-{}", name, uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            body(&path, &runtime);
            std::fs::remove_dir_all(&path).ok();
        })
        .unwrap()
        .join()
        .unwrap();
}

/// P1: an in-sheet "Plan & billing" CTA must not navigate the retained
/// Launcher behind the sheet (PR body: "preserve its background").
#[test]
fn probe_p1_launcher_open_plan_billing_keeps_background() {
    with_runtime("pr805-p1", |path, _| {
        let root = CoincubeDirectory::new(path.to_path_buf());
        let mut gui = launcher_gui(&root);
        let pane_id = gui.focus.unwrap();
        let tab_id = gui.panes.get(pane_id).unwrap().tabs[0].id;
        sign_in_home(&mut gui, pane_id);
        launcher_connect_entry(&mut gui, pane_id, tab_id);
        assert!(gui.connect_sheet.is_some());
        let before = home_section(&gui, pane_id);
        drop(
            gui.update(Message::ConnectSheet(ConnectSheetMessage::Account(
                view::ConnectAccountMessage::OpenPlanBilling,
            ))),
        );
        let after = home_section(&gui, pane_id);
        drop(gui.update(Message::ConnectSheet(ConnectSheetMessage::Close)));
        let closed = home_section(&gui, pane_id);
        println!("P1 before={before} after_cta={after} after_close={closed}");
        assert!(
            !after.starts_with("Connect"),
            "Launcher background navigated to {} (was {}); after Close: {}",
            after,
            before,
            closed
        );
    });
}

/// P1b: same for the renewal banner CTA.
#[test]
fn probe_p1b_launcher_renew_current_plan_keeps_background() {
    with_runtime("pr805-p1b", |path, _| {
        let root = CoincubeDirectory::new(path.to_path_buf());
        let mut gui = launcher_gui(&root);
        let pane_id = gui.focus.unwrap();
        let tab_id = gui.panes.get(pane_id).unwrap().tabs[0].id;
        sign_in_home(&mut gui, pane_id);
        launcher_connect_entry(&mut gui, pane_id, tab_id);
        let before = home_section(&gui, pane_id);
        drop(
            gui.update(Message::ConnectSheet(ConnectSheetMessage::Account(
                view::ConnectAccountMessage::RenewCurrentPlan,
            ))),
        );
        let after = home_section(&gui, pane_id);
        println!("P1b before={before} after_cta={after}");
        assert!(
            !after.starts_with("Connect"),
            "Launcher background navigated to {} (was {})",
            after,
            before
        );
    });
}

/// P2: opening from the Launcher's Connect entry must record that control so
/// Close/Escape can restore focus to it, as the Preferences sheet does.
#[test]
fn probe_p2_launcher_entry_records_focus_origin() {
    with_runtime("pr805-p2", |path, _| {
        let root = CoincubeDirectory::new(path.to_path_buf());
        let mut gui = launcher_gui(&root);
        let pane_id = gui.focus.unwrap();
        let tab_id = gui.panes.get(pane_id).unwrap().tabs[0].id;
        sign_in_home(&mut gui, pane_id);
        launcher_connect_entry(&mut gui, pane_id, tab_id);
        let focus = gui.connect_sheet.as_ref().unwrap().origin.focus.clone();
        println!("P2 origin.focus={focus:?}");
        assert_eq!(
            focus,
            Some(iced::advanced::widget::Id::new(
                "tenshu.launcher.legacy:connect:entry"
            )),
            "wrong no-Cube Launcher return target"
        );
        let close = gui.update(Message::KeyPressed(Key::Escape));
        assert!(close.units() > 0, "Escape restored no focus");
        assert_focus_target_mounted(&gui, focus.expect("recorded return target"));
    });
}

#[test]
fn no_cube_signed_out_launcher_close_focuses_mounted_sign_in() {
    with_runtime("connect-no-cube-focus", |path, _| {
        let root = CoincubeDirectory::new(path.to_path_buf());
        let mut gui = launcher_gui(&root);
        let pane_id = gui.focus.unwrap();
        let tab_id = gui.panes.get(pane_id).unwrap().tabs[0].id;
        if let tab::State::Home(home) = &mut gui.panes.get_mut(pane_id).unwrap().tabs[0].state {
            home.connect_account.step = crate::app::state::connect::ConnectFlowStep::Login {
                email: String::new(),
                loading: false,
            };
        }
        launcher_connect_entry(&mut gui, pane_id, tab_id);
        let focus = gui.connect_sheet.as_ref().unwrap().origin.focus.clone();
        assert_eq!(
            focus,
            Some(iced::advanced::widget::Id::new(
                "tenshu.launcher.legacy:connect:sign-in"
            ))
        );
        assert!(
            gui.update(Message::ConnectSheet(ConnectSheetMessage::Close))
                .units()
                > 0
        );
        assert_focus_target_mounted(&gui, focus.unwrap());
    });
}

/// P3: signed out, the Launcher's "Sign In" entry opens the sign-in screen
/// (review doc: "signed out, its Connect entry ... opens the V6 sign-in screen").
#[test]
fn probe_p3_launcher_signed_out_entry_opens_sign_in() {
    with_runtime("pr805-p3", |path, _| {
        let root = CoincubeDirectory::new(path.to_path_buf());
        let mut gui = launcher_gui(&root);
        let pane_id = gui.focus.unwrap();
        let tab_id = gui.panes.get(pane_id).unwrap().tabs[0].id;
        if let tab::State::Home(home) = &mut gui.panes.get_mut(pane_id).unwrap().tabs[0].state {
            home.connect_account.step = crate::app::state::connect::ConnectFlowStep::Login {
                email: String::new(),
                loading: false,
            };
        }
        launcher_connect_entry(&mut gui, pane_id, tab_id);
        let page = gui.connect_sheet.as_ref().map(|sheet| sheet.page);
        println!("P3 page={page:?}");
        assert_eq!(page, Some(ConnectSheetPage::Authentication));
    });
}

async fn outputs_within(task: Task<Message>) -> Vec<Message> {
    let Some(mut stream) = iced_runtime::task::into_stream(task) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some(iced_runtime::Action::Output(message))) => out.push(message),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    out
}

/// P4: a signed-out Cube's top-bar "Sign in" should overlay that Cube and
/// Close should return to it (P-Overview chip -> P-Connect-InCube).
#[test]
fn probe_p4_signed_out_cube_chip_overlays_cube() {
    with_runtime("pr805-p4", |path, runtime| {
        let (app, _) = app::claim_step1_tests::bitcoin_app(path);
        assert!(!app.can_restore_connect_session());
        let (panes, pane_id) =
            pane_grid::State::new(pane::Pane::new_with_tab(tab::State::App(app)));
        let mut gui = GUI {
            panes,
            focus: Some(pane_id),
            config: Config::new(CoincubeDirectory::new(path.to_path_buf()), None),
            window_id: None,
            window_init: None,
            window_config: None,
            global_cache: GlobalCache::default(),
            theme_mode: Default::default(),
            system_theme: None,
            system_reduces_motion: None,
            preferences_sheet: None,
            connect_sheet: None,
            preferences: unresolved_test_preferences(),
        };
        let cube_tab = gui.panes.get(pane_id).unwrap().tabs[0].id;
        drop(gui.view());
        let mut queue = vec![Message::Pane(
            pane_id,
            pane::Message::tab(
                cube_tab,
                tab::Message::Run(AppMessage::View(view::Message::OpenConnectSignIn)),
            ),
        )];
        let mut seen = Vec::new();
        for _ in 0..4 {
            let mut next = Vec::new();
            for message in queue.drain(..) {
                let navigation = match &message {
                    Message::Pane(_, pane::Message::View(_)) => true,
                    Message::Pane(_, pane::Message::Tab(_, inner))
                        if matches!(inner.as_ref(), tab::Message::AppAsync(..))
                            && format!("{:?}", inner).contains("OpenConnectSignIn") =>
                    {
                        true
                    }
                    Message::Pane(_, pane::Message::Tab(_, inner)) => matches!(
                        inner.as_ref(),
                        tab::Message::Run(AppMessage::View(view::Message::OpenConnectSignIn))
                            | tab::Message::Launch(home::Message::View(
                                home::ViewMessage::GoToSection(_)
                            ))
                    ),
                    _ => false,
                };
                if !navigation {
                    seen.push(format!(
                        "skipped: {}",
                        format!("{:?}", message)
                            .chars()
                            .take(160)
                            .collect::<String>()
                    ));
                    continue;
                }
                seen.push(format!("{message:?}").chars().take(160).collect::<String>());
                let task = gui.update(message);
                next.extend(runtime.block_on(outputs_within(task)));
            }
            queue = next;
        }
        let pane = gui.panes.get(pane_id).unwrap();
        let focused = &pane.tabs[pane.focused_tab];
        let focused_kind = match &focused.state {
            tab::State::Home(_) => "Home",
            tab::State::App(_) => "App",
            _ => "other",
        };
        let origin = gui
            .connect_sheet
            .as_ref()
            .map(|sheet| (sheet.origin.tab_id, sheet.origin.source, sheet.page));
        println!("P4 route={seen:#?}");
        println!(
            "P4 cube_tab={cube_tab} tabs={} focused={} ({focused_kind}) sheet={origin:?}",
            pane.tabs.len(),
            focused.id
        );
        assert_eq!(
            origin.map(|(tab_id, _, _)| tab_id),
            Some(cube_tab),
            "sheet did not overlay the Cube"
        );
        assert_eq!(focused_kind, "App");
        drop(gui.update(Message::ConnectSheet(ConnectSheetMessage::Close)));
        assert!(gui.connect_sheet.is_none());
        assert_eq!(gui.panes.get(pane_id).unwrap().tabs[0].id, cube_tab);
    });
}

#[test]
fn view_plans_sheet_returns_focus_to_cube_settings() {
    with_runtime("connect-view-plans-focus", |path, _| {
        let (app, _) = app::claim_step1_tests::bitcoin_app(path);
        let (panes, pane_id) =
            pane_grid::State::new(pane::Pane::new_with_tab(tab::State::App(app)));
        let mut gui = GUI {
            panes,
            focus: Some(pane_id),
            config: Config::new(CoincubeDirectory::new(path.to_path_buf()), None),
            window_id: None,
            window_init: None,
            window_config: None,
            global_cache: GlobalCache::default(),
            theme_mode: Default::default(),
            system_theme: None,
            system_reduces_motion: None,
            preferences_sheet: None,
            connect_sheet: None,
            preferences: unresolved_test_preferences(),
        };
        let tab_id = gui.panes.get(pane_id).unwrap().tabs[0].id;
        for id in [
            crate::app::view::settings::general::RECOVERY_ALERTS_VIEW_PLANS_ID,
            crate::app::view::settings::general::ESCROW_VIEW_PLANS_ID,
        ] {
            drop(gui.update(Message::Pane(
                pane_id,
                pane::Message::tab(
                    tab_id,
                    tab::Message::Run(AppMessage::View(view::Message::OpenPlanBillingFrom(id))),
                ),
            )));
            let sheet = gui.connect_sheet.as_ref().expect("Cube plans sheet");
            assert_eq!(sheet.origin.tab_id, tab_id);
            assert_eq!(
                sheet.origin.focus,
                Some(iced::advanced::widget::Id::new(id))
            );
            assert!(gui.update(Message::KeyPressed(Key::Escape)).units() > 0);
            assert!(gui.connect_sheet.is_none());
        }
    });
}

fn capture(gui: &GUI, name: &str) {
    use iced::advanced::{layout, renderer::Headless, widget::Tree, Layout};
    for bytes in [
        coincube_ui::font::REGULAR_BYTES,
        coincube_ui::font::MEDIUM_BYTES,
        coincube_ui::font::BOLD_BYTES,
        coincube_ui::font::BOOTSTRAP_ICONS_BYTE,
    ] {
        iced::advanced::graphics::text::font_system()
            .write()
            .unwrap()
            .load_font(std::borrow::Cow::Borrowed(bytes));
    }
    let mut renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
        coincube_ui::font::REGULAR,
        iced::Pixels(14.0),
        Some("tiny-skia"),
    ))
    .expect("software renderer");
    let theme = gui.theme();
    let size = iced::Size::new(1024.0, 768.0);
    let mut element = gui.view();
    let mut tree = Tree::new(element.as_widget());
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(iced::Size::ZERO, size),
    );
    let viewport = iced::Rectangle::with_size(size);
    element.as_widget_mut().update(
        &mut tree,
        &iced::Event::Window(iced::window::Event::RedrawRequested(Instant::now())),
        Layout::new(&node),
        iced::mouse::Cursor::Unavailable,
        &renderer,
        &mut iced::advanced::clipboard::Null,
        &mut iced::advanced::Shell::new(&mut Vec::new()),
        &viewport,
    );
    element.as_widget().draw(
        &tree,
        &mut renderer,
        &theme,
        &iced::advanced::renderer::Style {
            text_color: theme.colors.text.primary,
        },
        Layout::new(&node),
        iced::mouse::Cursor::Unavailable,
        &viewport,
    );
    let pixels = renderer.screenshot(
        iced::Size::new(1024, 768),
        1.0,
        theme.colors.general.background,
    );
    let directory = std::path::PathBuf::from(
        std::env::var_os("CONNECT_SHEET_CAPTURE_DIR").expect("capture directory set"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    image::save_buffer(
        directory.join(format!("{}.png", name)),
        &pixels,
        1024,
        768,
        image::ColorType::Rgba8,
    )
    .expect("save capture");
}

/// P5: composed 1024×768 captures of the mounted sheet for visual review.
#[test]
fn probe_p5_capture_compact_composed_sheet() {
    if std::env::var_os("CONNECT_SHEET_CAPTURE_DIR").is_none() {
        return;
    }
    with_runtime("pr805-p5", |path, _| {
        let root = CoincubeDirectory::new(path.to_path_buf());

        let mut gui = launcher_gui(&root);
        let pane_id = gui.focus.unwrap();
        let tab_id = gui.panes.get(pane_id).unwrap().tabs[0].id;
        if let tab::State::Home(home) = &mut gui.panes.get_mut(pane_id).unwrap().tabs[0].state {
            home.connect_account.step = crate::app::state::connect::ConnectFlowStep::Login {
                email: String::new(),
                loading: false,
            };
        }
        launcher_connect_entry(&mut gui, pane_id, tab_id);
        capture(&gui, "launcher-signed-out-entry");
        drop(gui.update(Message::ConnectSheet(
            ConnectSheetMessage::OpenAuthentication {
                create_account: false,
            },
        )));
        capture(&gui, "launcher-sign-in-page");

        let mut gui = launcher_gui(&root);
        let pane_id = gui.focus.unwrap();
        let tab_id = gui.panes.get(pane_id).unwrap().tabs[0].id;
        capture(&gui, "launcher-before-sheet");
        sign_in_home(&mut gui, pane_id);
        launcher_connect_entry(&mut gui, pane_id, tab_id);
        capture(&gui, "launcher-signed-in-account");
        drop(
            gui.update(Message::ConnectSheet(ConnectSheetMessage::SelectSection(
                ConnectSheetSection::PlanBilling,
            ))),
        );
        capture(&gui, "launcher-signed-in-plan");
        drop(
            gui.update(Message::ConnectSheet(ConnectSheetMessage::Account(
                view::ConnectAccountMessage::OpenPlanBilling,
            ))),
        );
        capture(&gui, "launcher-after-plan-cta");
        drop(gui.update(Message::ConnectSheet(ConnectSheetMessage::Close)));
        capture(&gui, "launcher-after-plan-cta-close");

        let (mut app, _) = app::claim_step1_tests::bitcoin_app(path);
        app::claim_step1_tests::signed_in_connect_for_shell(&mut app);
        let (panes, pane_id) =
            pane_grid::State::new(pane::Pane::new_with_tab(tab::State::App(app)));
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
            connect_sheet: None,
            preferences: unresolved_test_preferences(),
        };
        let tab_id = gui.panes.get(pane_id).unwrap().tabs[0].id;
        capture(&gui, "cube-before-sheet");
        drop(gui.update(Message::Pane(
            pane_id,
            pane::Message::tab(
                tab_id,
                tab::Message::Run(AppMessage::View(view::Message::OpenConnectSignIn)),
            ),
        )));
        capture(&gui, "cube-signed-in-account");
    });
}
