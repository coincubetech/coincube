//! A compact calendar trigger with an overlay that does not enlarge its form.

use chrono::NaiveDate;
use iced_aw::DatePicker;

use crate::{component::button, widget::Element};

/// Render an iced_aw calendar beside an editable date field.
///
/// Only `on_select` commits a value. Opening and cancelling leave the caller's
/// text untouched. The closed branch is a plain button, so closing discards
/// iced_aw's internal month/day state; every new open starts at `initial_date`
/// even after a cancelled navigation or an intervening typed edit.
pub fn calendar_button<'a, Message: Clone + 'static>(
    show: bool,
    initial_date: NaiveDate,
    on_open: Message,
    on_cancel: Message,
    on_select: impl Fn(NaiveDate) -> Message + 'static,
) -> Element<'a, Message> {
    let trigger = button::secondary_compact(None, "Choose date").on_press(on_open);
    if !show {
        return trigger.into();
    }

    DatePicker::new(true, initial_date, trigger, on_cancel, move |date| {
        on_select(date.into())
    })
    .font_size(16)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::widget::{tree::Tag, Tree};

    #[derive(Debug, Clone)]
    #[allow(dead_code)] // Widget-construction test never dispatches messages.
    enum Message {
        Open,
        Cancel,
        Selected(NaiveDate),
    }

    #[test]
    fn closed_calendar_drops_picker_state_before_the_next_open() {
        let date = NaiveDate::from_ymd_opt(2020, 3, 5).unwrap();
        let closed = calendar_button(
            false,
            date,
            Message::Open,
            Message::Cancel,
            Message::Selected,
        );
        let opened = calendar_button(
            true,
            date,
            Message::Open,
            Message::Cancel,
            Message::Selected,
        );
        assert_ne!(closed.as_widget().tag(), opened.as_widget().tag());
        assert_eq!(
            opened.as_widget().tag(),
            Tag::of::<iced_aw::widget::date_picker::State>()
        );
        let mut tree = Tree::new(opened.as_widget());
        tree.diff(closed.as_widget());
        assert_eq!(tree.tag, closed.as_widget().tag());

        let next_date = NaiveDate::from_ymd_opt(2022, 11, 9).unwrap();
        let reopened = calendar_button(
            true,
            next_date,
            Message::Open,
            Message::Cancel,
            Message::Selected,
        );
        tree.diff(reopened.as_widget());
        assert_eq!(tree.tag, opened.as_widget().tag());
    }
}
