use super::*;
use crate::app::view::nav::split::ShellSplit;
use iced::advanced::{
    layout,
    renderer::Headless,
    widget::{operation::focusable, operation::Focusable, Id, Operation, Tree},
    Layout,
};
use iced::{keyboard, mouse, Event, Rectangle, Size};

fn fixture_context() -> NavContext<'static> {
    static STATUS: ConnectionStatus = ConnectionStatus::Inactive;
    NavContext {
        has_vault: true,
        has_imported_bitcoin: false,
        has_p2p: false,
        network: ChainId::Bitcoin,
        p2p_test_coordinator: false,
        marketplace_flags: features::MarketplaceServerFlags::OFF,
        liquid_gate: features::LiquidGate::HIDDEN,
        cube_name: "Synthetic QA Cube",
        lightning_address: None,
        avatar: None,
        theme_mode: coincube_ui::theme::palette::ThemeMode::Dark,
        connect_authenticated: false,
        connect_email: None,
        connect_stream_status: &STATUS,
        btcb2_server_enabled: false,
        btcb2_already_claimed: false,
        btcb2_claim_resume: false,
    }
}

fn shell_tree<'a>(ctx: &NavContext<'a>, state: ShellState) -> Element<'a, Message> {
    let menu = Menu::Cube(CubeSubMenu::Overview);
    iced::widget::column![
        top_bar(ctx),
        ShellSplit::new(
            sidebar(&menu, ctx, state),
            container(text("Synthetic content")).into()
        ),
    ]
    .into()
}

fn shell_with_input<'a>(
    ctx: &NavContext<'a>,
    menu: &Menu,
    state: ShellState,
) -> Element<'a, Message> {
    iced::widget::column![
        top_bar(ctx),
        ShellSplit::new(
            sidebar(menu, ctx, state),
            container(
                iced::widget::text_input("Synthetic content input", "")
                    .id("content-input")
                    .on_input(Message::Clipboard)
                    .on_submit(Message::Reload),
            )
            .padding(24)
            .into(),
        ),
    ]
    .into()
}

#[derive(Default)]
struct FocusSnapshot {
    bounds: Vec<(Id, Rectangle)>,
    focused: Vec<Id>,
}

impl Operation for FocusSnapshot {
    fn traverse(&mut self, f: &mut dyn FnMut(&mut dyn Operation)) {
        f(self);
    }

    fn focusable(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Focusable) {
        if let Some(id) = id {
            self.bounds.push((id.clone(), bounds));
            if state.is_focused() {
                self.focused.push(id.clone());
            }
        }
    }
}

fn focus_snapshot(
    element: &mut Element<'_, Message>,
    tree: &mut Tree,
    node: &layout::Node,
    renderer: &iced::Renderer,
) -> FocusSnapshot {
    let mut snapshot = FocusSnapshot::default();
    element
        .as_widget_mut()
        .operate(tree, Layout::new(node), renderer, &mut snapshot);
    snapshot
}

fn load_fonts() {
    for bytes in coincube_ui::font::load() {
        iced::advanced::graphics::text::font_system()
            .write()
            .unwrap()
            .load_font(bytes);
    }
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
            &mut iced::advanced::widget::operation::black_box(operation.as_mut()),
        );
        match operation.finish() {
            iced::advanced::widget::operation::Outcome::None => return None,
            iced::advanced::widget::operation::Outcome::Some(result) => return Some(result),
            iced::advanced::widget::operation::Outcome::Chain(next) => operation = next,
        }
    }
}

fn send(
    element: &mut Element<'_, Message>,
    tree: &mut Tree,
    node: &layout::Node,
    renderer: &iced::Renderer,
    event: Event,
    cursor: mouse::Cursor,
    size: Size,
) -> Vec<Message> {
    let mut messages = Vec::new();
    element.as_widget_mut().update(
        tree,
        &event,
        Layout::new(node),
        cursor,
        renderer,
        &mut iced::advanced::clipboard::Null,
        &mut iced::advanced::Shell::new(&mut messages),
        &Rectangle::with_size(size),
    );
    messages
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

#[test]
fn shell_actions_follow_visual_order_and_dispatch_keyboard_once() {
    load_fonts();
    let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
        coincube_ui::font::REGULAR,
        14.into(),
        Some("tiny-skia"),
    ))
    .unwrap();
    let ctx = fixture_context();
    let mut element = shell_tree(&ctx, ShellState::default());
    let mut tree = Tree::new(element.as_widget());
    let size = Size::new(1024.0, 768.0);
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, size),
    );
    for expected in [
        LAUNCHER_RETURN_ID,
        "tenshu.connect.sign-in",
        CUBE_SWITCHER_ID,
    ] {
        operate(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            focusable::focus_next::<()>(),
        );
        assert_eq!(
            operate(
                &mut element,
                &mut tree,
                &node,
                &renderer,
                focusable::find_focused(),
            ),
            Some(Id::new(expected))
        );
    }
    assert!(matches!(
        send(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            key(keyboard::key::Named::Enter, false, true),
            mouse::Cursor::Unavailable,
            size
        )
        .as_slice(),
        [Message::ToggleCubeSwitcher]
    ));
    assert!(send(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        key(keyboard::key::Named::Enter, true, true),
        mouse::Cursor::Unavailable,
        size
    )
    .is_empty());
    send(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        key(keyboard::key::Named::Enter, false, false),
        mouse::Cursor::Unavailable,
        size,
    );
    operate(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        focusable::focus_previous::<()>(),
    );
    assert_eq!(
        operate(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            focusable::find_focused()
        ),
        Some(Id::new("tenshu.connect.sign-in"))
    );
    assert!(matches!(
        send(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            key(keyboard::key::Named::Space, false, true),
            mouse::Cursor::Unavailable,
            size
        )
        .as_slice(),
        [Message::OpenConnectSignIn]
    ));
}

#[test]
fn shell_escape_prefers_review_then_switcher_then_tools() {
    let all = ShellState {
        switcher_open: true,
        lock_confirmation: true,
        tools_open: true,
        backup_attention: false,
    };
    assert!(matches!(escape_intent(all), Some(Message::CancelCubeLock)));
    assert!(matches!(
        escape_intent(ShellState {
            lock_confirmation: false,
            ..all
        }),
        Some(Message::ToggleCubeSwitcher)
    ));
    assert!(matches!(
        escape_intent(ShellState {
            lock_confirmation: false,
            switcher_open: false,
            ..all
        }),
        Some(Message::ToggleVaultTools)
    ));
    assert!(escape_intent(ShellState::default()).is_none());
}

#[test]
fn pointer_focus_transfers_across_production_shell_split_and_retained_route() {
    load_fonts();
    let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
        coincube_ui::font::REGULAR,
        14.into(),
        Some("tiny-skia"),
    ))
    .unwrap();
    let ctx = fixture_context();
    let mut menu = Menu::Cube(CubeSubMenu::Overview);
    let mut element = shell_with_input(&ctx, &menu, ShellState::default());
    let mut tree = Tree::new(element.as_widget());
    let size = Size::new(1024.0, 768.0);
    let mut node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, size),
    );
    operate(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        focusable::focus::<()>(Id::new("content-input")),
    );
    assert_eq!(
        focus_snapshot(&mut element, &mut tree, &node, &renderer).focused,
        vec![Id::new("content-input")]
    );

    // Backup routes to a different panel while the fixture retains its content
    // input. It has no unrelated disclosure focus task to mask the pointer bug.
    let target = Id::new("tenshu.shell.nav.Backup & protection");
    let bounds = focus_snapshot(&mut element, &mut tree, &node, &renderer)
        .bounds
        .into_iter()
        .find(|(id, _)| id == &target)
        .unwrap()
        .1;
    let cursor = mouse::Cursor::Available(bounds.center());
    let focus_intents = send(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        cursor,
        size,
    );
    assert!(matches!(focus_intents.as_slice(), [Message::FocusAction(id)] if id == &target));

    // The production App arm returns this exact whole-tree focus operation.
    // Apply it after rebuilding, as the runtime does for a Task.
    element = shell_with_input(&ctx, &menu, ShellState::default());
    tree.diff(element.as_widget());
    node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, size),
    );
    operate(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        focusable::focus::<()>(target.clone()),
    );
    assert_eq!(
        focus_snapshot(&mut element, &mut tree, &node, &renderer).focused,
        vec![target.clone()]
    );

    let route = send(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        cursor,
        size,
    );
    menu = Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::Recovery));
    assert!(matches!(route.as_slice(), [Message::Menu(to)] if to == &menu));
    element = shell_with_input(&ctx, &menu, ShellState::default());
    tree.diff(element.as_widget());
    node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, size),
    );
    assert_eq!(
        focus_snapshot(&mut element, &mut tree, &node, &renderer).focused,
        vec![target.clone()]
    );

    operate(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        focusable::focus_next::<()>(),
    );
    assert_eq!(
        focus_snapshot(&mut element, &mut tree, &node, &renderer).focused,
        vec![Id::new("tenshu.shell.nav.Spark")]
    );
    operate(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        focusable::focus_previous::<()>(),
    );
    assert_eq!(
        focus_snapshot(&mut element, &mut tree, &node, &renderer).focused,
        vec![target]
    );

    // Clicking the retained content input transfers focus in the reverse
    // direction; neither Enter nor later Tab can activate the old shell row.
    let input_bounds = focus_snapshot(&mut element, &mut tree, &node, &renderer)
        .bounds
        .into_iter()
        .find(|(id, _)| id == &Id::new("content-input"))
        .unwrap()
        .1;
    let input_cursor = mouse::Cursor::Available(input_bounds.center());
    send(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        input_cursor,
        size,
    );
    send(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        input_cursor,
        size,
    );
    assert_eq!(
        focus_snapshot(&mut element, &mut tree, &node, &renderer).focused,
        vec![Id::new("content-input")]
    );
    assert!(matches!(
        send(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            key(keyboard::key::Named::Enter, false, true),
            mouse::Cursor::Unavailable,
            size
        )
        .as_slice(),
        [Message::Reload]
    ));
    operate(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        focusable::focus_previous::<()>(),
    );
    assert_eq!(
        focus_snapshot(&mut element, &mut tree, &node, &renderer)
            .focused
            .len(),
        1
    );
}

#[test]
fn production_lock_review_rebuild_focuses_cancel_then_restores_lock_trigger() {
    load_fonts();
    let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
        coincube_ui::font::REGULAR,
        14.into(),
        Some("tiny-skia"),
    ))
    .unwrap();
    let ctx = fixture_context();
    let menu = Menu::Cube(CubeSubMenu::Overview);
    let mut state = ShellState {
        switcher_open: true,
        ..Default::default()
    };
    let mut element = shell_with_input(&ctx, &menu, state);
    let mut tree = Tree::new(element.as_widget());
    let size = Size::new(1024.0, 768.0);
    let mut node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, size),
    );
    operate(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        focusable::focus::<()>(Id::new(CUBE_LOCK_ID)),
    );
    assert!(matches!(
        send(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            key(keyboard::key::Named::Enter, false, true),
            mouse::Cursor::Unavailable,
            size
        )
        .as_slice(),
        [Message::AskCubeLock]
    ));

    state.lock_confirmation = true;
    element = shell_with_input(&ctx, &menu, state);
    tree.diff(element.as_widget());
    node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, size),
    );
    operate(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        focusable::focus::<()>(Id::new(CUBE_LOCK_CANCEL_ID)),
    );
    assert_eq!(
        focus_snapshot(&mut element, &mut tree, &node, &renderer).focused,
        vec![Id::new(CUBE_LOCK_CANCEL_ID)]
    );
    assert!(matches!(
        escape_intent(state),
        Some(Message::CancelCubeLock)
    ));

    state.lock_confirmation = false;
    element = shell_with_input(&ctx, &menu, state);
    tree.diff(element.as_widget());
    node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, size),
    );
    operate(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        focusable::focus::<()>(Id::new(CUBE_LOCK_ID)),
    );
    assert_eq!(
        focus_snapshot(&mut element, &mut tree, &node, &renderer).focused,
        vec![Id::new(CUBE_LOCK_ID)]
    );
    assert!(matches!(
        escape_intent(state),
        Some(Message::ToggleCubeSwitcher)
    ));
    state.switcher_open = false;
    element = shell_with_input(&ctx, &menu, state);
    tree.diff(element.as_widget());
    node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, size),
    );
    operate(
        &mut element,
        &mut tree,
        &node,
        &renderer,
        focusable::focus::<()>(Id::new(CUBE_SWITCHER_ID)),
    );
    assert_eq!(
        focus_snapshot(&mut element, &mut tree, &node, &renderer).focused,
        vec![Id::new(CUBE_SWITCHER_ID)]
    );
    assert!(escape_intent(state).is_none());
}

#[tokio::test]
async fn focused_shell_capture_uses_production_compact_and_full_split() {
    load_fonts();
    for (mode, theme) in [
        (
            coincube_ui::theme::palette::ThemeMode::Dark,
            theme::Theme::dark(),
        ),
        (
            coincube_ui::theme::palette::ThemeMode::Light,
            theme::Theme::light(),
        ),
    ] {
        for (width, height) in [(1024, 768), (1440, 900)] {
            let mut ctx = fixture_context();
            ctx.theme_mode = mode;
            let mut element = shell_tree(&ctx, ShellState::default());
            let mut renderer = <iced::Renderer as Headless>::new(
                coincube_ui::font::REGULAR,
                iced::Pixels(14.0),
                Some("tiny-skia"),
            )
            .await
            .unwrap();
            let mut tree = Tree::new(element.as_widget());
            let size = Size::new(width as f32, height as f32);
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
                focusable::focus::<()>(Id::new(CUBE_SWITCHER_ID)),
            );
            assert_eq!(
                operate(
                    &mut element,
                    &mut tree,
                    &node,
                    &renderer,
                    focusable::find_focused()
                ),
                Some(Id::new(CUBE_SWITCHER_ID))
            );
            let expected_sidebar = if width == 1024 {
                COMPACT_WIDTH
            } else {
                FULL_WIDTH
            };
            assert_eq!(
                node.children()[1].children()[0].bounds().width,
                expected_sidebar
            );
            if let Ok(directory) = std::env::var("SHELL_KEYBOARD_PREVIEW_DIR") {
                let redraw = Event::Window(iced::window::Event::RedrawRequested(
                    std::time::Instant::now(),
                ));
                assert!(send(
                    &mut element,
                    &mut tree,
                    &node,
                    &renderer,
                    redraw,
                    mouse::Cursor::Unavailable,
                    size
                )
                .is_empty());
                element.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &theme,
                    &iced::advanced::renderer::Style {
                        text_color: theme.colors.shell.ink,
                    },
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &Rectangle::with_size(size),
                );
                std::fs::create_dir_all(&directory).unwrap();
                image::save_buffer(
                    std::path::Path::new(&directory).join(format!(
                        "tenshu-keyboard-focus-{mode:?}-{width}x{height}.png"
                    )),
                    &renderer.screenshot(
                        Size::new(width, height),
                        1.0,
                        theme.colors.shell.background,
                    ),
                    width,
                    height,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
    }
}
