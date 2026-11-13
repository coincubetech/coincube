//! Tenshu surfaces. Both return a [`Container`], so callers can still adjust
//! width, padding or alignment.

use iced::Length;

use crate::theme::tenshu::{space, surface};
use crate::widget::{Container, Element};

/// Wallet card (`.card`): full width, 20px padding.
pub fn card<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Container<'a, M> {
    Container::new(content)
        .padding(space::L)
        .width(Length::Fill)
        .style(surface::card)
}

/// Menu, popover or sheet (`.menu`): 6px padding, shrinks to its content.
pub fn panel<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Container<'a, M> {
    Container::new(content).padding(6.0).style(surface::panel)
}
