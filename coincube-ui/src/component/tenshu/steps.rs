//! A flow's steps on a page head's rule (`.hsteps`, `.step`): a numbered disc
//! and a label per step. The current step is ink, semibold and underlined in
//! the accent, its disc filled with the accent; a finished step's disc takes
//! the "good" colour (`li.done .n`); the rest are muted. Steps are progress,
//! not navigation, so none of them is a control.
//!
//! Place the row with [`PageHead::rule`](super::PageHead::rule): its
//! underline then sits on the head's 1px rule, as the section tabs do.

use iced::{Alignment, Background, Border, Length};

use crate::font;
use crate::theme::tenshu::{text as style, type_size, ON_ACCENT};
use crate::theme::Theme;
use crate::widget::{Column, Container, Element, Row};

/// `.steps { gap: 4px }`.
const STEP_GAP: f32 = 4.0;
/// `.step { padding: 10px 12px }`.
const STEP_PAD_Y: f32 = 10.0;
const STEP_PAD_X: f32 = 12.0;
/// `.step { gap: 8px }`, between the disc and the label.
const MARK_GAP: f32 = 8.0;
/// `.step .n { width: 20px; height: 20px }`.
const MARK: f32 = 20.0;
/// `.step { border-bottom: 2px }`.
const UNDERLINE: f32 = 2.0;

/// `labels` in order, numbered from 1, with `current` (0-based) marked. A
/// `current` past the last label marks none.
pub fn steps<'a, M: 'a>(labels: &[&'a str], current: usize) -> Element<'a, M> {
    labels
        .iter()
        .enumerate()
        .fold(Row::new().spacing(STEP_GAP), |row, (index, label)| {
            let state = step_state(index, current);
            row.push(step(
                index + 1,
                label,
                state == StepState::Current,
                state == StepState::Done,
            ))
        })
        .wrap()
        .into()
}

/// How a step at `index` reads when `current` is the step in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Done,
    Current,
    Ahead,
}

pub fn step_state(index: usize, current: usize) -> StepState {
    match index.cmp(&current) {
        std::cmp::Ordering::Less => StepState::Done,
        std::cmp::Ordering::Equal => StepState::Current,
        std::cmp::Ordering::Greater => StepState::Ahead,
    }
}

fn step<'a, M: 'a>(number: usize, label: &'a str, current: bool, done: bool) -> Element<'a, M> {
    let mark = Container::new(
        iced::widget::text(number.to_string())
            .size(type_size::CAPTION)
            .font(font::SEMIBOLD)
            .style(move |theme: &Theme| iced::widget::text::Style {
                color: Some(if current {
                    ON_ACCENT
                } else if done {
                    theme.colors.shell.good
                } else {
                    theme.colors.shell.muted
                }),
            }),
    )
    .center_x(Length::Fixed(MARK))
    .center_y(Length::Fixed(MARK))
    .style(move |theme: &Theme| iced::widget::container::Style {
        background: current.then_some(Background::Color(theme.colors.shell.accent)),
        border: Border {
            color: if current {
                theme.colors.shell.accent
            } else if done {
                theme.colors.shell.good
            } else {
                theme.colors.shell.line
            },
            width: 1.0,
            radius: (MARK / 2.0).into(),
        },
        ..Default::default()
    });
    let label = super::text::body(label)
        .font(if current {
            font::SEMIBOLD
        } else {
            font::REGULAR
        })
        .style(if current { style::ink } else { style::muted });
    let underline = Container::new(iced::widget::Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(UNDERLINE))
        .style(move |theme: &Theme| iced::widget::container::Style {
            background: current.then_some(Background::Color(theme.colors.shell.accent)),
            ..Default::default()
        });
    // The underline fills the step's width; set the width after the pushes,
    // since pushing a Fill child widens a column to Fill.
    Column::new()
        .spacing(STEP_PAD_Y)
        .push(
            Container::new(
                Row::new()
                    .spacing(MARK_GAP)
                    .align_y(Alignment::Center)
                    .push(mark)
                    .push(label),
            )
            .padding([0.0, STEP_PAD_X]),
        )
        .push(underline)
        .width(Length::Shrink)
        .padding(iced::Padding::default().top(STEP_PAD_Y))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_before_the_current_one_are_done() {
        let states: Vec<_> = (0..4).map(|index| step_state(index, 2)).collect();
        assert_eq!(
            states,
            [
                StepState::Done,
                StepState::Done,
                StepState::Current,
                StepState::Ahead
            ]
        );
        // Past the last step: every step is finished (Transfer's sent state).
        assert!((0..3).all(|index| step_state(index, 3) == StepState::Done));
    }
}
