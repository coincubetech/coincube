//! Offscreen frames of the shared top bar on an open Cube, through the
//! production wgpu renderer, for review against the `P-` boards. Opt-in only:
//! `TOP_BAR_CAPTURE_DIR=<dir> cargo test -p coincube-gui --lib top_bar_capture
//! -- --ignored`. Every Cube is a synthetic fixture.

use std::path::PathBuf;
use std::time::Instant;

use coincube_ui::{font, theme, widget::Element};
use iced::advanced::{
    layout, mouse,
    renderer::Headless,
    widget::{operation, Id, Operation, Tree},
    Layout, Renderer as _, Shell,
};
use iced::{Rectangle, Size};

use crate::app::{
    cache::Cache,
    menu::{CubeSubMenu, Menu},
    state::preferences::{BalancePrivacy, BalancePrivacyChrome},
    ConnectionStatus,
};

pub(crate) const WINDOWS: [(f32, f32); 2] = [(1024.0, 768.0), (1440.0, 900.0)];

pub(crate) fn directory() -> Option<PathBuf> {
    let directory = PathBuf::from(std::env::var("TOP_BAR_CAPTURE_DIR").ok()?);
    std::fs::create_dir_all(&directory).unwrap();
    Some(directory)
}

pub(crate) fn renderer() -> iced::Renderer {
    for bytes in font::load() {
        iced::advanced::graphics::text::font_system()
            .write()
            .unwrap()
            .load_font(bytes);
    }
    iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
        font::REGULAR,
        iced::Pixels(14.0),
        Some("wgpu"),
    ))
    .expect("wgpu headless renderer")
}

/// Lays `element` out at `size`, presses Tab and focuses `focus` (so the
/// ring paints), sends the first redraw and saves the frame to `path`. Panics if
/// `focus` names no focusable control, so a frame can't be saved unfocused
/// under a focus name.
pub(crate) fn save<M>(
    renderer: &mut iced::Renderer,
    mut element: Element<'_, M>,
    size: Size,
    theme: &theme::Theme,
    focus: Option<Id>,
    path: PathBuf,
) {
    let mut tree = Tree::new(element.as_widget());
    let node =
        element
            .as_widget_mut()
            .layout(&mut tree, renderer, &layout::Limits::new(Size::ZERO, size));
    if let Some(id) = focus {
        // Arrive as a keyboard user does, after Tab: only keyboard navigation
        // paints the ring (`coincube_ui::widget::action`).
        let tab = iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab);
        let viewport = Rectangle::with_size(size);
        element.as_widget_mut().update(
            &mut tree,
            &iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: tab.clone(),
                modified_key: tab,
                physical_key: iced::keyboard::key::Physical::Unidentified(
                    iced::keyboard::key::NativeCode::Unidentified,
                ),
                location: iced::keyboard::Location::Standard,
                modifiers: iced::keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            }),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            renderer,
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut Vec::new()),
            &viewport,
        );
        let mut focus: Box<dyn Operation<()>> =
            Box::new(operation::focusable::focus::<()>(id.clone()));
        loop {
            element.as_widget_mut().operate(
                &mut tree,
                Layout::new(&node),
                renderer,
                &mut operation::black_box(focus.as_mut()),
            );
            match focus.finish() {
                operation::Outcome::Chain(next) => focus = next,
                _ => break,
            }
        }
        let mut focused = operation::focusable::find_focused();
        element.as_widget_mut().operate(
            &mut tree,
            Layout::new(&node),
            renderer,
            &mut operation::black_box(&mut focused),
        );
        match focused.finish() {
            operation::Outcome::Some(found) => assert_eq!(found, id, "{path:?}"),
            _ => panic!("{:?} is not a focusable control here: {:?}", id, path),
        }
    }
    let viewport = Rectangle::with_size(size);
    renderer.reset(viewport);
    element.as_widget_mut().update(
        &mut tree,
        &iced::Event::Window(iced::window::Event::RedrawRequested(Instant::now())),
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        renderer,
        &mut iced::advanced::clipboard::Null,
        &mut Shell::new(&mut Vec::new()),
        &viewport,
    );
    element.as_widget().draw(
        &tree,
        renderer,
        theme,
        &iced::advanced::renderer::Style {
            text_color: theme.colors.shell.ink,
        },
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &viewport,
    );
    let pixels = renderer.screenshot(
        Size::new(size.width as u32, size.height as u32),
        1.0,
        theme.colors.shell.background,
    );
    renderer.reset(viewport);
    image::save_buffer(
        path,
        &pixels,
        size.width as u32,
        size.height as u32,
        image::ColorType::Rgba8,
    )
    .expect("save capture");
}

pub(crate) fn themes() -> [(&'static str, theme::Theme); 2] {
    [
        ("dark", theme::Theme::dark()),
        ("light", theme::Theme::light()),
    ]
}

fn cache(light: bool, privacy: BalancePrivacy) -> Cache {
    Cache {
        cube_name: "Everyday".into(),
        has_vault: true,
        sidebar_connect_signed_in: true,
        connect_email: Some("qa@example.invalid".into()),
        connect_stream_status: ConnectionStatus::Connected,
        balance_privacy: Some(BalancePrivacyChrome {
            privacy,
            busy: false,
            interactive: true,
        }),
        theme_mode: if light {
            coincube_ui::theme::palette::ThemeMode::Light
        } else {
            coincube_ui::theme::palette::ThemeMode::Dark
        },
        ..Default::default()
    }
}

fn dashboard(cache: &Cache) -> Element<'_, crate::app::view::Message> {
    static MENU: Menu = Menu::Cube(CubeSubMenu::Overview);
    crate::app::view::dashboard(
        &MENU,
        cache,
        iced::widget::column![
            iced::widget::text("Total balance").size(24),
            iced::widget::text("Synthetic top bar preview").size(14),
        ],
    )
}

#[test]
#[ignore = "writes PNG frames; needs a GPU adapter"]
fn top_bar_capture_cube() {
    let Some(directory) = directory() else {
        return;
    };
    let mut renderer = renderer();
    for (appearance, palette) in themes() {
        let light = appearance == "light";
        for (privacy, state) in [
            (BalancePrivacy::Shown, "shown"),
            (BalancePrivacy::Hidden, "hidden"),
        ] {
            let cache = cache(light, privacy);
            for (width, height) in WINDOWS {
                save(
                    &mut renderer,
                    dashboard(&cache),
                    Size::new(width, height),
                    &palette,
                    None,
                    directory.join(format!("cube-{appearance}-{state}-{width}x{height}.png")),
                );
            }
        }
        // Each control's focus ring, between its neighbours and the gutter.
        let cache = cache(light, BalancePrivacy::Shown);
        for (id, name) in [
            (super::LAUNCHER_RETURN_ID, "brand"),
            (super::BALANCE_PRIVACY_ID, "eye"),
            (super::CONNECTION_ID, "connection"),
            (super::APP_PREFERENCES_ID, "preferences"),
        ] {
            save(
                &mut renderer,
                dashboard(&cache),
                Size::new(1024.0, 768.0),
                &palette,
                Some(Id::new(id)),
                directory.join(format!("cube-{appearance}-focus-{name}-1024x768.png")),
            );
        }
    }
}

/// The side panel with both wallets named, each wallet and Settings selected
/// in turn, for review against `P-Overview`, `P-Send` and `P-Settings`
/// (Tenshu v175 L2). Selection alone paints no ring; the `focus-spark` frame
/// arrives by Tab and shows the 2px ring. Synthetic fixture names only.
/// `TOP_BAR_CAPTURE_DIR=<dir> cargo test -p coincube-gui --lib
/// side_panel_capture -- --ignored`.
#[test]
#[ignore = "writes PNG frames; needs a GPU adapter"]
fn side_panel_capture() {
    use crate::app::menu::{CubeSettingsOption, SparkSubMenu};
    let Some(directory) = directory() else {
        return;
    };
    let mut renderer = renderer();
    for (appearance, palette) in themes() {
        let mut cache = cache(appearance == "light", BalancePrivacy::Shown);
        cache.wallet_names = crate::app::cache::WalletNames {
            spark: Some("Daily spending".into()),
            vault: Some("Family savings".into()),
        };
        let pages = [
            ("overview", Menu::Cube(CubeSubMenu::Overview)),
            ("send", Menu::Spark(SparkSubMenu::Send)),
            (
                "settings",
                Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::General)),
            ),
        ];
        for (page, menu) in &pages {
            for (width, height) in WINDOWS {
                save(
                    &mut renderer,
                    crate::app::view::dashboard(
                        menu,
                        &cache,
                        iced::widget::column![iced::widget::text("Synthetic page").size(24)],
                    ),
                    Size::new(width, height),
                    &palette,
                    None,
                    directory.join(format!(
                        "side-panel-{appearance}-{page}-{width}x{height}.png"
                    )),
                );
            }
        }
        let (_, send) = &pages[1];
        save(
            &mut renderer,
            crate::app::view::dashboard(
                send,
                &cache,
                iced::widget::column![iced::widget::text("Synthetic page").size(24)],
            ),
            Size::new(1024.0, 768.0),
            &palette,
            Some(Id::new("tenshu.shell.nav.Spark")),
            directory.join(format!("side-panel-{appearance}-focus-spark-1024x768.png")),
        );
    }
}
