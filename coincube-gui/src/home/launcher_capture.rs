//! Offscreen frames of the real Launcher view through the production wgpu
//! renderer, for side-by-side review against the prototype. Opt-in only:
//! `LAUNCHER_CAPTURE_DIR=<dir> cargo test -p coincube-gui --lib
//! launcher_capture -- --ignored`. All Cubes are synthetic fixtures.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use coincube_core::miniscript::bitcoin::Network;
use coincube_ui::{font, theme, widget::Element};
use iced::{
    advanced::{layout, mouse, renderer::Headless, widget::Tree, Layout, Renderer as _, Shell},
    Rectangle, Size,
};

use super::{
    launcher_catalog::LauncherMode, launcher_presenter, launcher_scene::Motion, CubeCatalogKey,
    Home, State,
};
use crate::{app::settings::CubeSettings, chain::ChainId, dir::CoincubeDirectory};

fn renderer() -> Option<iced::Renderer> {
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
}

fn save<M>(
    renderer: &mut iced::Renderer,
    element: Element<'_, M>,
    size: Size,
    theme: &theme::Theme,
    path: PathBuf,
) {
    save_scrolled(renderer, element, size, theme, path, None);
}

/// As `save`, after snapping the scrollable `scroll` to its end.
fn save_scrolled<M>(
    renderer: &mut iced::Renderer,
    mut element: Element<'_, M>,
    size: Size,
    theme: &theme::Theme,
    path: PathBuf,
    scroll: Option<iced::advanced::widget::Id>,
) {
    let mut tree = Tree::new(element.as_widget());
    let node =
        element
            .as_widget_mut()
            .layout(&mut tree, renderer, &layout::Limits::new(Size::ZERO, size));
    if let Some(id) = scroll {
        let mut snap = iced::advanced::widget::operation::scrollable::snap_to(
            id,
            iced::widget::scrollable::RelativeOffset {
                x: None,
                y: Some(1.0),
            },
        );
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), renderer, &mut snap);
    }
    let viewport = Rectangle::with_size(size);
    let mut messages = Vec::new();
    element.as_widget_mut().update(
        &mut tree,
        &iced::Event::Window(iced::window::Event::RedrawRequested(Instant::now())),
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        renderer,
        &mut iced::advanced::clipboard::Null,
        &mut Shell::new(&mut messages),
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

fn home(light: bool) -> Home {
    let mut home = Home::new(
        CoincubeDirectory::new(PathBuf::new()),
        Some(Network::Bitcoin),
    )
    .0;
    home.network = ChainId::Bitcoin;
    let names = [
        "Everyday",
        "Archive",
        "Household 2024",
        "Travel fund",
        "Studio",
        "Lab",
    ];
    let mut cubes: Vec<CubeSettings> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            CubeSettings::new_with_raw_id(format!("cube-{i}"), (*name).into(), ChainId::Bitcoin)
        })
        .collect();
    cubes[0].set_vault(crate::app::settings::VaultIdentity {
        wallet_id: crate::app::settings::WalletId::new("fixture".into(), Some(1)),
        fingerprint: Some("12345678".into()),
    });
    cubes[2].backed_up = true;
    cubes[2].set_vault(crate::app::settings::VaultIdentity {
        wallet_id: crate::app::settings::WalletId::new("fixture2".into(), Some(1)),
        fingerprint: Some("87654321".into()),
    });
    home.state = State::Cubes {
        cubes,
        create_cube: false,
        source: ChainId::Bitcoin,
    };
    home.advisory_notice = None;
    home.catalog.mode = LauncherMode::Cubes;
    home.theme_mode = if light {
        coincube_ui::theme::palette::ThemeMode::Light
    } else {
        coincube_ui::theme::palette::ThemeMode::Dark
    };
    home
}

fn key(i: usize) -> CubeCatalogKey {
    CubeCatalogKey {
        chain: ChainId::Bitcoin,
        cube_id: format!("cube-{i}"),
    }
}

#[test]
#[ignore = "writes PNG frames; needs a GPU adapter"]
fn launcher_capture() {
    let Ok(directory) = std::env::var("LAUNCHER_CAPTURE_DIR") else {
        return;
    };
    let directory = PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let mut renderer = renderer().expect("wgpu headless renderer");
    let size = Size::new(1440.0, 900.0);
    for light in [false, true] {
        let appearance = if light { "light" } else { "dark" };
        let palette = if light {
            theme::Theme::light()
        } else {
            theme::Theme::dark()
        };
        let shot = |home: &Home, name: &str, renderer: &mut iced::Renderer| {
            save(
                renderer,
                super::launcher_catalog::launcher_view(home),
                size,
                &palette,
                directory.join(format!("{appearance}-{name}.png")),
            );
        };
        let mut home = home(light);
        // The prototype captures open the second Cube (dark) and the fourth
        // Cube (light); match them so the frames compare one to one.
        let pick = if light { 3 } else { 1 };
        let other = if light { 1 } else { 3 };
        shot(&home, "01-floor", &mut renderer);
        // Browse -> details on the second Cube: every 82ms of the 820ms turn.
        home.catalog.selected = Some(key(pick));
        for step in 0..=10u64 {
            home.catalog.motion = Some(Motion::at(
                None,
                Instant::now() - Duration::from_millis(step * 82),
            ));
            shot(&home, &format!("02-spin-{:04}ms", step * 82), &mut renderer);
        }
        home.catalog.motion = None;
        shot(&home, "03-details", &mut renderer);
        // Handoff to another Cube: 160ms delay, then its turn.
        home.catalog.selected = Some(key(other));
        for ms in [0u64, 80, 160, 400, 980] {
            home.catalog.motion = Some(Motion::at(
                Some(key(pick)),
                Instant::now() - Duration::from_millis(ms),
            ));
            shot(&home, &format!("04-handoff-{ms:04}ms"), &mut renderer);
        }
        home.catalog.motion = None;
        // Back to browse.
        home.catalog.selected = None;
        for ms in [0u64, 250, 820] {
            home.catalog.motion = Some(Motion::returning_at(
                key(other),
                Instant::now() - Duration::from_millis(ms),
            ));
            shot(&home, &format!("05-return-{ms:04}ms"), &mut renderer);
        }
        home.catalog.motion = None;
        home.catalog.page = 1;
        shot(&home, "06-floor-page2", &mut renderer);
        home.catalog.page = 0;

        // A 30fps sequence for the recording: spin in, hold, hand off, return.
        if std::env::var("LAUNCHER_CAPTURE_SEQUENCE").is_ok() {
            let mut frame = 0;
            let mut seq = |home: &Home, renderer: &mut iced::Renderer| {
                shot(home, &format!("seq-{frame:04}"), renderer);
                frame += 1;
            };
            home.catalog.selected = None;
            home.catalog.motion = None;
            for _ in 0..10 {
                seq(&home, &mut renderer);
            }
            home.catalog.selected = Some(key(pick));
            for ms in (0..=840u64).step_by(33) {
                home.catalog.motion =
                    Some(Motion::at(None, Instant::now() - Duration::from_millis(ms)));
                seq(&home, &mut renderer);
            }
            home.catalog.motion = None;
            for _ in 0..20 {
                seq(&home, &mut renderer);
            }
            home.catalog.selected = Some(key(other));
            for ms in (0..=1000u64).step_by(33) {
                home.catalog.motion = Some(Motion::at(
                    Some(key(pick)),
                    Instant::now() - Duration::from_millis(ms),
                ));
                seq(&home, &mut renderer);
            }
            home.catalog.motion = None;
            for _ in 0..20 {
                seq(&home, &mut renderer);
            }
            home.catalog.selected = None;
            for ms in (0..=840u64).step_by(33) {
                home.catalog.motion = Some(Motion::returning_at(
                    key(other),
                    Instant::now() - Duration::from_millis(ms),
                ));
                seq(&home, &mut renderer);
            }
            home.catalog.motion = None;
            for _ in 0..10 {
                seq(&home, &mut renderer);
            }
        }

        // A short window scrolls the stage above the frame top: the GPU
        // picture must stay under the live details face and labels.
        let scroll = iced::advanced::widget::Id::new("launcher.capture.scroll");
        home.catalog.selected = Some(key(pick));
        home.catalog.motion = None;
        save_scrolled(
            &mut renderer,
            iced::widget::scrollable(launcher_presenter::floor_body(&home))
                .id(scroll.clone())
                .height(iced::Length::Fill)
                .into(),
            Size::new(1440.0, 560.0),
            &palette,
            directory.join(format!("{appearance}-10-details-scrolled.png")),
            Some(scroll),
        );
        home.catalog.selected = None;

        // Details -> unlock on the same floor, through the real unlock stage.
        home.catalog.selected = Some(key(pick));
        home.catalog.open_from_floor = true;
        let cube = if let State::Cubes { cubes, .. } = &home.state {
            cubes[pick].clone()
        } else {
            unreachable!()
        };
        let context = home.launcher_scene_context(&cube).expect("floor context");
        let mode = home.theme_mode;
        let steps: Vec<f32> = if std::env::var("LAUNCHER_CAPTURE_SEQUENCE").is_ok() {
            (0..=25)
                .map(|i| super::launcher_scene::ease(i as f32 / 25.0))
                .collect()
        } else {
            vec![0.0, 0.25, 0.5, 0.75]
        };
        for progress in steps {
            let pin = String::new();
            let element: Element<'_, String> = launcher_presenter::unlock_turn_with_floor_controls(
                &cube,
                mode,
                &context,
                Some(progress),
                iced::widget::text_input("", &pin).into(),
            );
            save(
                &mut renderer,
                element,
                size,
                &palette,
                directory.join(format!(
                    "{appearance}-07-unlock-turn-{:04}.png",
                    (progress * 1000.0) as u32
                )),
            );
        }
        let pin = crate::pin_input::PinInput::new();
        let face: Element<'_, crate::pin_input::Message> =
            launcher_presenter::unlock_face_on_floor(&cube, mode, &context, || pin.view());
        save(
            &mut renderer,
            face,
            size,
            &palette,
            directory.join(format!("{appearance}-08-unlock.png")),
        );
        // The unlock screens without a Launcher page behind them.
        let pin = crate::pin_input::PinInput::new();
        let lone: Element<'_, crate::pin_input::Message> =
            launcher_presenter::unlock_face(&cube, mode, pin.view());
        save(
            &mut renderer,
            lone,
            size,
            &palette,
            directory.join(format!("{appearance}-09-unlock-lone.png")),
        );
    }
}
