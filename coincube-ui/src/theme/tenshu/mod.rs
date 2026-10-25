//! Tenshu v113 design tokens for the namespaced components.
//!
//! Colours come from the shell palette (`theme.colors.shell`), the single table
//! of approved v113 colours. This module adds the geometry, type sizes and the
//! few derived colours the artboards define. Values follow the v113 artboard
//! source (`design-review/canvas/P-Overview.dc.html`), which outranks the
//! hand-off summary where the two differ: wallet cards use a 16px radius.

pub mod button;
pub mod chip;
pub mod surface;
pub mod text;

use super::Theme;
use iced::Color;

/// Corner radii.
pub mod radius {
    /// Quiet text actions (`.rlink`).
    pub const LINK: f32 = 4.0;
    /// Menu rows and segmented-choice segments.
    pub const ROW: f32 = 6.0;
    /// Compact buttons, icon actions, inputs and the Cube switcher.
    pub const CONTROL: f32 = 8.0;
    /// Menus, popovers and sheets.
    pub const MENU: f32 = 10.0;
    /// Wallet cards.
    pub const CARD: f32 = 16.0;
    /// Status and account chips.
    pub const CHIP: f32 = 16.0;
    /// 36px pill actions.
    pub const PILL: f32 = 18.0;
}

/// Control and row dimensions.
pub mod size {
    pub const PILL_HEIGHT: f32 = 36.0;
    pub const BUTTON_HEIGHT: f32 = 32.0;
    pub const CHIP_HEIGHT: f32 = 32.0;
    /// Square icon-only actions such as the balance eye.
    pub const ICON_ACTION: f32 = 32.0;
    pub const ICON: f32 = 18.0;
    pub const ICON_SMALL: f32 = 16.0;
    /// Asset and coin marks.
    pub const COIN: f32 = 32.0;
    pub const ASSET_ROW_HEIGHT: f32 = 56.0;
    pub const TICKER_WIDTH: f32 = 64.0;
}

/// Spacing steps used by the Overview and wallet pages.
pub mod space {
    pub const XS: f32 = 4.0;
    pub const S: f32 = 8.0;
    pub const M: f32 = 12.0;
    /// Card padding.
    pub const L: f32 = 20.0;
    /// Gap between page sections.
    pub const XL: f32 = 36.0;
}

/// Text sizes in logical pixels. Body text is 14px with a 1.45 line height.
pub mod type_size {
    pub const CAPTION: f32 = 12.0;
    pub const CHIP: f32 = 13.0;
    pub const MONO: f32 = 13.0;
    pub const BODY: f32 = 14.0;
    pub const SECTION: f32 = 18.0;
    pub const TITLE: f32 = 20.0;
    pub const AMOUNT_CARD: f32 = 26.0;
    pub const AMOUNT_HERO: f32 = 40.0;
    pub const LINE_HEIGHT: f32 = 1.45;
}

/// Ink on the accent fill (`--on-accent`), the same in both themes.
pub const ON_ACCENT: Color = iced::color!(0x1a1208);
/// Accent fill under the pointer (`.btn.pri:hover`).
pub const ACCENT_HOVER: Color = iced::color!(0xff9a3f);
/// Opacity of a disabled control (`.btn:disabled`).
pub const DISABLED_ALPHA: f32 = 0.45;

/// Semantic colour of a status, chip or badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Confirmed, open, connected.
    Good,
    /// Backup not confirmed, reconnect required, pending.
    Warn,
    /// Errors and failed payments.
    Bad,
    /// Unknown, not checked, secondary text.
    Muted,
    /// Ordinary ink.
    Neutral,
    /// Accent text such as links.
    Accent,
}

impl Tone {
    pub fn color(self, theme: &Theme) -> Color {
        let shell = &theme.colors.shell;
        match self {
            Tone::Good => shell.good,
            Tone::Warn => shell.warn,
            Tone::Bad => shell.bad,
            Tone::Muted => shell.muted,
            Tone::Neutral => shell.ink,
            Tone::Accent => shell.accent_ink,
        }
    }
}

/// `color` with its opacity scaled by `factor`.
pub fn faded(color: Color, factor: f32) -> Color {
    Color {
        a: color.a * factor,
        ..color
    }
}

/// Source-over composite of `top` on an opaque `base`.
pub fn over(base: Color, top: Color) -> Color {
    let blend = |b: f32, t: f32| t * top.a + b * (1.0 - top.a);
    Color {
        r: blend(base.r, top.r),
        g: blend(base.g, top.g),
        b: blend(base.b, top.b),
        a: 1.0,
    }
}

/// Linear interpolation from `from` toward `to`.
pub fn mix(from: Color, to: Color, amount: f32) -> Color {
    let lerp = |a: f32, b: f32| a + (b - a) * amount;
    Color {
        r: lerp(from.r, to.r),
        g: lerp(from.g, to.g),
        b: lerp(from.b, to.b),
        a: lerp(from.a, to.a),
    }
}

/// The artboards define no pressed state. Pressing an accent fill darkens it
/// toward its own ink so the three pointer states stay distinguishable.
pub fn accent_pressed(theme: &Theme) -> Color {
    mix(theme.colors.shell.accent, ON_ACCENT, 0.15)
}

/// WCAG 2.x relative luminance of an opaque colour.
pub fn luminance(color: Color) -> f32 {
    let channel = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
}

/// WCAG 2.x contrast ratio between two opaque colours.
pub fn contrast(a: Color, b: Color) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (light, dark) = if la > lb { (la, lb) } else { (lb, la) };
    (light + 0.05) / (dark + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compositing_helpers_match_css_source_over() {
        let base = iced::color!(0x202020);
        let hover = iced::color!(0xf5f0e8, 0.06);
        let out = over(base, hover);
        let expected = 0.06 * (0xf5 as f32 / 255.0) + 0.94 * (0x20 as f32 / 255.0);
        assert!((out.r - expected).abs() < 1e-6);
        assert_eq!(out.a, 1.0);
        assert_eq!(mix(base, ON_ACCENT, 0.0), base);
        assert_eq!(faded(ON_ACCENT, DISABLED_ALPHA).a, DISABLED_ALPHA);
    }

    #[test]
    fn contrast_ratio_matches_wcag_reference_values() {
        assert!((contrast(Color::BLACK, Color::WHITE) - 21.0).abs() < 1e-3);
        assert!((contrast(Color::WHITE, Color::WHITE) - 1.0).abs() < 1e-6);
    }
}
