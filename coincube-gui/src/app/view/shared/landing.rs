//! Pure presentation pieces shared by the four wallet landing routes.
//!
//! Callers decide visibility, units, rates, and availability before constructing
//! an `AmountCell`. This module never reads preferences or interprets token units.

use coincube_ui::{
    component::text::{text, Text as _, H2_SIZE, P1_SIZE, P2_SIZE},
    theme,
    widget::{Column, ColumnExt, Element, Row, RowExt},
};
use iced::{widget::text::Wrapping, Alignment, Length};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmountCell {
    pub primary: String,
    pub secondary: Option<String>,
    /// Nonmonetary status only. A masked cell discards this so callers cannot
    /// accidentally retain an amount-bearing tooltip or accessibility label.
    pub explanation: Option<String>,
}

impl AmountCell {
    pub fn shown(primary: impl Into<String>, secondary: Option<String>) -> Self {
        Self {
            primary: primary.into(),
            secondary,
            explanation: None,
        }
    }

    pub fn unknown(explanation: impl Into<String>) -> Self {
        Self {
            primary: "—".into(),
            secondary: None,
            explanation: Some(explanation.into()),
        }
    }

    pub fn unavailable(explanation: impl Into<String>) -> Self {
        Self {
            primary: "Unavailable".into(),
            secondary: None,
            explanation: Some(explanation.into()),
        }
    }

    pub fn masked() -> Self {
        Self {
            primary: "••••••".into(),
            secondary: None,
            explanation: None,
        }
    }

    pub fn with_explanation(mut self, explanation: impl Into<String>) -> Self {
        self.explanation = Some(explanation.into());
        self
    }

    /// Erases every supplied monetary string, including explanatory text.
    pub fn redact(self) -> Self {
        Self::masked()
    }
}

#[derive(Clone, Copy)]
pub enum AmountTier {
    Total,
    Card,
    Row,
}

pub fn amount_cell<'a, M: 'a>(cell: AmountCell, tier: AmountTier) -> Element<'a, M> {
    let size = match tier {
        AmountTier::Total => H2_SIZE + 8,
        AmountTier::Card => H2_SIZE,
        AmountTier::Row => P1_SIZE,
    };
    Column::new()
        .spacing(3)
        .push(
            text(cell.primary)
                .size(size)
                .bold()
                .wrapping(Wrapping::WordOrGlyph),
        )
        .push_maybe(cell.secondary.map(|s| {
            text(s)
                .size(P2_SIZE)
                .style(theme::text::secondary)
                .wrapping(Wrapping::WordOrGlyph)
        }))
        .push_maybe(cell.explanation.map(|s| {
            text(s)
                .size(P2_SIZE)
                .style(theme::text::secondary)
                .wrapping(Wrapping::WordOrGlyph)
        }))
        .into()
}

pub fn section_heading<'a, M: 'a>(
    title: &'static str,
    action: Option<Element<'a, M>>,
) -> Row<'a, M> {
    Row::new()
        .spacing(12)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .push(text(title).size(H2_SIZE).bold())
        .push(iced::widget::Space::new().width(Length::Fill))
        .push_maybe(action)
}

#[cfg(test)]
mod tests {
    use super::AmountCell;

    #[test]
    fn masking_erases_primary_secondary_and_explanation() {
        let cell = AmountCell::shown("42 BTC", Some("$123".into()))
            .with_explanation("42 BTC available at current rate")
            .redact();
        assert_eq!(cell, AmountCell::masked());
        assert!(!format!("{cell:?}").contains("42"));
        assert!(!format!("{cell:?}").contains("123"));
    }

    #[test]
    fn unknown_and_zero_are_distinct() {
        let unknown = AmountCell::unknown("History is still loading");
        let known_zero = AmountCell::shown("0 SATS", None);
        assert_ne!(unknown, known_zero);
        assert_eq!(unknown.primary, "—");
    }
}
