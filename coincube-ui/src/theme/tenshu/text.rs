//! Text colours for Tenshu typography.

use iced::widget::text::Style;

use super::{Theme, Tone};

pub fn ink(theme: &Theme) -> Style {
    tone(theme, Tone::Neutral)
}

pub fn muted(theme: &Theme) -> Style {
    tone(theme, Tone::Muted)
}

pub fn accent(theme: &Theme) -> Style {
    tone(theme, Tone::Accent)
}

pub fn tone(theme: &Theme, tone: Tone) -> Style {
    Style {
        color: Some(tone.color(theme)),
    }
}
