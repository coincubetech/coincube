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
/// (`[aria-pressed="true"]`, `input:checked + label`) takes the selected fill,
/// ink and a 1px accent outline, as App preferences draws it. Under the
/// pointer an unselected segment takes the selected fill without the outline.
pub fn segment(
    theme: &Theme,
    status: Status,
    selected: bool,
    segment_radius: f32,
) -> button::Style {
    let shell = &theme.colors.shell;
    let disabled = status == Status::Disabled;
    let pointed = matches!(status, Status::Hovered | Status::Pressed);
    let ink = if selected || pointed {
        shell.ink
    } else {
        shell.muted
    };
    // App preferences fills an unselected segment with the selected colour
    // under the pointer (`.seg label:hover`); pressing adds one hover layer.
    let fill = match status {
        Status::Pressed => Some(over(shell.selected, shell.hover)),
        Status::Hovered => Some(shell.selected),
        Status::Active | Status::Disabled => selected.then_some(shell.selected),
    };
    button::Style {
        background: fill.map(|fill| Background::Color(fade_if(disabled, fill))),
        text_color: fade_if(disabled, ink),
        border: Border {
            color: if selected {
                fade_if(disabled, shell.accent)
            } else {
                Color::TRANSPARENT
            },
            width: if selected { 1.0 } else { 0.0 },
            radius: segment_radius.into(),
        },
        ..button::Style::default()
    }
}

/// A scope chip (`.sb`): transparent with a 1px line; hover lays the hover
/// overlay; the selected chip takes the selected fill and an accent outline.
pub fn choice_chip(theme: &Theme, status: Status, selected: bool) -> button::Style {
    let shell = &theme.colors.shell;
    let disabled = status == Status::Disabled;
    let base = selected.then_some(shell.selected);
    button::Style {
        background: pointer_fill(theme, status, base)
            .map(|fill| Background::Color(fade_if(disabled, fill))),
        text_color: fade_if(disabled, shell.ink),
        border: Border {
            color: fade_if(disabled, if selected { shell.accent } else { shell.line }),
            width: 1.0,
            radius: radius::CHIP.into(),
        },
        ..button::Style::default()
    }
}

/// A section tab (`.tab`): muted text, ink under the pointer and when
/// current. It has no fill; the underline is drawn by [`tab_underline`].
pub fn tab(theme: &Theme, status: Status, selected: bool) -> button::Style {
    let shell = &theme.colors.shell;
    let disabled = status == Status::Disabled;
    let ink = if selected || matches!(status, Status::Hovered | Status::Pressed) {
        shell.ink
    } else {
        shell.muted
    };
    button::Style {
        background: (status == Status::Pressed).then_some(Background::Color(shell.hover)),
        text_color: fade_if(disabled, ink),
        border: Border {
            radius: radius::ROW.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// The current tab's 2px accent underline; other tabs keep the space clear.
pub fn tab_underline(theme: &Theme, selected: bool) -> container::Style {
    container::Style {
        background: selected.then_some(Background::Color(theme.colors.shell.accent)),
        ..container::Style::default()
    }
}

/// A section-list row (`.ni`): no fill until hovered or current, both of
/// which take the selected fill.
pub fn nav_item(theme: &Theme, status: Status, selected: bool) -> button::Style {
    let shell = &theme.colors.shell;
    let disabled = status == Status::Disabled;
    let fill = match status {
        Status::Pressed => Some(over(shell.selected, shell.hover)),
        Status::Hovered => Some(shell.selected),
        Status::Active | Status::Disabled => selected.then_some(shell.selected),
    };
    button::Style {
        background: fill.map(|fill| Background::Color(fade_if(disabled, fill))),
        text_color: fade_if(disabled, shell.ink),
        border: Border {
            radius: radius::CONTROL.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// A radio row (`.radio`): no fill; the pointer lays the hover overlay;
/// disabled rows (`.radio.off`) turn muted.
pub fn radio_row(theme: &Theme, status: Status) -> button::Style {
    let shell = &theme.colors.shell;
    button::Style {
        background: pointer_fill(theme, status, None).map(Background::Color),
        text_color: if status == Status::Disabled {
            shell.muted
        } else {
            shell.ink
        },
        border: Border {
            radius: radius::ROW.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// The radio's 16px ring: muted, or the accent when selected.
pub fn radio_ring(theme: &Theme, selected: bool, enabled: bool) -> container::Style {
    let shell = &theme.colors.shell;
    container::Style {
        border: Border {
            color: fade_if(!enabled, if selected { shell.accent } else { shell.muted }),
            width: 1.5,
            radius: (super::size::RADIO / 2.0).into(),
        },
        ..container::Style::default()
    }
}

/// The selected radio's 8px accent dot.
pub fn radio_dot(theme: &Theme, selected: bool, enabled: bool) -> container::Style {
    container::Style {
        background: selected
            .then(|| Background::Color(fade_if(!enabled, theme.colors.shell.accent))),
        border: Border {
            radius: (super::size::RADIO_DOT / 2.0).into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// A disclosure row (`.dsc`): no fill; the pointer lays the hover overlay.
pub fn disclosure(theme: &Theme, status: Status) -> button::Style {
    let shell = &theme.colors.shell;
    button::Style {
        background: pointer_fill(theme, status, None).map(Background::Color),
        text_color: fade_if(status == Status::Disabled, shell.ink),
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

/// A read-only value in a field box (`.cpy .inp`): same fill, border and
/// radius as a resting field.
pub fn value_box(theme: &Theme, field_radius: f32) -> container::Style {
    let shell = &theme.colors.shell;
    container::Style {
        text_color: Some(shell.ink),
        background: Some(Background::Color(shell.background)),
        border: Border {
            color: shell.line,
            width: 1.0,
            radius: field_radius.into(),
        },
        ..container::Style::default()
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
