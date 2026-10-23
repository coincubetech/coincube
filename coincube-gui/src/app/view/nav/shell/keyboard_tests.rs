use super::*;
use crate::app::view::nav::split::ShellSplit;
use iced::advanced::{
    layout,
    renderer::Headless,
    widget::{operation::focusable, Id, Operation, Tree},
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
