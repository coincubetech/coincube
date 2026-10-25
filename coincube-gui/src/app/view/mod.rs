mod message;

pub mod buysell;
pub mod connect;
pub mod duress;
pub mod global_home;
pub mod liquid;
pub mod nav;
pub mod p2p;
pub mod preferences;
pub mod settings;
pub mod shared;
pub mod spark;
pub mod wallet_header;

pub mod vault;

use std::iter::FromIterator;

pub use liquid::*;
pub use message::*;
pub use spark::{
    SparkOverviewMessage, SparkOverviewView, SparkReceiveMessage, SparkReceiveView,
    SparkSendMessage, SparkSendView, SparkSettingsMessage, SparkSettingsStatus, SparkSettingsView,
    SparkStatus, SparkTransactionsMessage, SparkTransactionsStatus, SparkTransactionsView,
};
pub use vault::fiat::FiatAmountConverter;
pub use vault::warning::warn;

use iced::{
    widget::{column, container, row, scrollable, Space},
    Alignment, Length,
};

use coincube_ui::component::text::Text;
use coincube_ui::{
    color,
    component::{button, text, text::*},
    icon::cross_icon,
    theme,
    widget::*,
};

use crate::app::{
    cache::{Cache, SparkNotice},
    menu::Menu,
};

/// Simple toast notification for clipboard copy and other success messages
pub fn simple_toast(message: &str) -> Container<Message> {
    container(text::p2_regular(message))
        .padding(15)
        .style(theme::notification::success)
        .max_width(400.0)
}

/// Wraps `content` in the shared balance card style used across wallet overview and send screens
/// (themed card background, orange border, rounded corners).
pub fn balance_header_card<'a, Msg: 'a>(content: impl Into<Element<'a, Msg>>) -> Element<'a, Msg> {
    container(content)
        .padding(20)
        .width(Length::Fill)
        .style(theme::container::balance_header)
        .into()
}

/// The user-facing sentence for a [`SparkNotice`]. Split out so the copy
/// is testable without rendering.
pub fn spark_notice_text(notice: &SparkNotice) -> String {
    match notice {
        SparkNotice::StableBalancePaused { reason } => format!(
            "Stable Balance was paused: converting bitcoin to USDB failed {} times in a row \
             ({reason}). Your funds are unchanged. You can turn it back on in Spark settings \
             once the issue clears.",
            coincube_spark_protocol::STABLE_BALANCE_PAUSE_THRESHOLD
        ),
        SparkNotice::StableBalanceOffWithHolding => "This Spark wallet holds USDB from when \
             Stable Balance was on, but the setting is off on this device — it doesn't travel \
             with your seed. Review it in Spark settings."
            .to_string(),
    }
}

/// A warning strip for a Spark Stable Balance condition, rendered by the
/// `dashboard` wrapper when `cache.spark_notice` is set.
/// "Spark Settings" routes to the page with the
/// toggle; × dismisses for the session.
pub fn spark_notice_banner(notice: &SparkNotice) -> Element<'_, Message> {
    let body = container(
        row![
            coincube_ui::icon::warning_icon().color(color::BLACK),
            // `Fill`, not the default `Shrink`: a shrink text is laid out
            // first with the whole row available, wraps into all of it, and
            // pushes the buttons off the edge. A fill child is sized after
            // the fixed ones, so it wraps within what's actually left.
            text::p2_regular(spark_notice_text(notice))
                .color(color::BLACK)
                .width(Length::Fill),
            button::secondary(None, "Spark Settings")
                .padding([6, 14])
                .width(Length::Fixed(140.0))
                .on_press(Message::Menu(Menu::Spark(
                    crate::app::menu::SparkSubMenu::Settings(None),
                ))),
            iced::widget::Button::new(
                cross_icon()
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center),
            )
            .padding([8, 10])
            .style(theme::button::secondary)
            .on_press(Message::DismissSparkNotice),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .padding([8, 16])
    .width(Length::Fill)
    .style(theme::notification::warning);

    container(row![
        Space::new().width(Length::FillPortion(1)),
        container(body)
            .width(Length::FillPortion(8))
            .max_width(1500),
        Space::new().width(Length::FillPortion(1)),
    ])
    .padding([8, 0])
    .width(Length::Fill)
    .into()
}

pub fn dashboard<'a, T: Into<Element<'a, Message>>>(
    menu: &'a Menu,
    cache: &'a Cache,
    content: T,
) -> Element<'a, Message> {
    dashboard_with_info(
        menu,
        cache,
        content,
        &cache.cube_name,
        cache.avatar_handle.as_ref(),
        cache.lightning_address.as_deref(),
    )
}

pub fn dashboard_with_info<'a, T: Into<Element<'a, Message>>>(
    menu: &'a Menu,
    cache: &'a Cache,
    content: T,
    cube_name: &'a str,
    avatar_handle: Option<&'a iced::widget::image::Handle>,
    lightning_address: Option<&'a str>,
) -> Element<'a, Message> {
    let content_column: Element<'_, Message> = Column::new()
        .push(warn(None))
        .push_maybe(cache.spark_notice.as_ref().map(spark_notice_banner))
        .push(
            Container::new(
                scrollable(row!(
                    Space::new().width(Length::FillPortion(1)),
                    column!(
                        Space::new().height(Length::Fixed(30.0)),
                        content.into(),
                        // Symmetric bottom breathing room so the last element
                        // of any screen doesn't sit flush against the viewport
                        // edge when the content fills the height.
                        Space::new().height(Length::Fixed(30.0)),
                    )
                    .width(Length::FillPortion(8))
                    .max_width(1500),
                    Space::new().width(Length::FillPortion(1)),
                ))
                .on_scroll(|w| Message::Scroll(w.absolute_offset().y)),
            )
            .center_x(Length::Fill)
            .style(theme::container::background)
            .height(Length::Fill),
        )
        .width(Length::Fill)
        .into();

    dashboard_shell_with_info(
        menu,
        cache,
        content_column,
        cube_name,
        avatar_handle,
        lightning_address,
    )
}

/// Use the same shell for panels with their own scroll handling, such as P2P
/// chat. `body` owns its content and warnings; the shell owns all navigation.
pub fn dashboard_shell<'a, T: Into<Element<'a, Message>>>(
    menu: &'a Menu,
    cache: &'a Cache,
    body: T,
) -> Element<'a, Message> {
    dashboard_shell_with_info(
        menu,
        cache,
        body.into(),
        &cache.cube_name,
        cache.avatar_handle.as_ref(),
        cache.lightning_address.as_deref(),
    )
}

fn dashboard_shell_with_info<'a>(
    menu: &'a Menu,
    cache: &'a Cache,
    content_column: Element<'a, Message>,
    cube_name: &'a str,
    avatar_handle: Option<&'a iced::widget::image::Handle>,
    lightning_address: Option<&'a str>,
) -> Element<'a, Message> {
    // Passkey Cubes used to be exempt from this banner, because there was no
    // way for one to back its seed up. There is now (**I11**), so an
    // un-backed-up passkey Cube gets the same nudge as any other — it is the
    // Cube that most needs it.
    let show_backup_warning = !cache.current_cube_backed_up;
    let nav_ctx = nav::NavContext {
        has_vault: cache.has_vault,
        has_imported_bitcoin: cache.has_imported_bitcoin,
        has_p2p: cache.has_p2p,
        network: cache.chain(),
        p2p_test_coordinator: cache.p2p_test_coordinator,
        marketplace_flags: cache.marketplace_flags,
        liquid_gate: cache.liquid_gate,
        btcb2_server_enabled: cache.btcb2_server_enabled,
        btcb2_already_claimed: cache.btcb2_already_claimed,
        btcb2_claim_resume: cache.btcb2_claim_resume,
        cube_name,
        lightning_address,
        avatar: avatar_handle,
        theme_mode: cache.theme_mode,
        connect_authenticated: cache.sidebar_connect_signed_in,
        connect_email: cache.connect_email.as_deref(),
        connect_stream_status: &cache.connect_stream_status,
    };

    let shell_state = nav::shell::ShellState {
        switcher_open: cache.cube_switcher_open,
        lock_confirmation: cache.cube_lock_confirmation,
        // Menu entry opens Tools in App::update. Honor an explicit close even
        // while a Tools child remains selected; the content route is retained.
        tools_open: cache.vault_tools_open,
        backup_attention: show_backup_warning,
    };
    Column::new()
        .push(nav::shell::top_bar_with_privacy(
            &nav_ctx,
            cache.balance_privacy,
        ))
        .push(nav::split::ShellSplit::new(
            nav::shell::sidebar(menu, &nav_ctx, shell_state),
            content_column,
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

pub fn modal<'a, T: Into<Element<'a, Message>>, F: Into<Element<'a, Message>>>(
    is_previous: bool,
    content: T,
    fixed_footer: Option<F>,
) -> Element<'a, Message> {
    Column::new()
        .push(
            Container::new(
                Row::new()
                    .push(if is_previous {
                        Column::new()
                            .push(
                                button::secondary(None, "< Back")
                                    .width(Length::Fixed(150.0))
                                    .on_press(Message::Previous),
                            )
                            .width(Length::Fill)
                    } else {
                        Column::new().width(Length::Fill)
                    })
                    .align_y(iced::Alignment::Center)
                    .push(button::secondary(Some(cross_icon()), "Close").on_press(Message::Close)),
            )
            .padding(10)
            .style(theme::container::background),
        )
        .push(modal_section(Container::new(scrollable(content))))
        .push(fixed_footer)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn modal_section<'a, T: 'a>(menu: Container<'a, T>) -> Container<'a, T> {
    Container::new(menu.max_width(1500))
        .style(theme::container::background)
        .center_x(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
}

pub fn placeholder<'a, T: Into<Element<'a, Message>>>(
    icon: T,
    title: &'a str,
    subtitle: &'a str,
) -> Element<'a, Message> {
    let content = Column::new()
        .push(icon)
        .push(text(title).style(theme::text::secondary).bold())
        .push(
            text(subtitle)
                .size(P2_SIZE)
                .style(theme::text::secondary)
                .align_x(Alignment::Center),
        )
        .spacing(16)
        .align_x(Alignment::Center);

    Container::new(content)
        .width(Length::Fill)
        .padding(60)
        .center_x(Length::Fill)
        .style(|t| container::Style {
            background: Some(iced::Background::Color(t.colors.cards.simple.background)),
            border: iced::Border {
                radius: 20.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

/// Full-panel placeholder for a route whose feature isn't available on the
/// current network — e.g. a restored or deep-linked route onto a rail item
/// that renders greyed out. Mirrors [`placeholder`] but owns its subtitle so
/// it can be built from a transient reason string (see
/// [`crate::app::features::route_availability`]).
pub fn feature_unavailable_panel(reason: String) -> Element<'static, Message> {
    let content = Column::new()
        .push(
            coincube_ui::icon::plug_icon()
                .size(48)
                .style(theme::text::secondary),
        )
        .push(
            text("Not available on this network")
                .style(theme::text::secondary)
                .bold(),
        )
        .push(
            text(reason)
                .size(P2_SIZE)
                .style(theme::text::secondary)
                .align_x(Alignment::Center),
        )
        .spacing(16)
        .align_x(Alignment::Center);

    Container::new(content)
        .width(Length::Fill)
        .padding(60)
        .center_x(Length::Fill)
        .style(|t| container::Style {
            background: Some(iced::Background::Color(t.colors.cards.simple.background)),
            border: iced::Border {
                radius: 20.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

/// Loading-state card mirroring [`placeholder`]'s layout, but with the
/// app's pulsing-dots `loading_indicator` (the same one shown while a Cube
/// loads) below the title instead of a subtitle. Shown while an initial
/// fetch is in flight so panels don't briefly flash an empty
/// "No transactions" state before data arrives.
pub fn loading_placeholder<'a, T: Into<Element<'a, Message>>>(
    icon: T,
    title: &'a str,
) -> Element<'a, Message> {
    let content = Column::new()
        .push(icon)
        .push(text(title).style(theme::text::secondary).bold())
        .push(crate::loading::loading_indicator::<Message>(None))
        .spacing(16)
        .align_x(Alignment::Center);

    Container::new(content)
        .width(Length::Fill)
        .padding(60)
        .center_x(Length::Fill)
        .style(|t| container::Style {
            background: Some(iced::Background::Color(t.colors.cards.simple.background)),
            border: iced::Border {
                radius: 20.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

pub fn toast_overlay<'a, I: Iterator<Item = (usize, log::Level, String)>>(
    iter: I,
    theme: &coincube_ui::theme::Theme,
) -> coincube_ui::widget::Element<'a, Message> {
    use coincube_ui::{color, component::text, icon::cross_icon, theme::notification};

    // Color mapping for toast levels using the theme
    let toast = |id: usize, level: log::Level, content: String| {
        const WIDGET_HEIGHT: u32 = 80;

        // Use theme palette for the toast background
        let palette = notification::palette_for_level(&level, theme);
        let bg_color = palette.background;
        let border_color = palette.border.unwrap_or(palette.background);
        let text_color = palette.text.unwrap_or(color::WHITE);

        let bg = iced::Background::Color(bg_color);
        let border = iced::Border {
            width: 1.0,
            color: border_color,
            radius: 25.0.into(),
        };

        let inner = iced::widget::row![
            container(text::p1_bold(content).color(text_color))
                .width(600)
                .height(WIDGET_HEIGHT)
                .padding(15)
                .align_y(iced::Alignment::Center),
            iced::widget::Button::new(
                cross_icon()
                    .color(text_color)
                    .size(36)
                    .align_x(iced::Alignment::Center)
                    .align_y(iced::Alignment::Center)
                    .height(iced::Length::Fill)
            )
            .height(WIDGET_HEIGHT)
            .width(60)
            .style(move |_, status| {
                let base = iced::widget::button::Style::default();
                match status {
                    iced::widget::button::Status::Hovered => base.with_background(iced::Color {
                        a: 0.2,
                        ..color::BLACK
                    }),
                    _ => base,
                }
            })
            .on_press(Message::DismissToast(id))
        ];

        // Wrap the entire row in a single styled container so the close
        // button sits inside the rounded rectangle. clip(true) ensures
        // the hover highlight respects the border radius.
        container(inner)
            .style(move |_| {
                iced::widget::container::Style::default()
                    .background(bg)
                    .border(border)
            })
            .clip(true)
    };

    let centered = iced::widget::row![
        // Offset notifications past the full-width Cube shell.
        iced::widget::Space::new().width(nav::shell::FULL_WIDTH),
        // center toasts horizontally
        iced::widget::Space::new().width(iced::Length::Fill),
        iced::widget::Column::from_iter(
            iter.map(|(id, level, content)| toast(id, level, content).into())
        )
        .spacing(10),
        iced::widget::Space::new().width(iced::Length::Fill),
    ];

    // full screen positioning
    let column = iced::widget::column![
        iced::widget::Space::new().height(iced::Length::Fill),
        centered,
        iced::widget::Space::new().height(25),
    ];

    container(column)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}
