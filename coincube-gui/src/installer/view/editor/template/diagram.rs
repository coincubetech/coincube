//! Theme-aware policy timing diagram shared by all six Vault introductions.
//! The wide composition keeps policy keys beside labelled spend windows; the
//! compact composition stacks them so the timing labels remain readable.

use iced::{
    widget::{container, responsive, Space},
    Alignment, Background, Border, Color, Length, Size,
};

use coincube_ui::theme::{palette::ThemeMode, Theme};
use coincube_ui::{
    color,
    component::text::{caption, p2_regular, Text},
    theme,
    widget::*,
};

use crate::installer::message::Message;

/// Relative visual ordering only. Actual block delays are set in the next step.
#[derive(Clone, Copy)]
pub enum Timelock {
    None,
    After(f32),
}

#[derive(Clone)]
pub struct PolicyRow<'a> {
    pub label: &'a str,
    pub keys: Vec<(u32, Color)>,
    pub box_label: &'a str,
    pub timelock: Timelock,
}

fn surface(t: &Theme) -> Color {
    if matches!(t.mode, ThemeMode::Light) {
        color::LIGHT_CARD_BG
    } else {
        color::GREY_5
    }
}

fn key_surface(t: &Theme) -> Color {
    if matches!(t.mode, ThemeMode::Light) {
        color::LIGHT_SURFACE
    } else {
        color::GREY_6
    }
}

fn badge<'a>(number: u32, key_color: Color) -> Element<'a, Message> {
    Container::new(
        p2_regular(number.to_string())
            .style(theme::text::adaptive(key_color))
            .bold(),
    )
    .center_x(Length::Fixed(26.0))
    .center_y(Length::Fixed(26.0))
    .style(move |t| container::Style {
        background: Some(Background::Color(key_surface(t))),
        border: Border {
            color: theme::text::adapt_color(key_color, t),
            width: 1.5,
            radius: 13.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn policy_box<'a>(row: &PolicyRow<'a>) -> Element<'a, Message> {
    let badges = row.keys.iter().fold(
        Row::new().spacing(4).align_y(Alignment::Center),
        |items, (number, key_color)| items.push(badge(*number, *key_color)),
    );
    Container::new(
        Row::new()
            .align_y(Alignment::Center)
            .spacing(8)
            .push(badges)
            .push(p2_regular(row.box_label).bold()),
    )
    .width(Length::Fill)
    .padding([8, 10])
    .style(|t| container::Style {
        background: Some(Background::Color(key_surface(t))),
        border: Border {
            radius: 10.0.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

fn spend_bar<'a>(label: &'static str, bar_color: Color, width: Length) -> Element<'a, Message> {
    Container::new(
        Row::new()
            .width(Length::Fill)
            .align_y(Alignment::Center)
            .push(Space::new().width(Length::Fill))
            .push(
                caption(label)
                    .style(theme::text::adaptive(bar_color))
                    .bold(),
            )
            .push(Space::new().width(Length::Fill)),
    )
    .width(width)
    .center_y(Length::Fixed(38.0))
    .padding([0, 7])
    .style(move |t| {
        let adapted = theme::text::adapt_color(bar_color, t);
        container::Style {
            background: Some(Background::Color(Color { a: 0.16, ..adapted })),
            border: Border {
                color: Color { a: 0.5, ..adapted },
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        }
    })
    .into()
}

fn spend_window<'a>(timelock: Timelock, compact: bool) -> Element<'a, Message> {
    match timelock {
        Timelock::None => spend_bar("CAN SPEND", color::GREEN, Length::Fill),
        Timelock::After(fraction) => {
            // The segments communicate relative activation order, not a block
            // count. Keep both labels readable on the narrower wide layout.
            let minimum = if compact { 45 } else { 60 };
            let locked = ((fraction * 100.0).round() as u16).clamp(minimum, 75);
            Row::new()
                .spacing(6)
                .width(Length::Fill)
                .push(spend_bar(
                    "CAN'T SPEND (TIMELOCKED)",
                    color::RED,
                    Length::FillPortion(locked),
                ))
                .push(spend_bar(
                    "CAN SPEND",
                    color::GREEN,
                    Length::FillPortion(100 - locked),
                ))
                .into()
        }
    }
}

fn policy_row<'a>(row: PolicyRow<'a>, compact: bool, key_width: f32) -> Element<'a, Message> {
    let keys = Column::new()
        .spacing(6)
        .push(caption(row.label).style(theme::text::secondary))
        .push(policy_box(&row));
    if compact {
        Column::new()
            .width(Length::Fill)
            .spacing(8)
            .push(keys)
            .push(spend_window(row.timelock, true))
            .into()
    } else {
        Row::new()
            .width(Length::Fill)
            .spacing(16)
            .align_y(Alignment::End)
            .push(Container::new(keys).width(Length::Fixed(key_width)))
            .push(Container::new(spend_window(row.timelock, false)).width(Length::Fill))
            .into()
    }
}

fn diagram_content<'a>(rows: &[PolicyRow<'a>], available_width: f32) -> Element<'a, Message> {
    let compact = available_width < 730.0;
    let key_width = if rows.iter().any(|row| row.keys.len() > 3) {
        310.0
    } else {
        240.0
    };
    let header: Element<'a, Message> = if compact {
        Row::new()
            .width(Length::Fill)
            .push(caption("Receipt of funds").style(theme::text::secondary))
            .push(Space::new().width(Length::Fill))
            .push(caption("After inactivity").style(theme::text::secondary))
            .into()
    } else {
        Row::new()
            .width(Length::Fill)
            .push(Space::new().width(Length::Fixed(key_width + 16.0)))
            .push(
                Row::new()
                    .width(Length::Fill)
                    .push(caption("Receipt of funds").style(theme::text::secondary))
                    .push(Space::new().width(Length::Fill))
                    .push(
                        caption("After some time* of wallet inactivity")
                            .style(theme::text::secondary),
                    ),
            )
            .into()
    };
    let content = rows.iter().cloned().fold(
        Column::new().width(Length::Fill).spacing(20).push(header),
        |column, row| column.push(policy_row(row, compact, key_width)),
    );
    Container::new(
        content.push(
            caption("*Recovery delays can be configured in the next step.")
                .style(theme::text::secondary),
        ),
    )
    .width(Length::Fill)
    .padding(18)
    .style(|t| container::Style {
        background: Some(Background::Color(surface(t))),
        border: Border {
            radius: 16.0.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

/// Horizontal policy/key column beside labelled timing bars at wide widths.
/// At compact widths the same labelled bars remain visible below each policy.
pub fn policy_timeline<'a>(rows: Vec<PolicyRow<'a>>) -> Element<'a, Message> {
    responsive(move |size: Size| diagram_content(&rows, size.width))
        .height(Length::Shrink)
        .into()
}
