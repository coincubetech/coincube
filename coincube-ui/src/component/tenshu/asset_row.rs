//! Compact asset row (`.ar`): coin mark, ticker, balance and actions on one
//! 56px line. The balance obeys the same fail-closed visibility as
//! [`super::balance`]; the coin, ticker and actions carry no amount.

use iced::{alignment, Alignment, Length};

use crate::component::tenshu::balance::{balance, AmountVisibility, BalanceTier, BalanceValue};
use crate::component::tenshu::text;
use crate::theme::tenshu::{size, space};
use crate::widget::{Container, Element, Row};

pub fn asset_row<'a, M: 'a>(
    coin: impl Into<Element<'a, M>>,
    ticker: &'a str,
    value: BalanceValue<'a>,
    visibility: AmountVisibility,
    actions: impl IntoIterator<Item = Element<'a, M>>,
) -> Element<'a, M> {
    let coin = Container::new(coin)
        .width(Length::Fixed(size::COIN))
        .height(Length::Fixed(size::COIN))
        .align_x(alignment::Horizontal::Center)
        .align_y(alignment::Vertical::Center);
    let actions = actions.into_iter().fold(
        Row::new().spacing(space::S).align_y(Alignment::Center),
        |row, action| row.push(action),
    );
    Row::new()
        .spacing(space::M)
        .align_y(Alignment::Center)
        .height(Length::Fixed(size::ASSET_ROW_HEIGHT))
        .push(coin)
        .push(text::label(ticker).width(Length::Fixed(size::TICKER_WIDTH)))
        .push(Container::new(balance(value, BalanceTier::Row, visibility)).width(Length::Fill))
        .push(actions)
        .into()
}
