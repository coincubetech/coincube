//! Theme-aware policy timing diagram shared by all six Vault introductions.

use iced::{widget::container, widget::Space, Alignment, Background, Border, Color, Length};

use coincube_ui::theme::{palette::ThemeMode, Theme};
use coincube_ui::{
    color,
    component::text::{caption, p2_regular, Text},
    theme,
    widget::*,
};

use crate::installer::message::Message;

/// A visual ordering hint. Actual block delays are chosen on the following step.
#[derive(Clone, Copy)]
pub enum Timelock {
    None,
    After(f32),
}

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

fn timing_segment<'a>(segment_color: Color, width: Length) -> Element<'a, Message> {
    Container::new(Space::new())
        .width(width)
        .height(Length::Fixed(10.0))
        .style(move |t| container::Style {
            background: Some(Background::Color(theme::text::adapt_color(
                segment_color,
                t,
            ))),
            border: Border {
                radius: 5.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

fn policy_row<'a>(row: PolicyRow<'a>) -> Element<'a, Message> {
    let badges = row.keys.into_iter().fold(
        Row::new().spacing(4).align_y(Alignment::Center),
        |items, (number, key_color)| items.push(badge(number, key_color)),
    );
    let (timing_label, timing_bar): (&str, Element<'a, Message>) = match row.timelock {
        Timelock::None => (
            "Can spend as soon as funds arrive",
            timing_segment(color::GREEN, Length::Fill),
        ),
        Timelock::After(fraction) => {
            let locked = ((fraction * 100.0).round() as u16).clamp(1, 99);
            (
                "Timelocked · can spend after inactivity",
                Row::new()
                    .spacing(4)
                    .push(timing_segment(color::RED, Length::FillPortion(locked)))
                    .push(timing_segment(
                        color::GREEN,
                        Length::FillPortion(100 - locked),
                    ))
                    .into(),
            )
        }
    };

    Column::new()
        .width(Length::Fill)
        .spacing(8)
        .push(caption(row.label).style(theme::text::secondary))
        .push(
            Container::new(
                Row::new()
                    .align_y(Alignment::Center)
                    .spacing(12)
                    .push(badges)
                    .push(p2_regular(row.box_label).bold()),
            )
            .width(Length::Fill)
            .padding([10, 12])
            .style(|t| container::Style {
                background: Some(Background::Color(key_surface(t))),
                border: Border {
                    radius: 10.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .push(p2_regular(timing_label).style(theme::text::secondary))
        .push(timing_bar)
        .into()
}

/// Render one readable vertical row per policy. The timing bar is compact;
/// status text remains outside it so it does not clip at a 1024px window.
pub fn policy_timeline<'a>(rows: Vec<PolicyRow<'a>>) -> Element<'a, Message> {
    let content = rows.into_iter().fold(
        Column::new().width(Length::Fill).spacing(18).push(
            caption("Receipt of funds → after wallet inactivity").style(theme::text::secondary),
        ),
        |column, row| column.push(policy_row(row)),
    );

    Container::new(
        content.push(
            caption("Recovery delays can be configured in the next step.")
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
