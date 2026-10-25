//! Tenshu typography: IBM Plex Sans for UI text, IBM Plex Mono for addresses
//! and identifiers. Plex Sans digits share one advance width, so amounts set
//! in it line up as tabular figures.

use iced::advanced::text::Shaping;
use iced::widget::text::{IntoFragment, LineHeight};

use crate::font;
use crate::theme::tenshu::{text as style, type_size};
use crate::widget::Text;

fn base<'a>(content: impl IntoFragment<'a>, size: f32) -> Text<'a> {
    iced::widget::text(content)
        .shaping(Shaping::Advanced)
        .size(size)
        .line_height(LineHeight::Relative(type_size::LINE_HEIGHT))
}

/// Page title, 20px semibold (`.ttl h1`).
pub fn title<'a>(content: impl IntoFragment<'a>) -> Text<'a> {
    base(content, type_size::TITLE)
        .font(font::SEMIBOLD)
        .style(style::ink)
}

/// Section heading, 18px semibold (`.ttl h2`).
pub fn section<'a>(content: impl IntoFragment<'a>) -> Text<'a> {
    base(content, type_size::SECTION)
        .font(font::SEMIBOLD)
        .style(style::ink)
}

/// Body text, 14px regular.
pub fn body<'a>(content: impl IntoFragment<'a>) -> Text<'a> {
    base(content, type_size::BODY)
        .font(font::REGULAR)
        .style(style::ink)
}

/// Emphasised body text such as a ticker, 14px medium.
pub fn label<'a>(content: impl IntoFragment<'a>) -> Text<'a> {
    base(content, type_size::BODY)
        .font(font::MEDIUM)
        .style(style::ink)
}

/// Caption, 12px muted (`.cap`).
pub fn caption<'a>(content: impl IntoFragment<'a>) -> Text<'a> {
    base(content, type_size::CAPTION)
        .font(font::REGULAR)
        .style(style::muted)
}

/// Address, txid or other identifier, 13px IBM Plex Mono (`.mono`).
pub fn mono<'a>(content: impl IntoFragment<'a>) -> Text<'a> {
    base(content, type_size::MONO)
        .font(font::MONO_REGULAR)
        .style(style::ink)
}
