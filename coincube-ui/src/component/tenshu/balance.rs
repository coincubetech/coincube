//! Balance presentation that fails closed.
//!
//! The caller passes formatted strings and the visibility taken from the
//! accepted global preference. Unless that visibility is [`AmountVisibility::Shown`],
//! every value renders as the shared mask and its strings are dropped before
//! any widget is built, so they never enter the Iced tree. These widgets hold
//! no privacy state and do not react to hover, press or focus.

use std::borrow::Cow;

use iced::advanced::text::Shaping;
use iced::widget::text::LineHeight;
use iced::Alignment;

use crate::font;
use crate::icon;
use crate::theme::tenshu::{size, text as style, type_size};
use crate::widget::{Column, Element, Row, Text};

/// The one mask for a hidden amount, shared with the classic Overview.
pub const MASK: &str = "••••••";

/// Placeholder for a balance that has no value yet.
const NO_VALUE: &str = "—";

/// Whether amounts may be drawn, as decided by the global preference owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmountVisibility {
    Shown,
    Hidden,
    /// The preference has not been resolved yet. Treated as hidden.
    Unresolved,
}

impl AmountVisibility {
    /// Maps the accepted "hide balances" preference. `Some(false)` is the only
    /// value that shows amounts.
    pub fn from_hidden_preference(hidden: Option<bool>) -> Self {
        match hidden {
            Some(false) => AmountVisibility::Shown,
            Some(true) => AmountVisibility::Hidden,
            None => AmountVisibility::Unresolved,
        }
    }

    pub fn shows_amounts(self) -> bool {
        self == AmountVisibility::Shown
    }
}

/// A formatted number with an optional muted unit, such as `50,360` `SATS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Figure<'a> {
    value: Cow<'a, str>,
    unit: Option<Cow<'a, str>>,
}

impl<'a> Figure<'a> {
    pub fn new(value: impl Into<Cow<'a, str>>) -> Self {
        Self {
            value: value.into(),
            unit: None,
        }
    }

    pub fn unit(mut self, unit: impl Into<Cow<'a, str>>) -> Self {
        self.unit = Some(unit.into());
        self
    }
}

impl<'a> From<&'a str> for Figure<'a> {
    fn from(value: &'a str) -> Self {
        Figure::new(value)
    }
}

impl From<String> for Figure<'static> {
    fn from(value: String) -> Self {
        Figure::new(value)
    }
}

/// What the owning state knows about a balance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BalanceValue<'a> {
    /// An authoritative amount. `secondary` is the other unit or a fiat value.
    Known {
        primary: Figure<'a>,
        secondary: Option<Figure<'a>>,
    },
    /// An authoritative zero, drawn with the artboards' muted zero treatment.
    Zero {
        primary: Figure<'a>,
        secondary: Option<Figure<'a>>,
    },
    /// Hide the amount regardless of visibility.
    Masked,
    /// Not known yet, such as while history loads.
    Pending { label: Cow<'a, str> },
    /// Cannot be known now, such as while the node is offline.
    Unavailable { reason: Cow<'a, str> },
}

/// Where the balance sits, which sets its scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalanceTier {
    /// Total balance: 40px figure over a 14px secondary line (`.big`, `.sats`).
    Hero,
    /// Wallet card: 26px figure over a 14px secondary line (`.amt`, `.sats`).
    Card,
    /// Asset row: one 14px line with the secondary value muted (`.val`).
    Row,
}

/// Draw a balance. Unless `visibility` shows amounts, only [`MASK`] is drawn.
pub fn balance<'a, M: 'a>(
    value: BalanceValue<'a>,
    tier: BalanceTier,
    visibility: AmountVisibility,
) -> Element<'a, M> {
    let value = if visibility.shows_amounts() {
        value
    } else {
        // Drop every caller string before building widgets.
        BalanceValue::Masked
    };
    match tier {
        BalanceTier::Hero | BalanceTier::Card => stacked(value, tier),
        BalanceTier::Row => inline(value),
    }
}

fn stacked<'a, M: 'a>(value: BalanceValue<'a>, tier: BalanceTier) -> Element<'a, M> {
    let (size, line_height) = match tier {
        BalanceTier::Hero => (type_size::AMOUNT_HERO, 1.15),
        _ => (type_size::AMOUNT_CARD, 1.2),
    };
    let headline = |content: Cow<'a, str>| -> Text<'a> {
        amount_text(content, size, font::SEMIBOLD).line_height(LineHeight::Relative(line_height))
    };
    let column = Column::new().spacing(2);
    match value {
        BalanceValue::Known { primary, secondary } => with_figure_line(
            column.push(headline(figure_text(primary)).style(style::ink)),
            secondary,
            false,
        ),
        BalanceValue::Zero { primary, secondary } => with_figure_line(
            column.push(headline(figure_text(primary)).style(style::ink)),
            secondary,
            true,
        ),
        BalanceValue::Masked => column
            .push(headline(MASK.into()).style(style::ink))
            .push(amount_text(MASK.into(), type_size::BODY, font::SEMIBOLD).style(style::ink)),
        BalanceValue::Pending { label } => column
            .push(headline(NO_VALUE.into()).style(style::muted))
            .push(note(label, icon::clock_icon(), style::muted)),
        BalanceValue::Unavailable { reason } => column
            .push(headline(NO_VALUE.into()).style(style::muted))
            .push(note(reason, icon::warning_icon(), warn)),
    }
    .into()
}

fn inline<'a, M: 'a>(value: BalanceValue<'a>) -> Element<'a, M> {
    let row = Row::new().spacing(10).align_y(Alignment::Center);
    match value {
        BalanceValue::Known { primary, secondary } => {
            push_secondary(row.push(figure_row(primary, false)), secondary)
        }
        BalanceValue::Zero { primary, secondary } => {
            push_secondary(row.push(figure_row(primary, true)), secondary)
        }
        BalanceValue::Masked => {
            row.push(amount_text(MASK.into(), type_size::BODY, font::SEMIBOLD).style(style::ink))
        }
        BalanceValue::Pending { label } => row
            .push(amount_text(NO_VALUE.into(), type_size::BODY, font::SEMIBOLD).style(style::muted))
            .push(note(label, icon::clock_icon(), style::muted)),
        BalanceValue::Unavailable { reason } => row
            .push(amount_text(NO_VALUE.into(), type_size::BODY, font::SEMIBOLD).style(style::muted))
            .push(note(reason, icon::warning_icon(), warn)),
    }
    .into()
}

fn with_figure_line<'a, M: 'a>(
    column: Column<'a, M>,
    figure: Option<Figure<'a>>,
    zero: bool,
) -> Column<'a, M> {
    match figure {
        Some(figure) => column.push(figure_row(figure, zero)),
        None => column,
    }
}

/// `value unit` on one line; the value is semibold ink, or regular muted at zero.
fn figure_row<'a, M: 'a>(figure: Figure<'a>, zero: bool) -> Row<'a, M> {
    let Figure { value, unit } = figure;
    let value = if zero {
        amount_text(value, type_size::BODY, font::REGULAR).style(style::muted)
    } else {
        amount_text(value, type_size::BODY, font::SEMIBOLD).style(style::ink)
    };
    let row = Row::new().spacing(4).align_y(Alignment::Center).push(value);
    match unit {
        Some(unit) => {
            row.push(amount_text(unit, type_size::BODY, font::REGULAR).style(style::muted))
        }
        None => row,
    }
}

fn push_secondary<'a, M: 'a>(row: Row<'a, M>, secondary: Option<Figure<'a>>) -> Row<'a, M> {
    match secondary {
        Some(secondary) => row.push(
            amount_text(figure_text(secondary), type_size::BODY, font::REGULAR).style(style::muted),
        ),
        None => row,
    }
}

fn figure_text(figure: Figure<'_>) -> Cow<'_, str> {
    match figure.unit {
        Some(unit) => Cow::Owned(format!("{} {}", figure.value, unit)),
        None => figure.value,
    }
}

fn note<'a, M: 'a>(
    content: Cow<'a, str>,
    glyph: Text<'a>,
    colour: fn(&crate::theme::Theme) -> iced::widget::text::Style,
) -> Row<'a, M> {
    Row::new()
        .spacing(6)
        .align_y(Alignment::Center)
        .push(glyph.size(size::ICON_SMALL).style(colour))
        .push(amount_text(content, type_size::CHIP, font::REGULAR).style(colour))
}

fn warn(theme: &crate::theme::Theme) -> iced::widget::text::Style {
    style::tone(theme, crate::theme::tenshu::Tone::Warn)
}

fn amount_text<'a>(content: Cow<'a, str>, size: f32, weight: iced::Font) -> Text<'a> {
    iced::widget::text(content)
        .shaping(Shaping::Advanced)
        .size(size)
        .font(weight)
}
