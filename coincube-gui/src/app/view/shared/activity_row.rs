//! One recent-activity grammar for wallet landings and the later Activity table.
//! The caller owns the source order, stable identity, and action destination.

use coincube_ui::{
    component::text::{text, P1_SIZE, P2_SIZE},
    icon, theme,
    widget::{Column, ColumnExt, Element, Row, RowExt},
};
use iced::{
    widget::{button, container, text::Wrapping, tooltip},
    Alignment, Length,
};

use super::landing::{amount_cell, AmountCell, AmountTier};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityIdentity {
    pub cube: String,
    pub wallet: String,
    pub payment: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityDirection {
    Incoming,
    Outgoing,
    SelfTransfer,
}

impl ActivityDirection {
    fn label(self) -> &'static str {
        match self {
            Self::Incoming => "Received",
            Self::Outgoing => "Sent",
            Self::SelfTransfer => "Self-transfer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityStatus {
    Created,
    Pending,
    Complete,
    Failed,
    TimedOut,
    Refundable,
    RefundPending,
    WaitingFeeAcceptance,
    Unconfirmed,
    ReviewRequired,
}

impl ActivityStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Created => "Created",
            Self::Pending => "Pending",
            Self::Complete => "Complete",
            Self::Failed => "Failed",
            Self::TimedOut => "Timed out",
            Self::Refundable => "Refund available",
            Self::RefundPending => "Refund pending",
            Self::WaitingFeeAcceptance => "Review fee",
            Self::Unconfirmed => "Unconfirmed",
            Self::ReviewRequired => "Review required",
        }
    }
}

impl From<crate::app::wallets::DomainPaymentStatus> for ActivityStatus {
    fn from(value: crate::app::wallets::DomainPaymentStatus) -> Self {
        use crate::app::wallets::DomainPaymentStatus as S;
        match value {
            S::Created => Self::Created,
            S::Pending => Self::Pending,
            S::Complete => Self::Complete,
            S::Failed => Self::Failed,
            S::TimedOut => Self::TimedOut,
            S::Refundable => Self::Refundable,
            S::RefundPending => Self::RefundPending,
            S::WaitingFeeAcceptance => Self::WaitingFeeAcceptance,
        }
    }
}

pub struct ActivityRow {
    pub identity: ActivityIdentity,
    pub rail: Option<String>,
    pub date: String,
    pub timezone_tooltip: Option<String>,
    pub direction: ActivityDirection,
    pub label: String,
    pub wallet_caption: Option<String>,
    pub amount: AmountCell,
    pub status: ActivityStatus,
}

/// `wallet_column` adds the Activity table's wallet cell; compact landing rows
/// omit it. The action comes from the caller's current selection mapping; an
/// unidentifiable or ambiguous source row remains visible but cannot open.
pub fn activity_row<'a, M: Clone + 'a>(
    data: ActivityRow,
    wallet_column: bool,
    open: Option<M>,
) -> Element<'a, M> {
    let direction_icon: Element<'a, M> = match data.direction {
        ActivityDirection::Incoming => icon::arrow_down().size(P1_SIZE).into(),
        ActivityDirection::Outgoing => icon::arrow_right().size(P1_SIZE).into(),
        ActivityDirection::SelfTransfer => icon::arrow_down_up_icon().size(P1_SIZE).into(),
    };
    let full_label = data.label.clone();
    let wallet_name = data.identity.wallet.clone();
    let mut visible_label: String = data.label.chars().take(72).collect();
    if data.label.chars().count() > 72 {
        visible_label.push('…');
    }
    let status_style: fn(&theme::Theme) -> iced::widget::container::Style = match data.status {
        ActivityStatus::Complete => theme::pill::success,
        ActivityStatus::Failed | ActivityStatus::TimedOut | ActivityStatus::Refundable => {
            theme::pill::error
        }
        ActivityStatus::Pending
        | ActivityStatus::RefundPending
        | ActivityStatus::WaitingFeeAcceptance
        | ActivityStatus::Unconfirmed => theme::pill::warning,
        ActivityStatus::Created | ActivityStatus::ReviewRequired => theme::pill::simple,
    };
    let date: Element<'a, M> = if let Some(timezone) = data.timezone_tooltip {
        tooltip(
            text(data.date).size(P2_SIZE).style(theme::text::secondary),
            container(text(timezone).size(P2_SIZE))
                .padding([5, 8])
                .style(theme::container::border_grey),
            tooltip::Position::Bottom,
        )
        .into()
    } else {
        text(data.date)
            .size(P2_SIZE)
            .style(theme::text::secondary)
            .into()
    };
    let detail = Column::new()
        .spacing(3)
        .push(
            Row::new()
                .spacing(7)
                .align_y(Alignment::Center)
                .push(direction_icon)
                .push(
                    text(data.direction.label())
                        .size(P2_SIZE)
                        .style(theme::text::secondary),
                )
                .push(
                    text(visible_label)
                        .size(P1_SIZE)
                        .width(Length::Fill)
                        .wrapping(Wrapping::WordOrGlyph),
                ),
        )
        .push_maybe(
            data.wallet_caption
                .or(data.rail)
                .map(|s| text(s).size(P2_SIZE).style(theme::text::secondary)),
        );
    let row = Row::new()
        .spacing(14)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .push(container(date).width(Length::Fixed(115.0)))
        .push(detail.width(Length::Fill))
        .push_maybe(
            wallet_column.then(|| text(wallet_name).size(P2_SIZE).width(Length::Fixed(90.0))),
        )
        .push(
            container(amount_cell::<M>(data.amount, AmountTier::Row))
                .width(Length::Fixed(190.0))
                .align_x(iced::alignment::Horizontal::Right),
        )
        .push(
            container(text(data.status.label()).size(P2_SIZE))
                .padding([4, 8])
                .width(Length::Fixed(126.0))
                .align_x(iced::alignment::Horizontal::Center)
                .style(status_style),
        );
    // The stable identity is deliberately view data rather than part of a label
    // hash. The action opens the exact row represented by the caller's source.
    tooltip(
        button(row)
            .on_press_maybe(open)
            .width(Length::Fill)
            .padding([12, 10])
            .style(theme::button::transparent_border),
        container(text(full_label).size(P2_SIZE))
            .padding([5, 8])
            .style(theme::container::border_grey),
        tooltip::Position::Bottom,
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::ActivityStatus;
    use crate::app::wallets::DomainPaymentStatus as S;

    #[test]
    fn every_domain_payment_status_has_its_own_label() {
        let states = [
            S::Created,
            S::Pending,
            S::Complete,
            S::Failed,
            S::TimedOut,
            S::Refundable,
            S::RefundPending,
            S::WaitingFeeAcceptance,
        ];
        let labels: std::collections::HashSet<_> = states
            .iter()
            .copied()
            .map(|status| ActivityStatus::from(status).label())
            .collect();
        assert_eq!(labels.len(), states.len());
    }
}
