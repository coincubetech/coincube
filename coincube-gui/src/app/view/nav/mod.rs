//! Cube navigation. [`shell`] paints the single text sidebar and top bar on
//! every live Cube screen. The older rail modules still provide route/item
//! inventories while screen families migrate; their rail renderers are not
//! mounted by the current dashboard or P2P panels.

pub mod items;
pub mod primary;
pub mod secondary;
pub mod shell;
pub mod split;

pub mod cube;
pub mod liquid;
pub mod marketplace;
pub mod spark;
pub mod tertiary;
pub mod vault;

pub use items::SubItem;

use crate::app::{menu::Menu, view::Message};
use coincube_ui::{
    color,
    component::text,
    icon::cube_icon,
    image::{tenshu_wordmark, theme_toggle_button},
    theme,
    widget::{Button, Column, ColumnExt, Element, Row},
};
use iced::{
    widget::{column, container, row, scrollable, text::Wrapping, Space},
    Alignment, Length,
};

/// Width of the primary + secondary rails. The tertiary rail is an
/// overlay on the content area, so the sidebar column itself is always
/// this width. Used for the wordmark / identity / toggle bands and for
/// the toast offset.
pub const SIDEBAR_BASE_WIDTH: f32 = primary::RAIL_WIDTH + secondary::RAIL_WIDTH;

/// Height of the logo header band that caps both rails. Tall enough for the
/// stacked Tenshu lockup (mark ~2× the label + the "Tenshu" label).
const WORDMARK_BAND_HEIGHT: f32 = 60.0;
/// Label font size for the Tenshu lockup in the compact two-rail header.
const WORDMARK_SIZE: f32 = 14.0;
/// Fixed height for the identity block (avatar + cube name + LN
/// address). Fixed so the tertiary overlay's top offset is deterministic.
const IDENTITY_BLOCK_HEIGHT: f32 = 130.0;

/// Vertical distance from the top of the sidebar column to the top of
/// the rails row (primary + secondary). The tertiary overlay uses this
/// to position itself so its items align horizontally with the rails.
pub const TERTIARY_TOP_OFFSET: f32 = WORDMARK_BAND_HEIGHT + IDENTITY_BLOCK_HEIGHT;

/// Composed sidebar: wordmark · identity block · two rails · theme toggle.
///
/// Layout (top to bottom):
/// ```text
/// [ Tenshu logo (mark + label)    ]  full 144px
/// [ avatar + cube name + LN addr  ]  full 144px — pushes both rails down
/// [ primary (72px) | secondary (72px) ]  fills the remaining height
/// [ dark/light toggle             ]  full 144px
/// ```
///
/// The two rails sit inside a single scrollable that takes all the height
/// left between the identity block and the toggle. When the rails are
/// taller than that (short windows, many submenu items) they scroll
/// together rather than pushing the toggle off-screen or being clipped.
/// The rails themselves are content-sized and unstyled; the surrounding
/// container paints the shared `sidebar_primary` background so it covers
/// the whole band even when the rails are shorter than the viewport.
pub fn sidebar<'a>(menu: &Menu, ctx: &NavContext<'a>) -> Element<'a, Message> {
    // Base sidebar is always 144px wide (primary + secondary rails only).
    // The tertiary rail lives as an overlay on the content area — see
    // [`tertiary_rail`] and `dashboard_with_info` — so content never
    // shifts horizontally when the tertiary rail slides out.
    let rails_row: Row<Message> = row![primary::rail(menu, ctx), secondary::rail(menu, ctx)]
        .width(Length::Fixed(SIDEBAR_BASE_WIDTH));
    let rails = container(
        scrollable(rails_row)
            .width(Length::Fixed(SIDEBAR_BASE_WIDTH))
            .height(Length::Fill),
    )
    .width(Length::Fixed(SIDEBAR_BASE_WIDTH))
    .height(Length::Fill)
    .style(theme::container::sidebar_primary);

    let wordmark = container(tenshu_wordmark(WORDMARK_SIZE))
        .width(Length::Fixed(SIDEBAR_BASE_WIDTH))
        .height(Length::Fixed(WORDMARK_BAND_HEIGHT))
        .center_x(Length::Fixed(SIDEBAR_BASE_WIDTH))
        .center_y(Length::Fixed(WORDMARK_BAND_HEIGHT))
        .style(theme::container::sidebar_primary);

    let identity = identity_block(ctx);
    let toggle = theme_toggle_row(ctx);

    let col: Column<Message> = column![wordmark, identity, rails, toggle]
        .push_maybe(
            ctx.connect_authenticated
                .then(|| coincube_ui::component::session_status::indicator(false)),
        )
        .width(Length::Fixed(SIDEBAR_BASE_WIDTH))
        .height(Length::Fill)
        .align_x(Alignment::Start);
    container(col)
        .width(Length::Fixed(SIDEBAR_BASE_WIDTH))
        .height(Length::Fill)
        .style(theme::container::sidebar_primary)
        .into()
}

/// Tertiary rail for the current route, to be overlaid on the left
/// edge of the content area by `dashboard_with_info`. Returns `None`
/// when the active route has no third level.
pub fn tertiary_rail<'a>(menu: &Menu, ctx: &NavContext<'a>) -> Option<Element<'a, Message>> {
    tertiary::rail(menu, ctx)
}

/// Full-width identity header — avatar + cube name + optional Lightning
/// Address. Rendered above both rails so the top of each rail is pushed
/// down equally (the first secondary-rail item lines up with the `Home`
/// / `Cube` item on the primary rail).
fn identity_block<'a>(ctx: &NavContext<'a>) -> Element<'a, Message> {
    const AVATAR_SIZE: f32 = 56.0;

    let avatar: Element<'a, Message> = if let Some(handle) = ctx.avatar {
        iced::widget::image(handle.clone())
            .width(Length::Fixed(AVATAR_SIZE))
            .height(Length::Fixed(AVATAR_SIZE))
            .into()
    } else {
        container(cube_icon().size(28).color(color::GREY_3))
            .width(Length::Fixed(AVATAR_SIZE))
            .height(Length::Fixed(AVATAR_SIZE))
            .center_x(Length::Fixed(AVATAR_SIZE))
            .center_y(Length::Fixed(AVATAR_SIZE))
            .style(
                |t: &coincube_ui::theme::Theme| iced::widget::container::Style {
                    background: Some(iced::Background::Color(t.colors.cards.simple.background)),
                    border: iced::Border {
                        radius: (AVATAR_SIZE / 2.0).into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .into()
    };

    // The avatar is managed on Cube → Settings → Avatar, so clicking it
    // jumps straight there (rather than Cube → Overview, where there's
    // nothing to do with it).
    let avatar_button: Element<'a, Message> = Button::new(avatar)
        .style(theme::button::transparent)
        .on_press(Message::Menu(Menu::Cube(
            crate::app::menu::CubeSubMenu::Settings(crate::app::menu::CubeSettingsOption::Avatar),
        )))
        .into();

    let mut col: Column<Message> = Column::new()
        .padding([12, 8])
        .spacing(4)
        .align_x(Alignment::Center)
        .width(Length::Fixed(SIDEBAR_BASE_WIDTH))
        .push(avatar_button);

    if !ctx.cube_name.is_empty() {
        // Cube name + (optional) Connect status dot. We render the dot
        // inline with the name rather than on its own row so the
        // header height doesn't grow for users on local-daemon cubes.
        let name = text::p2_bold(ctx.cube_name)
            .style(theme::text::primary)
            .align_x(Alignment::Center);
        col = col.push(
            row![name, connect_status_dot(ctx.connect_stream_status),]
                .spacing(4)
                .align_y(Alignment::Center),
        );
    }

    if let Some(addr) = ctx.lightning_address {
        let display_addr = if addr.contains('@') {
            addr.to_string()
        } else {
            format!("{}@coincube.io", addr)
        };
        let label = text::caption(display_addr.clone())
            .color(color::GREY_3)
            .width(Length::Fill)
            .wrapping(Wrapping::WordOrGlyph);
        let btn: Button<Message> = Button::new(label)
            .width(Length::Fill)
            .style(theme::button::transparent)
            .on_press(Message::Clipboard(display_addr));

        col = col.push(btn);
    }

    container(col)
        .width(Length::Fixed(SIDEBAR_BASE_WIDTH))
        .height(Length::Fixed(IDENTITY_BLOCK_HEIGHT))
        .style(theme::container::sidebar_primary)
        .into()
}

/// Dark/light toggle anchored at the bottom of the sidebar, spanning
/// both rails. Mirrors the placement in the pre-refactor sidebar.
fn theme_toggle_row<'a>(ctx: &NavContext<'a>) -> Element<'a, Message> {
    let toggle = theme_toggle_button(ctx.theme_mode, Message::ToggleTheme);
    container(toggle)
        .width(Length::Fixed(SIDEBAR_BASE_WIDTH))
        .padding(iced::Padding {
            top: 4.0,
            right: 8.0,
            bottom: 16.0,
            left: 8.0,
        })
        .style(theme::container::sidebar_primary)
        .into()
}

/// Small 8px colored dot showing the Connect realtime-stream health.
/// `Inactive` collapses to an empty `Space` so users without Connect
/// (local-daemon installs, fresh launches before `ConnectStreamReady`)
/// don't see a confusing red dot in their sidebar.
fn connect_status_dot<'a>(status: &'a crate::app::ConnectionStatus) -> Element<'a, Message> {
    use iced::widget::{container, tooltip as iced_tooltip};
    if !status.is_visible() {
        return Space::new().width(Length::Fixed(0.0)).into();
    }
    let dot_color = match status {
        crate::app::ConnectionStatus::Connected => color::GREEN,
        crate::app::ConnectionStatus::Connecting => color::ORANGE,
        crate::app::ConnectionStatus::Error(_) => color::RED,
        // Unreachable due to the early-return above, but keep the
        // match exhaustive so adding a new variant produces a compile
        // error instead of a silent default.
        crate::app::ConnectionStatus::Inactive => color::GREY_3,
    };
    let dot = container(
        Space::new()
            .width(Length::Fixed(0.0))
            .height(Length::Fixed(0.0)),
    )
    .width(Length::Fixed(8.0))
    .height(Length::Fixed(8.0))
    .style(
        move |_t: &coincube_ui::theme::Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(dot_color)),
            border: iced::Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    iced_tooltip(
        dot,
        coincube_ui::component::text::caption(status.tooltip()),
        iced_tooltip::Position::Bottom,
    )
    .into()
}

/// Ambient data both rails need while rendering.
pub struct NavContext<'a> {
    pub has_vault: bool,
    pub has_imported_bitcoin: bool,
    pub has_p2p: bool,
    /// Active chain identity. Drives per-feature availability gating
    /// (Spark/Liquid on the primary rail, Buy/Sell + P2P in Marketplace)
    /// via [`crate::app::features`].
    pub network: crate::chain::ChainId,
    /// Whether a non-mainnet Mostro coordinator is configured. Only
    /// affects P2P on test networks (mainnet P2P is always available).
    pub p2p_test_coordinator: bool,
    /// Server-controlled Marketplace availability (from `/connect/features`).
    /// Gates whether the Marketplace top-level nav and its Buy/Sell + P2P
    /// sub-items are shown at all — on top of the per-network gate. Fail-closed
    /// [`MarketplaceServerFlags::OFF`](crate::app::features::MarketplaceServerFlags::OFF)
    /// until features load, hiding the section while the API stance is unknown.
    pub marketplace_flags: crate::app::features::MarketplaceServerFlags,
    /// Whether the Liquid wallet is shown at all (sunset gate). When this is
    /// hidden the Liquid top-level entry disappears from the primary rail —
    /// hidden, not greyed, since "not on this account" isn't a per-network
    /// state. See [`crate::app::features::LiquidGate`].
    pub liquid_gate: crate::app::features::LiquidGate,
    pub cube_name: &'a str,
    pub lightning_address: Option<&'a str>,
    pub avatar: Option<&'a iced::widget::image::Handle>,
    pub theme_mode: coincube_ui::theme::palette::ThemeMode,
    pub connect_authenticated: bool,
    pub connect_email: Option<&'a str>,
    /// Latest realtime-stream health for the small status dot rendered
    /// under the cube name in [`identity_block`]. `Inactive` renders
    /// no dot (e.g. local-daemon installs without Connect).
    pub connect_stream_status: &'a crate::app::ConnectionStatus,
    /// Account-scoped Bitcoin Blake2b grant, mirrored from `/connect/features`
    /// into [`crate::app::cache::Cache::btcb2_server_enabled`].
    pub btcb2_server_enabled: bool,
    /// Whether a Bitcoin Blake2b Cube already reuses this Cube's descriptor.
    pub btcb2_already_claimed: bool,
    pub btcb2_claim_resume: bool,
}

impl NavContext<'_> {
    /// This Cube as a candidate Bitcoin Blake2b claim source.
    pub fn claim_source_cube(&self) -> crate::app::features::ClaimSourceCube {
        crate::app::features::ClaimSourceCube {
            chain: self.network,
            has_vault: self.has_vault,
            server_enabled: self.btcb2_server_enabled,
            already_claimed: self.btcb2_already_claimed,
        }
    }
}

#[cfg(test)]
mod shell_tests {
    use super::*;
    use iced::advanced::{
        layout,
        renderer::Headless,
        widget::{Id, Operation, Tree},
        Layout,
    };

    #[derive(Default)]
    struct Labels(Vec<(String, iced::Rectangle)>);

    impl Operation for Labels {
        fn traverse(&mut self, f: &mut dyn FnMut(&mut dyn Operation)) {
            f(self);
        }

        fn text(&mut self, _: Option<&Id>, bounds: iced::Rectangle, value: &str) {
            self.0.push((value.to_owned(), bounds));
        }
    }

    fn load_fonts() {
        for bytes in [
            coincube_ui::font::REGULAR_BYTES,
            coincube_ui::font::MEDIUM_BYTES,
            coincube_ui::font::SEMIBOLD_BYTES,
            coincube_ui::font::BOOTSTRAP_ICONS_BYTE,
        ] {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(std::borrow::Cow::Borrowed(bytes));
        }
    }

    fn redraw(
        element: &mut coincube_ui::widget::Element<'_, Message>,
        tree: &mut Tree,
        node: &layout::Node,
        renderer: &iced::Renderer,
        size: iced::Size,
    ) {
        let mut messages = Vec::new();
        element.as_widget_mut().update(
            tree,
            &iced::Event::Window(iced::window::Event::RedrawRequested(
                std::time::Instant::now(),
            )),
            Layout::new(node),
            iced::mouse::Cursor::Unavailable,
            renderer,
            &mut iced::advanced::clipboard::Null,
            &mut iced::advanced::Shell::new(&mut messages),
            &iced::Rectangle::with_size(size),
        );
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn single_sidebar_has_anchored_actions_at_both_minimum_and_full_sizes() {
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
            for (width, height, expected_sidebar) in [(1440, 900, 248.0), (1024, 768, 208.0)] {
                let cache = crate::app::cache::Cache {
                    cube_name: "Everyday".into(),
                    has_vault: true,
                    backup_warning_dismissed: false,
                    theme_mode: mode,
                    ..Default::default()
                };
                let menu = Menu::Cube(crate::app::menu::CubeSubMenu::Overview);
                let fixture = iced::widget::column![
                    iced::widget::text("Total balance").size(24),
                    iced::widget::text("Synthetic shell preview").size(14),
                ];
                let mut element = crate::app::view::dashboard(&menu, &cache, fixture);
                let mut renderer = <iced::Renderer as Headless>::new(
                    coincube_ui::font::REGULAR,
                    iced::Pixels(14.0),
                    Some("tiny-skia"),
                )
                .await
                .unwrap();
                let mut tree = Tree::new(element.as_widget());
                let size = iced::Size::new(width as f32, height as f32);
                let node = element.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(iced::Size::ZERO, size),
                );
                let body = &node.children()[1];
                let sidebar = &body.children()[0];
                assert!(
                    (sidebar.bounds().width - expected_sidebar).abs() <= 1.0,
                    "sidebar width at {width}: {}",
                    sidebar.bounds().width
                );
                let mut labels = Labels::default();
                element.as_widget_mut().operate(
                    &mut tree,
                    Layout::new(&node),
                    &renderer,
                    &mut labels,
                );
                for required in [
                    "Tenshu",
                    "Everyday",
                    "Overview",
                    "Activity",
                    "Backup & protection",
                    "Tools",
                    "Settings",
                ] {
                    assert!(
                        labels.0.iter().any(|(label, _)| label == required),
                        "missing {required} at {width}",
                        required = required,
                        width = width
                    );
                }
                let settings = labels
                    .0
                    .iter()
                    .find(|(label, _)| label == "Settings")
                    .unwrap()
                    .1;
                assert!(
                    settings.y > height as f32 * 0.70,
                    "Settings must stay anchored at the foot"
                );
                let brand = labels
                    .0
                    .iter()
                    .find(|(label, _)| label == "Tenshu")
                    .unwrap()
                    .1;
                let mut messages = Vec::new();
                let cursor = iced::mouse::Cursor::Available(brand.center());
                for event in [
                    iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left),
                    iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left),
                ] {
                    element.as_widget_mut().update(
                        &mut tree,
                        &iced::Event::Mouse(event),
                        Layout::new(&node),
                        cursor,
                        &renderer,
                        &mut iced::advanced::clipboard::Null,
                        &mut iced::advanced::Shell::new(&mut messages),
                        &iced::Rectangle::with_size(size),
                    );
                }
                assert!(matches!(messages.as_slice(), [Message::OpenLauncher]));
                if let Ok(directory) = std::env::var("SIDEBAR_PREVIEW_DIR") {
                    redraw(&mut element, &mut tree, &node, &renderer, size);
                    std::fs::create_dir_all(&directory).unwrap();
                    element.as_widget().draw(
                        &tree,
                        &mut renderer,
                        &theme,
                        &iced::advanced::renderer::Style {
                            text_color: theme.colors.shell.ink,
                        },
                        Layout::new(&node),
                        iced::mouse::Cursor::Unavailable,
                        &iced::Rectangle::with_size(size),
                    );
                    let pixels = renderer.screenshot(
                        iced::Size::new(width, height),
                        1.0,
                        theme.colors.shell.background,
                    );
                    image::save_buffer(
                        std::path::Path::new(&directory)
                            .join(format!("tenshu-shell-{mode:?}-{width}x{height}.png")),
                        &pixels,
                        width,
                        height,
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                }
            }
        }
    }

    #[tokio::test]
    async fn long_name_switcher_and_lock_review_fit_compact_shell() {
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
            for (confirmation, state_name) in [(false, "switcher"), (true, "lock-review")] {
                let cache = crate::app::cache::Cache {
                    cube_name:
                        "Family household multi-signature treasury for education and retirement"
                            .into(),
                    has_vault: true,
                    cube_switcher_open: true,
                    cube_lock_confirmation: confirmation,
                    backup_warning_dismissed: false,
                    theme_mode: mode,
                    ..Default::default()
                };
                let menu = Menu::Cube(crate::app::menu::CubeSubMenu::Overview);
                let mut element = crate::app::view::dashboard(
                    &menu,
                    &cache,
                    iced::widget::text("Synthetic shell preview"),
                );
                let mut renderer = <iced::Renderer as Headless>::new(
                    coincube_ui::font::REGULAR,
                    iced::Pixels(14.0),
                    Some("tiny-skia"),
                )
                .await
                .unwrap();
                let mut tree = Tree::new(element.as_widget());
                let size = iced::Size::new(1024.0, 768.0);
                let node = element.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(iced::Size::ZERO, size),
                );
                assert_eq!(node.children()[1].children()[0].bounds().width, 208.0);
                let mut labels = Labels::default();
                element.as_widget_mut().operate(
                    &mut tree,
                    Layout::new(&node),
                    &renderer,
                    &mut labels,
                );
                for required in ["All Cubes", "Settings", "Cancel"] {
                    if required == "Cancel" && !confirmation {
                        continue;
                    }
                    assert!(labels.0.iter().any(|(label, _)| label == required));
                }
                assert!(labels.0.iter().any(
                    |(label, _)| label.starts_with("Family household") && label.ends_with('…')
                ));
                if confirmation {
                    assert!(labels.0.iter().any(|(label, _)| label == "Lock"));
                } else {
                    assert!(labels.0.iter().any(|(label, _)| label == "Lock Cube"));
                }
                if let Ok(directory) = std::env::var("SIDEBAR_PREVIEW_DIR") {
                    redraw(&mut element, &mut tree, &node, &renderer, size);
                    std::fs::create_dir_all(&directory).unwrap();
                    element.as_widget().draw(
                        &tree,
                        &mut renderer,
                        &theme,
                        &iced::advanced::renderer::Style {
                            text_color: theme.colors.shell.ink,
                        },
                        Layout::new(&node),
                        iced::mouse::Cursor::Unavailable,
                        &iced::Rectangle::with_size(size),
                    );
                    let pixels = renderer.screenshot(
                        iced::Size::new(1024, 768),
                        1.0,
                        theme.colors.shell.background,
                    );
                    image::save_buffer(
                        std::path::Path::new(&directory)
                            .join(format!("tenshu-shell-{mode:?}-long-name-{state_name}.png")),
                        &pixels,
                        1024,
                        768,
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                }
            }
        }
    }
}
