mod message;

pub mod activity;

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
    advanced::widget::Id,
    widget::{container, row, scrollable, Space},
    Alignment, Length,
};

use coincube_ui::component::tenshu::{
    gutter_for_window,
    page::{PAGE_BOTTOM, PAGE_TOP},
    page_head::BODY_TOP,
    PageHead, PageWidth,
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

    page(body, PageWidth::Reading).vertical(8.0, 8.0).into()
}

/// A Cube page with no head of its own. Settings and the Vault's Tools still
/// get theirs from the route (see [`dashboard_with_info_layout`]).
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

/// A Cube page under a pinned [`page_head`]: the head stays put and only
/// `content` scrolls (HANDOFF "v175 delta", "Page head").
pub fn dashboard_headed<'a, T: Into<Element<'a, Message>>>(
    menu: &'a Menu,
    cache: &'a Cache,
    head: PageHead<'a, Message>,
    content: T,
) -> Element<'a, Message> {
    dashboard_with_info_layout(
        menu,
        cache,
        Some(head),
        content,
        &cache.cube_name,
        cache.avatar_handle.as_ref(),
        cache.lightning_address.as_deref(),
        PageWidth::Reading,
    )
}

/// The Cube's coin, the mark before a page head's Cube line (`.idl > img`).
pub fn cube_coin<'a>(cache: &Cache) -> Element<'a, Message> {
    let light = matches!(
        cache.theme_mode,
        coincube_ui::theme::palette::ThemeMode::Light
    );
    iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(
        crate::home::launcher_coin::widget_source(cache.chain(), 16.0, 1.0, light).into_bytes(),
    ))
    .width(Length::Fixed(16.0))
    .height(Length::Fixed(16.0))
    .into()
}

/// "‹name› · ‹network›", the line under a page title.
pub fn network_line(cache: &Cache, name: &str) -> String {
    format!("{name} · {}", nav::shell::network_label(cache.chain()))
}

/// The Cube's name as the side panel shows it, or "Cube" while it has none.
/// Names are saved without a length limit and may hold line breaks, and a
/// pinned head doesn't scroll, so heads get the same one-line, 16-character
/// cut.
pub fn cube_display_name(cache: &Cache) -> String {
    if cache.cube_name.is_empty() {
        "Cube".to_owned()
    } else {
        nav::shell::short_cube_name(&cache.cube_name)
    }
}

/// A pinned page head: `title`, then the Cube's coin and "‹Cube› · ‹network›".
/// Add the page's actions and its back target with the [`PageHead`] builders.
pub fn page_head<'a>(
    cache: &Cache,
    title: impl iced::widget::text::IntoFragment<'a>,
) -> PageHead<'a, Message> {
    coincube_ui::component::tenshu::page_head(title, Message::FocusAction).context(
        cube_coin(cache),
        network_line(cache, &cube_display_name(cache)),
    )
}

/// A wallet page's head: "Spark · ‹nickname›" or "Vault · ‹alias›" as the
/// side panel names it, plain "Spark" or "Vault" without a name.
pub fn wallet_page_head<'a>(
    cache: &Cache,
    wallet: &str,
    name: Option<&str>,
) -> PageHead<'a, Message> {
    page_head(cache, nav::shell::wallet_label(wallet, name))
}

/// The head of a page inside a wallet (Send, Receive, its activity, a
/// transaction): `title`, then the coin and "‹wallet› · ‹Cube› · ‹network›",
/// so the page still says which wallet it belongs to (the H-Send boards).
pub fn wallet_sub_page_head<'a>(
    cache: &Cache,
    wallet: &str,
    name: Option<&str>,
    title: impl iced::widget::text::IntoFragment<'a>,
) -> PageHead<'a, Message> {
    coincube_ui::component::tenshu::page_head(title, Message::FocusAction).context(
        cube_coin(cache),
        format!(
            "{} · {}",
            nav::shell::wallet_label(wallet, name),
            network_line(cache, &cube_display_name(cache))
        ),
    )
}

/// [`wallet_sub_page_head`] for a Spark page.
pub fn spark_page_head<'a>(
    cache: &Cache,
    title: impl iced::widget::text::IntoFragment<'a>,
) -> PageHead<'a, Message> {
    wallet_sub_page_head(cache, "Spark", cache.wallet_names.spark.as_deref(), title)
}

/// [`wallet_sub_page_head`] for a Vault page.
pub fn vault_page_head<'a>(
    cache: &Cache,
    title: impl iced::widget::text::IntoFragment<'a>,
) -> PageHead<'a, Message> {
    wallet_sub_page_head(cache, "Vault", cache.wallet_names.vault.as_deref(), title)
}

/// Activity fills the content area edge to edge (`.main`) so its header and
/// filters stay put while the list and the detail pane scroll on their own;
/// `view::activity` puts them on the same centred page column as [`page`].
pub fn dashboard_activity<'a, T: Into<Element<'a, Message>>>(
    menu: &'a Menu,
    cache: &'a Cache,
    content: T,
) -> Element<'a, Message> {
    let content_column: Element<'_, Message> = Column::new()
        .push(warn(None))
        .push_maybe(cache.spark_notice.as_ref().map(spark_notice_banner))
        .push(
            Container::new(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|theme: &theme::Theme| iced::widget::container::Style {
                    background: Some(iced::Background::Color(theme.colors.shell.background)),
                    ..Default::default()
                }),
        )
        .width(Length::Fill)
        .into();
    dashboard_shell_with_info(
        menu,
        cache,
        content_column,
        &cache.cube_name,
        cache.avatar_handle.as_ref(),
        cache.lightning_address.as_deref(),
    )
}

/// Send and Receive: a reading page. Their 680px form and 240px context rail
/// sit together from the left edge at 1440px and wrap at the 1024px minimum
/// window size.
pub fn dashboard_payment<'a, T: Into<Element<'a, Message>>>(
    menu: &'a Menu,
    cache: &'a Cache,
    head: Option<PageHead<'a, Message>>,
    content: T,
) -> Element<'a, Message> {
    dashboard_with_info_layout(
        menu,
        cache,
        head,
        content,
        &cache.cube_name,
        cache.avatar_handle.as_ref(),
        cache.lightning_address.as_deref(),
        PageWidth::Reading,
    )
}

/// The Cube shell's page gutter for a content pane `pane_width` wide.
///
/// The canvas rule is `clamp(24px, 3vw, 48px)` of the *window*; the pane is
/// the window minus the side menu, whose width itself depends on the window
/// (`nav::split::sidebar_width_for_window`), so add the menu back first.
pub fn page_gutter(pane_width: f32) -> f32 {
    let with_full_menu = pane_width + nav::shell::FULL_WIDTH;
    let window = if nav::split::sidebar_width_for_window(with_full_menu) == nav::shell::FULL_WIDTH {
        with_full_menu
    } else {
        pane_width + nav::shell::COMPACT_WIDTH
    };
    gutter_for_window(window)
}

/// Every Cube screen's page container (HANDOFF "Page layout"): one column at
/// most `PAGE_MAX` wide, centred, at least [`page_gutter`] from each side.
pub fn page<'a, T: Into<Element<'a, Message>>>(
    content: T,
    width: PageWidth,
) -> coincube_ui::component::tenshu::Page<'a, Message> {
    coincube_ui::component::tenshu::page(content, width, page_gutter)
}

pub fn dashboard_with_info<'a, T: Into<Element<'a, Message>>>(
    menu: &'a Menu,
    cache: &'a Cache,
    content: T,
    cube_name: &'a str,
    avatar_handle: Option<&'a iced::widget::image::Handle>,
    lightning_address: Option<&'a str>,
) -> Element<'a, Message> {
    dashboard_with_info_layout(
        menu,
        cache,
        None,
        content,
        cube_name,
        avatar_handle,
        lightning_address,
        PageWidth::Reading,
    )
}

/// The pinned head over a page's scrollable, or an empty slot. The slot is
/// always there, so a head that comes and goes (a Send that turns into its
/// celebration) never moves the scrollable in the tree and resets its offset.
fn head_slot<'a>(head: Option<PageHead<'a, Message>>) -> Element<'a, Message> {
    head.map(|head| head.view(page_gutter))
        .unwrap_or_else(|| Space::new().into())
}

/// The page body's top padding: tighter under a head (`.main .pg`).
fn body_top(headed: bool) -> f32 {
    if headed {
        BODY_TOP
    } else {
        PAGE_TOP
    }
}

/// `head` is the page's own; without one, Settings and the Vault's Tools
/// take theirs from the route. Settings carries its scope switch and tabs in
/// the head; the Tools tabs stay in the body for now (plan W6).
#[allow(clippy::too_many_arguments)]
fn dashboard_with_info_layout<'a, T: Into<Element<'a, Message>>>(
    menu: &'a Menu,
    cache: &'a Cache,
    head: Option<PageHead<'a, Message>>,
    content: T,
    cube_name: &'a str,
    avatar_handle: Option<&'a iced::widget::image::Handle>,
    lightning_address: Option<&'a str>,
    width: PageWidth,
) -> Element<'a, Message> {
    let head = head
        .map(|head| vault::tools::with_back(menu, head))
        .or_else(|| settings::frame::page_head(menu, cache))
        .or_else(|| vault::tools::page_head(menu, cache));
    let body = page(
        Column::new()
            .push_maybe(vault::tools::in_content_navigation(menu, cache))
            .push(content.into())
            .width(Length::Fill),
        width,
    )
    .vertical(body_top(head.is_some()), PAGE_BOTTOM);
    let content_column: Element<'_, Message> = Column::new()
        .push(warn(None))
        .push_maybe(cache.spark_notice.as_ref().map(spark_notice_banner))
        .push(head_slot(head))
        .push(
            Container::new(scrollable(body).on_scroll(|w| Message::Scroll(w.absolute_offset().y)))
                .width(Length::Fill)
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

const WALLET_PAGE_SCROLL_ID: &str = "tenshu.wallet.page.scroll";

/// A reading page with Activity's detail pane (`.det`) for a selected row:
/// beside the page at the content's right edge when both fit, else over the
/// page from that edge. `content` and `detail` are rebuilt per layout width.
///
/// The pinned `head` spans the page beside the pane, not the pane itself.
pub fn dashboard_with_detail<'a>(
    menu: &'a Menu,
    cache: &'a Cache,
    head: impl Fn() -> PageHead<'a, Message> + 'a,
    content: impl Fn() -> Element<'a, Message> + 'a,
    detail: impl Fn() -> Option<Element<'a, Message>> + 'a,
) -> Element<'a, Message> {
    let body = iced::widget::responsive(move |size: iced::Size| {
        let page_body = Column::new()
            .push(head_slot(Some(head())))
            .push(
                Container::new(
                    scrollable(
                        page(
                            Column::new().push(content()).width(Length::Fill),
                            PageWidth::Reading,
                        )
                        .vertical(BODY_TOP, PAGE_BOTTOM),
                    )
                    .id(iced::advanced::widget::Id::from(WALLET_PAGE_SCROLL_ID))
                    .on_scroll(|w| Message::Scroll(w.absolute_offset().y)),
                )
                .width(Length::Fill)
                .style(theme::container::background)
                .height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill);
        // The page keeps one place in the tree, pane or not, beside or over:
        // otherwise Iced drops its scroll offset when a row is selected.
        let detail = detail();
        let beside = activity::wallet_detail_beside(size.width);
        let mut split = Row::new().push(page_body).height(Length::Fill);
        let mut over: Element<'a, Message> = Space::new().into();
        match detail {
            Some(detail) if beside => {
                split = split
                    .push(iced::widget::rule::vertical(1))
                    .push(activity::detail_column(detail));
            }
            Some(detail) => {
                over = row![
                    Space::new().width(Length::Fill),
                    iced::widget::opaque(row![
                        iced::widget::rule::vertical(1),
                        activity::detail_column(detail)
                    ])
                ]
                .into();
            }
            None => {}
        }
        iced::widget::Stack::new().push(split).push(over).into()
    });
    let content_column: Element<'_, Message> = Column::new()
        .push(warn(None))
        .push_maybe(cache.spark_notice.as_ref().map(spark_notice_banner))
        .push(body)
        .width(Length::Fill)
        .into();
    dashboard_shell_with_info(
        menu,
        cache,
        content_column,
        &cache.cube_name,
        cache.avatar_handle.as_ref(),
        cache.lightning_address.as_deref(),
    )
}

/// Use the same shell for panels with their own scroll handling, such as P2P
/// chat. `body` owns its content and warnings; the shell owns all navigation.
///
/// The shell adds no page head. P2P order details pins its own above its
/// scrollable. The chats have none (plan L3 exception): each lays out its own
/// header, message list and composer to the pane's full height, and a head
/// above it would take that height from the messages.
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
        cube_name,
        lightning_address,
        avatar: avatar_handle,
        ..nav::NavContext::from_cache(cache)
    };

    let shell_state = nav::shell::ShellState {
        switcher_open: cache.cube_switcher_open,
        lock_confirmation: cache.cube_lock_confirmation,
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

/// Full-panel placeholder for a route whose feature isn't available — e.g. a
/// restored Liquid route after its account gate closes, or a network-disabled
/// rail item. Mirrors [`placeholder`] but owns its copy so it can be built
/// from a transient reason string (see
/// [`crate::app::features::route_availability`]).
pub fn feature_unavailable_panel(reason: String) -> Element<'static, Message> {
    if is_liquid_account_unavailable(&reason) {
        let overview = Message::Menu(Menu::Cube(crate::app::menu::CubeSubMenu::Overview));
        let gate = Column::new()
            .push(iced::widget::rule::horizontal(1))
            .push(
                Container::new(
                    Column::new()
                        .push(text(reason).bold())
                        .push(
                            text("Your other wallets are unaffected.")
                                .size(P2_SIZE)
                                .style(theme::text::secondary),
                        )
                        .push(
                            button::secondary_action(
                                liquid_account_overview_id(),
                                None,
                                "Back to Overview",
                                Message::FocusAction,
                            )
                            .on_press(overview.clone()),
                        )
                        .spacing(14),
                )
                .padding([24, 0]),
            )
            .push(iced::widget::rule::horizontal(1))
            .width(Length::Fill);
        let content = Column::new()
            .push(
                button::secondary_action(
                    liquid_account_back_id(),
                    Some(coincube_ui::icon::arrow_back()),
                    "Overview",
                    Message::FocusAction,
                )
                .style(theme::button::transparent)
                .width(Length::Shrink)
                .on_press(overview),
            )
            .push(Space::new().height(Length::Fixed(10.0)))
            .push(text("Liquid").size(H3_SIZE).bold())
            .push(Space::new().height(Length::Fixed(32.0)))
            .push(gate)
            .width(Length::Fill);

        // The dashboard's page container supplies the 28px top and the gutter.
        return Container::new(content).width(Length::Fill).into();
    }

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

fn is_liquid_account_unavailable(reason: &str) -> bool {
    reason == crate::app::features::LIQUID_ACCOUNT_UNAVAILABLE_REASON
}

fn liquid_account_overview_id() -> Id {
    Id::from("tenshu.liquid.account-unavailable.overview")
}

fn liquid_account_back_id() -> Id {
    Id::from("tenshu.liquid.account-unavailable.back")
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

#[cfg(test)]
mod unavailable_panel_tests {
    use super::{
        feature_unavailable_panel, is_liquid_account_unavailable, liquid_account_back_id,
        liquid_account_overview_id, Message,
    };
    use crate::app::{
        features::{self, LiquidGate, MarketplaceServerFlags},
        menu::{CubeSubMenu, LiquidSubMenu, Menu, SparkSubMenu},
        view::shared::landing::capture,
    };
    use coincube_core::miniscript::bitcoin::Network;
    use coincube_ui::theme;
    use iced::keyboard::key::Named;

    #[test]
    fn account_disabled_liquid_gets_the_account_panel() {
        let reason = features::route_availability(
            &Menu::Liquid(LiquidSubMenu::Overview),
            Network::Bitcoin,
            false,
            MarketplaceServerFlags::OFF,
            LiquidGate::HIDDEN,
        )
        .reason()
        .unwrap()
        .to_owned();

        assert!(is_liquid_account_unavailable(&reason));
        assert_eq!(reason, features::LIQUID_ACCOUNT_UNAVAILABLE_REASON);
    }

    #[test]
    fn unsupported_networks_keep_the_network_panel() {
        let granted = LiquidGate {
            server_enabled: true,
            local_state_exists: false,
        };
        // Liquid isn't here: it is hidden for everyone since Tenshu v175, so
        // a Liquid route gets the account panel on every network
        // (`features::gated_off_liquid_route_reads_as_account_scoped_not_network_scoped`).
        for (menu, network, expected_reason) in [(
            Menu::Spark(SparkSubMenu::Overview),
            Network::Signet,
            "Spark isn't available on Signet.",
        )] {
            let reason = features::route_availability(
                &menu,
                network,
                false,
                MarketplaceServerFlags::OFF,
                granted,
            )
            .reason()
            .unwrap()
            .to_owned();
            assert!(!is_liquid_account_unavailable(&reason));
            assert_eq!(reason, expected_reason);
        }
    }

    #[test]
    fn unavailable_routes_mount_in_both_themes_and_overview_activates_from_keyboard() {
        let granted = LiquidGate {
            server_enabled: true,
            local_state_exists: false,
        };
        let cases = [
            (
                "liquid-account-unavailable",
                Menu::Liquid(LiquidSubMenu::Overview),
                Network::Bitcoin,
                LiquidGate::HIDDEN,
                true,
            ),
            (
                "spark-network-unavailable",
                Menu::Spark(SparkSubMenu::Overview),
                Network::Signet,
                granted,
                false,
            ),
        ];

        for (scenario, menu, network, liquid_gate, account_gate) in cases {
            let reason = features::route_availability(
                &menu,
                network,
                false,
                MarketplaceServerFlags::OFF,
                liquid_gate,
            )
            .reason()
            .expect("route is unavailable")
            .to_owned();
            for (appearance, palette) in [
                ("dark", theme::Theme::dark()),
                ("light", theme::Theme::light()),
            ] {
                for outer in [(1024, 768), (1440, 900)] {
                    let mounted = capture::inspect(
                        scenario,
                        feature_unavailable_panel(reason.clone()),
                        outer,
                        appearance,
                        palette,
                    );
                    if account_gate {
                        assert!(mounted.text.iter().any(|(value, _)| value == "Liquid"));
                        assert!(mounted.text.iter().any(|(value, _)| value == "Overview"));
                        assert!(mounted.contains(features::LIQUID_ACCOUNT_UNAVAILABLE_REASON));
                        assert!(mounted.contains("Your other wallets are unaffected."));
                        assert!(mounted.contains("Back to Overview"));
                        assert!(!mounted.contains("Not available on this network"));
                        assert!(mounted.has_focus(liquid_account_back_id()));
                        assert!(mounted.has_focus(liquid_account_overview_id()));
                    } else {
                        assert!(mounted.contains("Not available on this network"));
                        assert!(mounted.contains(&reason));
                        assert!(!mounted.contains("Your other wallets are unaffected."));
                        assert!(!mounted.contains("Back to Overview"));
                        assert!(!mounted.has_focus(liquid_account_back_id()));
                        assert!(!mounted.has_focus(liquid_account_overview_id()));
                    }
                }
            }
            if account_gate {
                for target in [liquid_account_back_id(), liquid_account_overview_id()] {
                    for key in [Named::Enter, Named::Space] {
                        assert!(matches!(
                            capture::activate_with_key(
                                feature_unavailable_panel(reason.clone()),
                                target.clone(),
                                (1024, 768),
                                false,
                                key,
                            )
                            .as_slice(),
                            [Message::Menu(Menu::Cube(CubeSubMenu::Overview))]
                        ));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod page_tests {
    use super::{nav::split::sidebar_width_for_window, page_gutter};
    use coincube_ui::component::tenshu::gutter_for_window;

    #[test]
    fn page_gutter_is_the_window_clamp_seen_from_the_pane() {
        // 1440 window: 248px menu, 1192px pane, 43.2px gutter.
        assert_eq!(page_gutter(1192.0), gutter_for_window(1440.0));
        // 1024 minimum window: compact 208px menu.
        assert_eq!(page_gutter(816.0), gutter_for_window(1024.0));
        // From 1600 the gutter stops growing.
        assert_eq!(page_gutter(2560.0 - 248.0), 48.0);
    }

    #[test]
    fn every_window_width_gets_its_own_gutter_within_a_pixel_and_a_bit() {
        // The menu narrows at 1100, so windows 1060..=1100 (compact menu) and
        // 1100..=1140 (full menu) share pane widths; the gutter is then read
        // from the full-menu window, at most 40px * 3% = 1.2px off.
        for window in 1024..=2560 {
            let window = window as f32;
            let pane = window - sidebar_width_for_window(window);
            let off = (page_gutter(pane) - gutter_for_window(window)).abs();
            assert!(off <= 1.21, "window {}: off by {}", window, off);
        }
    }
}

#[cfg(test)]
mod page_head_tests {
    use super::{
        dashboard, dashboard_headed, page_head, spark_page_head, vault_page_head, wallet_page_head,
        Message,
    };
    use crate::app::cache::{Cache, WalletNames};
    use crate::app::menu::{CubeSettingsOption, CubeSubMenu, Menu, VaultSubMenu};
    use coincube_ui::component::tenshu::PageHead;
    use coincube_ui::widget::{Column, Element};
    use iced::advanced::widget::operation::{Focusable, Scrollable};
    use iced::advanced::widget::{Id, Operation, Tree};
    use iced::advanced::{layout, renderer::Headless, Layout};
    use iced::{mouse, Point, Rectangle, Size, Vector};

    const WINDOW: Size = Size::new(1024.0, 768.0);
    const TITLE: &str = "A long page";

    /// The text and scrollables of a laid-out page, as an [`Operation`] sees
    /// them. Also used by the P2P order details test.
    #[derive(Default)]
    pub(crate) struct Probe {
        text: Vec<(String, Rectangle)>,
        scrollables: Vec<(Rectangle, Vector)>,
    }

    impl Operation for Probe {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn text(&mut self, _id: Option<&Id>, bounds: Rectangle, value: &str) {
            self.text.push((value.to_owned(), bounds));
        }

        fn focusable(&mut self, _id: Option<&Id>, _bounds: Rectangle, _: &mut dyn Focusable) {}

        fn scrollable(
            &mut self,
            _id: Option<&Id>,
            bounds: Rectangle,
            _content_bounds: Rectangle,
            translation: Vector,
            _state: &mut dyn Scrollable,
        ) {
            self.scrollables.push((bounds, translation));
        }
    }

    impl Probe {
        pub(crate) fn text(&self, value: &str) -> Rectangle {
            self.text
                .iter()
                .find(|(text, _)| text == value)
                .unwrap_or_else(|| panic!("no {:?}", value))
                .1
        }

        /// The scrollable that holds `inner`, and how far it has scrolled.
        pub(crate) fn scroller_of(&self, inner: Rectangle) -> (Rectangle, Vector) {
            *self
                .scrollables
                .iter()
                .filter(|(bounds, _)| bounds.x <= inner.x && inner.x < bounds.x + bounds.width)
                .max_by(|(a, _), (b, _)| a.width.partial_cmp(&b.width).unwrap())
                .expect("a scrollable around the page body")
        }
    }

    pub(crate) fn renderer() -> iced::Renderer {
        for bytes in coincube_ui::font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            coincube_ui::font::REGULAR,
            iced::Pixels(14.0),
            Some("tiny-skia"),
        ))
        .unwrap()
    }

    fn long_page<'a>(menu: &'a Menu, cache: &'a Cache) -> Element<'a, Message> {
        let rows = (0..80).fold(Column::new().spacing(12), |column, row| {
            column.push(iced::widget::text(format!("Row {}", row)).size(14))
        });
        dashboard_headed(menu, cache, page_head(cache, TITLE), rows)
    }

    /// Lays the page out in a 1024×768 window as a fresh view, as every frame
    /// does, and probes it.
    pub(crate) fn frame(
        tree: &mut Tree,
        renderer: &iced::Renderer,
        element: &mut Element<'_, Message>,
    ) -> (layout::Node, Probe) {
        tree.diff(element.as_widget());
        let node = element.as_widget_mut().layout(
            tree,
            renderer,
            &layout::Limits::new(Size::ZERO, WINDOW),
        );
        let mut probe = Probe::default();
        element
            .as_widget_mut()
            .operate(tree, Layout::new(&node), renderer, &mut probe);
        (node, probe)
    }

    /// Turns the mouse wheel `dy` pixels down the page with the cursor at
    /// `at`, and returns the messages that publishes.
    pub(crate) fn wheel(
        tree: &mut Tree,
        renderer: &iced::Renderer,
        element: &mut Element<'_, Message>,
        node: &layout::Node,
        at: Point,
        dy: f32,
    ) -> Vec<Message> {
        let mut messages = Vec::new();
        element.as_widget_mut().update(
            tree,
            &iced::Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { x: 0.0, y: -dy },
            }),
            Layout::new(node),
            mouse::Cursor::Available(at),
            renderer,
            &mut iced::advanced::clipboard::Null,
            &mut iced::advanced::Shell::new(&mut messages),
            &Rectangle::with_size(WINDOW),
        );
        messages
    }

    #[test]
    fn the_head_stays_put_while_a_long_page_scrolls_under_it() {
        let renderer = renderer();
        let cache = Cache {
            cube_name: "Everyday".into(),
            ..Cache::default()
        };
        let menu = Menu::Cube(CubeSubMenu::Overview);
        let mut element = long_page(&menu, &cache);
        let mut tree = Tree::new(element.as_widget());
        let (node, before) = frame(&mut tree, &renderer, &mut element);
        let title = before.text(TITLE);
        let line = before.text("Everyday · Bitcoin mainnet");
        let first_row = before.text("Row 0");
        let (viewport, offset) = before.scroller_of(first_row);
        assert_eq!(offset, Vector::ZERO);
        assert!(
            viewport.y >= line.y + line.height,
            "only the body scrolls; the head sits above its viewport: {:?} {:?}",
            line,
            viewport
        );

        let messages = wheel(
            &mut tree,
            &renderer,
            &mut element,
            &node,
            first_row.center(),
            400.0,
        );
        let scrolled = messages
            .iter()
            .find_map(|message| match message {
                Message::Scroll(y) => Some(*y),
                _ => None,
            })
            .expect("the body reports its scroll");
        assert!(scrolled > 0.0, "{}", scrolled);

        // The next frame is a new view over the same tree.
        let mut element = long_page(&menu, &cache);
        let (_, after) = frame(&mut tree, &renderer, &mut element);
        assert_eq!(after.text(TITLE), title, "the title does not move");
        assert_eq!(
            after.text("Everyday · Bitcoin mainnet"),
            line,
            "the Cube line does not move"
        );
        let (_, offset) = after.scroller_of(after.text("Row 0"));
        assert_eq!(offset.y, scrolled, "the body moved by the reported offset");
        // Content draws at its layout position less the translation.
        let shown_first_row = after.text("Row 0").y - offset.y;
        assert!(
            shown_first_row < viewport.y,
            "the first row scrolled out under the head: {} vs {:?}",
            shown_first_row,
            viewport
        );
    }

    #[test]
    fn wallet_heads_name_the_wallet_as_the_side_panel_does() {
        let renderer = renderer();
        let menu = Menu::Cube(CubeSubMenu::Overview);
        let named = Cache {
            cube_name: "Everyday".into(),
            wallet_names: WalletNames {
                spark: Some("Daily spending".into()),
                vault: Some("Family savings".into()),
            },
            ..Cache::default()
        };
        // A blank name reads as no name, as in the side panel.
        let unnamed = Cache {
            cube_name: "Everyday".into(),
            wallet_names: WalletNames {
                spark: None,
                vault: Some("  ".into()),
            },
            ..Cache::default()
        };
        type Head = fn(&Cache) -> PageHead<'_, Message>;
        let cases: [(&Cache, Head, &str, &str); 6] = [
            // H-Send, H-Send-Vault, H-Receive: "‹w.name› · ‹Cube› · ‹network›".
            (
                &named,
                |cache| vault_page_head(cache, "Send"),
                "Send",
                "Vault · Family savings · Everyday · Bitcoin mainnet",
            ),
            (
                &named,
                |cache| spark_page_head(cache, "Receive"),
                "Receive",
                "Spark · Daily spending · Everyday · Bitcoin mainnet",
            ),
            (
                &unnamed,
                |cache| vault_page_head(cache, "Send"),
                "Send",
                "Vault · Everyday · Bitcoin mainnet",
            ),
            (
                &unnamed,
                |cache| spark_page_head(cache, "Receive"),
                "Receive",
                "Spark · Everyday · Bitcoin mainnet",
            ),
            // A wallet's own page is titled with its name.
            (
                &named,
                |cache| wallet_page_head(cache, "Vault", cache.wallet_names.vault.as_deref()),
                "Vault · Family savings",
                "Everyday · Bitcoin mainnet",
            ),
            (
                &named,
                |cache| wallet_page_head(cache, "Spark", cache.wallet_names.spark.as_deref()),
                "Spark · Daily spending",
                "Everyday · Bitcoin mainnet",
            ),
        ];
        for (cache, head, title, line) in cases {
            let mut element = dashboard_headed(&menu, cache, head(cache), Column::new());
            let mut tree = Tree::new(element.as_widget());
            let (_, probe) = frame(&mut tree, &renderer, &mut element);
            probe.text(title);
            probe.text(line);
        }
    }

    fn rows<'a>() -> Column<'a, Message> {
        (0..4).fold(Column::new(), |column, row| {
            column.push(iced::widget::text(format!("Row {}", row)))
        })
    }

    #[test]
    fn heads_cut_a_long_cube_name_as_the_side_panel_does() {
        let renderer = renderer();
        let named = |name: &str| Cache {
            cube_name: name.into(),
            ..Cache::default()
        };
        // Saved names have no length limit and may hold line breaks; the
        // head doesn't scroll, so it shows the side panel's 16-character cut.
        let long = named("A very long Cube name\nthat goes on and on, line after line");
        let short = named("Everyday");
        let overview = Menu::Cube(CubeSubMenu::Overview);
        let settings = Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::General));
        let tools = Menu::Vault(VaultSubMenu::Recovery);
        let send = Menu::Vault(VaultSubMenu::Send);
        type Page = for<'a> fn(&'a Menu, &'a Cache) -> Element<'a, Message>;
        let cases: [(&Menu, Page, &str); 4] = [
            (
                &overview,
                |menu, cache| dashboard_headed(menu, cache, page_head(cache, "Overview"), rows()),
                "A very long Cube… · Bitcoin mainnet",
            ),
            (
                &settings,
                |menu, cache| dashboard(menu, cache, rows()),
                "Cube · A very long Cube… · Bitcoin mainnet",
            ),
            (
                &tools,
                |menu, cache| dashboard(menu, cache, rows()),
                "A very long Cube… · Bitcoin mainnet",
            ),
            (
                &send,
                |menu, cache| dashboard_headed(menu, cache, vault_page_head(cache, "Send"), rows()),
                "Vault · A very long Cube… · Bitcoin mainnet",
            ),
        ];
        for (menu, page, line) in cases {
            let viewport = |cache: &Cache, line: Option<&str>| {
                let mut element = page(menu, cache);
                let mut tree = Tree::new(element.as_widget());
                let (_, probe) = frame(&mut tree, &renderer, &mut element);
                if let Some(line) = line {
                    probe.text(line);
                }
                probe.scroller_of(probe.text("Row 0")).0
            };
            // The head is as tall as it is with a short name.
            assert_eq!(
                viewport(&long, Some(line)).y,
                viewport(&short, None).y,
                "{:?}",
                menu
            );
        }
    }

    #[test]
    fn a_vault_tool_step_keeps_back_to_vault_under_its_own_title() {
        let renderer = renderer();
        let cache = Cache::default();
        // Recovery's compose and review steps pass their own head on the
        // Recovery tool route; the Tools head's Back to Vault stays.
        for (menu, back) in [
            (Menu::Vault(VaultSubMenu::Recovery), true),
            (Menu::Vault(VaultSubMenu::Send), false),
        ] {
            let mut element =
                dashboard_headed(&menu, &cache, vault_page_head(&cache, "Recovery"), rows());
            let mut tree = Tree::new(element.as_widget());
            let (_, probe) = frame(&mut tree, &renderer, &mut element);
            probe.text("Recovery");
            assert_eq!(
                probe.text.iter().any(|(text, _)| text == "Back to Vault"),
                back,
                "{:?}",
                menu
            );
        }
    }
}
