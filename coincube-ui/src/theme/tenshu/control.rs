//! Styles for Tenshu form controls: toggles, segmented choices and text fields.
//!
//! Toggles follow `.tog` (Settings, Overview) and `.sw` (App preferences),
//! segmented choices `.seg`, and fields `.inp` (Send, Receive) and `.ctl`
//! (Activity filters) in the v113 artboards. Pointer states reuse the button
//! rules: hover lays the shell `hover` overlay over the fill, pressed stacks it
//! twice or darkens an accent fill, and disabled fades to `DISABLED_ALPHA`.

use iced::widget::button::{self, Status};
use iced::widget::{container, text_input};
use iced::{Background, Border, Color};

use super::{accent_pressed, faded, over, radius, Theme, ACCENT_HOVER, DISABLED_ALPHA, ON_ACCENT};

fn fade_if(disabled: bool, color: Color) -> Color {
    if disabled {
        faded(color, DISABLED_ALPHA)
    } else {
        color
    }
}

/// `fill` under the pointer: one hover layer, two when pressed.
fn pointer_fill(theme: &Theme, status: Status, fill: Option<Color>) -> Option<Color> {
    let hover = theme.colors.shell.hover;
    let layered = |base: Color| match status {
        Status::Hovered => over(base, hover),
        Status::Pressed => over(over(base, hover), hover),
        Status::Active | Status::Disabled => base,
    };
    match (status, fill) {
        (Status::Active | Status::Disabled, fill) => fill,
        (_, Some(base)) => Some(layered(base)),
        (Status::Hovered, None) => Some(hover),
        (_, None) => Some(Color {
            a: 1.0 - (1.0 - hover.a) * (1.0 - hover.a),
            ..hover
        }),
    }
}

// ----- Toggle -------------------------------------------------------------

/// The pill toggle's outer button (`.tog`): transparent, 1px line, 16px radius.
pub fn toggle_pill(theme: &Theme, status: Status) -> button::Style {
    let shell = &theme.colors.shell;
    let disabled = status == Status::Disabled;
    button::Style {
        background: pointer_fill(theme, status, None).map(Background::Color),
        text_color: fade_if(disabled, shell.ink),
        border: Border {
            color: fade_if(disabled, shell.line),
            width: 1.0,
            radius: radius::CHIP.into(),
        },
        ..button::Style::default()
    }
}

/// The pill toggle's 30×18 track (`.tog .kn`): line when off, accent when on.
pub fn toggle_pill_track(theme: &Theme, is_on: bool, enabled: bool) -> container::Style {
    let shell = &theme.colors.shell;
    let fill = if is_on { shell.accent } else { shell.line };
    container::Style {
        background: Some(Background::Color(fade_if(!enabled, fill))),
        border: Border {
            radius: (super::size::TOGGLE_TRACK_HEIGHT / 2.0).into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// The 40×24 switch (`.sw`): a muted outline when off, an accent fill when on.
pub fn switch(theme: &Theme, status: Status, is_on: bool) -> button::Style {
    let shell = &theme.colors.shell;
    let disabled = status == Status::Disabled;
    let (fill, border) = if is_on {
        let fill = match status {
            Status::Hovered => ACCENT_HOVER,
            Status::Pressed => accent_pressed(theme),
            Status::Active | Status::Disabled => shell.accent,
        };
        (Some(fill), fill)
    } else {
        (pointer_fill(theme, status, None), shell.muted)
    };
    button::Style {
        background: fill.map(|fill| Background::Color(fade_if(disabled, fill))),
        text_color: fade_if(disabled, shell.ink),
        border: Border {
            color: fade_if(disabled, border),
            width: 1.0,
            radius: (super::size::SWITCH_HEIGHT / 2.0).into(),
        },
        ..button::Style::default()
    }
}

/// A toggle thumb. Off it takes the track's contrasting ink (`--ink` on the
/// pill's line track, `--muted` inside the switch outline); on, the accent ink.
pub fn thumb(theme: &Theme, is_on: bool, enabled: bool, pill: bool) -> container::Style {
    let shell = &theme.colors.shell;
    let fill = match (is_on, pill) {
        (true, _) => ON_ACCENT,
        (false, true) => shell.ink,
        (false, false) => shell.muted,
    };
    container::Style {
        background: Some(Background::Color(fade_if(!enabled, fill))),
        border: Border {
            radius: f32::MAX.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

// ----- Segmented choice ---------------------------------------------------

/// The segmented group's frame (`.seg`): window background, 1px line.
pub fn segmented_frame(theme: &Theme, frame_radius: f32) -> container::Style {
    let shell = &theme.colors.shell;
    container::Style {
        background: Some(Background::Color(shell.background)),
        border: Border {
            color: shell.line,
            width: 1.0,
            radius: frame_radius.into(),
        },
        ..container::Style::default()
    }
}

/// One segment (`.seg button`): muted text on no fill; the selected segment
/// (`[aria-pressed="true"]`) takes the selected fill and ink.
pub fn segment(
    theme: &Theme,
    status: Status,
    selected: bool,
    segment_radius: f32,
) -> button::Style {
    let shell = &theme.colors.shell;
    let disabled = status == Status::Disabled;
    let base = selected.then_some(shell.selected);
    let ink = if selected || matches!(status, Status::Hovered | Status::Pressed) {
        shell.ink
    } else {
        shell.muted
    };
    button::Style {
        background: pointer_fill(theme, status, base)
            .map(|fill| Background::Color(fade_if(disabled, fill))),
        text_color: fade_if(disabled, ink),
        border: Border {
            radius: segment_radius.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

// ----- Text field ---------------------------------------------------------

/// A text field (`.inp`, `.ctl`): window background, 1px line; the pointer
/// raises the border to muted, an error turns it bad, and keyboard focus draws
/// a 2px focus border (the artboard's focus border plus its 1px focus shadow).
pub fn field(
    theme: &Theme,
    status: text_input::Status,
    field_radius: f32,
    invalid: bool,
) -> text_input::Style {
    let shell = &theme.colors.shell;
    let (border_color, border_width) = match status {
        text_input::Status::Focused { .. } => (shell.focus, 2.0),
        _ if invalid => (shell.bad, 1.0),
        text_input::Status::Hovered => (shell.muted, 1.0),
        text_input::Status::Active => (shell.line, 1.0),
        text_input::Status::Disabled => (faded(shell.line, DISABLED_ALPHA), 1.0),
    };
    let disabled = status == text_input::Status::Disabled;
    text_input::Style {
        background: Background::Color(shell.background),
        border: Border {
            color: border_color,
            width: border_width,
            radius: field_radius.into(),
        },
        icon: shell.muted,
        placeholder: if disabled {
            faded(shell.muted, DISABLED_ALPHA)
        } else {
            shell.muted
        },
        value: if disabled { shell.muted } else { shell.ink },
        selection: faded(shell.focus, 0.35),
    }
}

/// A storage tag beside a setting label (`.tag`, "This device").
pub fn tag(theme: &Theme) -> container::Style {
    let shell = &theme.colors.shell;
    container::Style {
        text_color: Some(shell.muted),
        border: Border {
            color: shell.line,
            width: 1.0,
            radius: radius::LINK.into(),
        },
        ..container::Style::default()
    }
}

/// The 1px rule under a setting row.
pub fn divider(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(theme.colors.shell.line)),
        ..container::Style::default()
    }
}
