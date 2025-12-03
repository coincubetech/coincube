use crate::app::view::{
    buysell::{panel::BuyOrSell, MavapayFlowStep, MavapayState},
    BuySellMessage, MavapayMessage, Message as ViewMessage,
};

use iced::{widget::*, Alignment, Length};

use coincube_ui::component::{button, text};
use coincube_ui::{color, theme, widget::Column};

// TODO: Use labels instead of placeholders for all input forms
pub fn form<'a>(state: &'a MavapayState) -> iced::Element<'a, ViewMessage, theme::Theme> {
    let form = match &state.step {
        MavapayFlowStep::Transaction { .. } => transactions_form,
        // TODO: Implement checkout UI, and subscription for SSE events
        MavapayFlowStep::Checkout { .. } => unimplemented!("Checkout UI"),
    };

    let element: iced::Element<'a, BuySellMessage, theme::Theme> = form(state).into();
    element.map(|b| ViewMessage::BuySell(b))
}

fn transactions_form<'a>(state: &'a MavapayState) -> Column<'a, BuySellMessage> {
    use coincube_ui::icon::bitcoin_icon;

    let MavapayFlowStep::Transaction {
        amount,
        current_price,
        buy_or_sell,
        ..
    } = &state.step
    else {
        unreachable!()
    };

    let header = iced::widget::row![
        Space::with_width(Length::Fill),
        text::h4_bold("Bitcoin ↔ Fiat Exchange").color(color::WHITE),
        Space::with_width(Length::Fill),
    ]
    .align_y(Alignment::Center);

    // Current price display
    let mut price_display = iced::widget::column![header, Space::with_height(Length::Fixed(20.0))];

    // TODO: Replace with realtime BTC-fiat conversion display
    if let Some(price) = current_price {
        price_display = price_display
            .push(
                Container::new(
                    Row::new()
                        .push(
                            text(format!(
                                "1 SAT = {:.4} {}",
                                price.btc_price_in_unit_currency / 100_000_000.0,
                                price.currency
                            ))
                            .size(16)
                            .color(color::WHITE),
                        )
                        .push(Space::with_width(Length::Fill))
                        .push(bitcoin_icon().size(20).color(color::ORANGE))
                        .align_y(Alignment::Center),
                )
                .padding(15)
                .style(theme::card::simple)
                .width(Length::Fixed(600.0)), // Match form width
            )
            .push(Space::with_height(Length::Fixed(15.0)));
    }

    // Exchange form with payment mode radio buttons
    let beneficiary_input_form = iced::widget::column![
        Space::with_height(Length::Fixed(15.0)),
        // Amount field (common to both modes)
        text("Amount in BTCSAT").size(14).color(color::GREY_3),
        Space::with_height(Length::Fixed(5.0)),
        Container::new(
            iced_aw::number_input(amount, .., |a| {
                BuySellMessage::Mavapay(MavapayMessage::AmountChanged(a))
            })
            .size(14)
            .padding(10),
        )
        .width(Length::Fixed(200.0)),
        // TODO: Display source/target currencies, with realtime conversion rate
        match buy_or_sell {
            BuyOrSell::Buy { address: _ } => {
                // TODO: display input amount, generated address and bank deposit details.
                Space::with_height(0)
            }
            BuyOrSell::Sell => {
                // TODO: display onchain bitcoin address for deposit, and beneficiary input forms
                // TODO: If country uses BankTransfer, render banks selector dropdown
                Space::with_height(0)
            }
        },
        button::primary(None, "Process Payment")
            .on_press(BuySellMessage::Mavapay(MavapayMessage::CreateQuote))
            .width(Length::Fill)
    ]
    .spacing(10)
    .align_x(Alignment::Center)
    .width(Length::Fill);

    // combine UI, render beneficiary input form using card styling
    price_display.push(
        Container::new(beneficiary_input_form)
            .padding(20)
            .style(theme::card::simple)
            .width(Length::Fixed(600.0)),
    )
}
