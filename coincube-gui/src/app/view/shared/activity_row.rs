//! One recent-activity grammar for wallet landings and the later Activity table.
//! The caller owns the source order, stable identity, and action destination.

use coincube_ui::{
    component::text::{text, Text as _, P1_SIZE, P2_SIZE},
    icon, theme,
    widget::{action::Action, Column, Element, Row, RowExt},
};
use iced::{
    advanced::widget::Id,
    widget::{container, text::Wrapping, tooltip},
    Alignment, Length,
};

use super::landing::{amount_cell, AmountCell, AmountTier};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityIdentity {
    pub cube: String,
    pub wallet: String,
    pub payment: Option<String>,
}

impl ActivityIdentity {
    /// Length-prefix each part so an identifier can neither collide with a
    /// neighbouring Cube nor change when the row's label or date changes.
    fn focus_id(&self) -> Id {
        let payment = self.payment.as_deref().unwrap_or_default();
        Id::from(format!(
            "tenshu.activity.{}:{}:{}:{}:{}:{}",
            self.cube.len(),
            self.cube,
            self.wallet.len(),
            self.wallet,
            payment.len(),
            payment,
        ))
    }
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

fn status_badge_style(theme: &theme::Theme, status: ActivityStatus) -> container::Style {
    let color = match status {
        ActivityStatus::Complete => theme.colors.text.success,
        ActivityStatus::Failed | ActivityStatus::TimedOut | ActivityStatus::Refundable => {
            theme.colors.text.error
        }
        ActivityStatus::Pending
        | ActivityStatus::RefundPending
        | ActivityStatus::WaitingFeeAcceptance
        | ActivityStatus::Unconfirmed => theme.colors.text.warning,
        ActivityStatus::Created | ActivityStatus::ReviewRequired => theme.colors.text.secondary,
    };
    container::Style {
        text_color: Some(color),
        border: iced::Border {
            color,
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    }
}

/// `wallet_column` adds the Activity table's wallet cell; compact landing rows
/// omit it. The action comes from the caller's current selection mapping; an
/// unidentifiable or ambiguous source row remains visible but cannot open.
pub fn activity_row<'a, M: Clone + 'static>(
    data: ActivityRow,
    wallet_column: bool,
    open: Option<M>,
    focus_intent: impl FnOnce(Id) -> M,
) -> Element<'a, M> {
    let direction_icon: Element<'a, M> = match data.direction {
        ActivityDirection::Incoming => icon::arrow_down().size(P1_SIZE).into(),
        ActivityDirection::Outgoing => icon::arrow_right().size(P1_SIZE).into(),
        ActivityDirection::SelfTransfer => icon::arrow_down_up_icon().size(P1_SIZE).into(),
    };
    let full_label = data.label.clone();
    let wallet_name = data.identity.wallet.clone();
    let mut visible_label: String = data.label.chars().take(30).collect();
    if data.label.chars().count() > 30 {
        visible_label.push('…');
    }
    let status = data.status;
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
    let caption = data.wallet_caption.or(data.rail);
    let detail = Column::new()
        .spacing(3)
        .push(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(
                    container(
                        text(visible_label)
                            .size(P1_SIZE)
                            .bold()
                            .wrapping(Wrapping::None),
                    )
                    .clip(true),
                )
                .push(
                    container(text(status.label()).size(P2_SIZE))
                        .padding([2, 7])
                        .style(move |theme| status_badge_style(theme, status)),
                ),
        )
        .push(
            Row::new()
                .spacing(5)
                .push(date)
                .push_maybe(caption.map(|value| {
                    text(format!("· {value}"))
                        .size(P2_SIZE)
                        .style(theme::text::secondary)
                        .wrapping(Wrapping::None)
                })),
        );
    let row = Row::new()
        .spacing(12)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .push(
            container(direction_icon)
                .width(Length::Fixed(28.0))
                .height(Length::Fixed(28.0))
                .align_x(iced::alignment::Horizontal::Center)
                .align_y(iced::alignment::Vertical::Center)
                .style(|theme| container::Style {
                    border: iced::Border {
                        color: theme.colors.shell.line,
                        width: 1.0,
                        radius: 14.0.into(),
                    },
                    ..Default::default()
                }),
        )
        .push(container(detail).width(Length::Fill).clip(true))
        .push_maybe(
            wallet_column.then(|| text(wallet_name).size(P2_SIZE).width(Length::Fixed(90.0))),
        )
        .push(
            container(amount_cell::<M>(data.amount, AmountTier::Row))
                .width(Length::Fixed(150.0))
                .align_x(iced::alignment::Horizontal::Right),
        );
    // The action opens the exact row represented by the caller's source. An
    // unidentifiable/duplicate row remains visible, but Action skips it during
    // focus traversal and resets held key state when its identity is replaced.
    let action = tooltip(
        Action::new(data.identity.focus_id(), row, focus_intent)
            .on_press_maybe(open)
            .width(Length::Fill)
            .padding([8, 4])
            .style(theme::button::transparent_border),
        container(text(format!("{} · {full_label}", data.direction.label())).size(P2_SIZE))
            .padding([5, 8])
            .style(theme::container::border_grey),
        tooltip::Position::Bottom,
    );
    Column::new()
        .push(action)
        .push(iced::widget::rule::horizontal(1))
        .into()
}

#[cfg(test)]
mod tests {
    use super::{activity_row, ActivityDirection, ActivityIdentity, ActivityRow, ActivityStatus};
    use crate::app::view::shared::landing::{capture, AmountCell};
    use crate::app::wallets::DomainPaymentStatus as S;
    use coincube_ui::{font, widget::Element};
    use iced::advanced::{
        layout,
        renderer::Headless,
        widget::{operation, operation::focusable, Id, Operation, Tree},
        Layout,
    };
    use iced::{keyboard, mouse, Event, Rectangle, Size};

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Intent {
        Open(&'static str),
        Focus(Id),
    }

    fn row(payment: Option<&str>, open: Option<Intent>) -> Element<'static, Intent> {
        activity_row(
            ActivityRow {
                identity: ActivityIdentity {
                    cube: "cube-A".into(),
                    wallet: "Spark".into(),
                    payment: payment.map(str::to_owned),
                },
                rail: Some("Lightning".into()),
                date: "Oct 24, 11:45".into(),
                timezone_tooltip: Some("Local time".into()),
                direction: ActivityDirection::Incoming,
                label: "Payment".into(),
                wallet_caption: None,
                amount: AmountCell::shown("1,000 SATS", Some("$0.50".into())),
                status: ActivityStatus::Complete,
            },
            false,
            open,
            Intent::Focus,
        )
    }

    fn id(payment: &str) -> Id {
        ActivityIdentity {
            cube: "cube-A".into(),
            wallet: "Spark".into(),
            payment: Some(payment.into()),
        }
        .focus_id()
    }

    fn operate<T: 'static>(
        element: &mut Element<'_, Intent>,
        tree: &mut Tree,
        node: &layout::Node,
        renderer: &iced::Renderer,
        operation: impl Operation<T> + 'static,
    ) -> Option<T> {
        let mut operation: Box<dyn Operation<T>> = Box::new(operation);
        loop {
            element.as_widget_mut().operate(
                tree,
                Layout::new(node),
                renderer,
                &mut operation::black_box(operation.as_mut()),
            );
            match operation.finish() {
                operation::Outcome::None => return None,
                operation::Outcome::Some(value) => return Some(value),
                operation::Outcome::Chain(next) => operation = next,
            }
        }
    }

    fn key(named: keyboard::key::Named, pressed: bool) -> Event {
        let key = keyboard::Key::Named(named);
        let physical_key =
            keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified);
        if pressed {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key,
                physical_key,
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            })
        } else {
            Event::Keyboard(keyboard::Event::KeyReleased {
                key: key.clone(),
                modified_key: key,
                physical_key,
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
            })
        }
    }

    fn send(
        element: &mut Element<'_, Intent>,
        tree: &mut Tree,
        node: &layout::Node,
        renderer: &iced::Renderer,
        event: Event,
    ) -> Vec<Intent> {
        let mut messages = Vec::new();
        element.as_widget_mut().update(
            tree,
            &event,
            Layout::new(node),
            mouse::Cursor::Unavailable,
            renderer,
            &mut iced::advanced::clipboard::Null,
            &mut iced::advanced::Shell::new(&mut messages),
            &Rectangle::with_size(Size::new(760.0, 600.0)),
        );
        messages
    }

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

    #[test]
    fn production_row_focus_skips_disabled_and_reorder_keeps_identity() {
        for bytes in font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            font::REGULAR,
            14.into(),
            Some("tiny-skia"),
        ))
        .unwrap();
        let make = |reverse: bool| -> Element<'static, Intent> {
            let first = row(Some("payment-A"), Some(Intent::Open("A")));
            let second = row(Some("payment-B"), Some(Intent::Open("B")));
            if reverse {
                iced::widget::column![second, row(Some("duplicate"), None), first].into()
            } else {
                iced::widget::column![first, row(Some("duplicate"), None), second].into()
            }
        };
        let mut element = make(false);
        let mut tree = Tree::new(element.as_widget());
        let limits = layout::Limits::new(Size::ZERO, Size::new(760.0, 600.0));
        let mut node = element
            .as_widget_mut()
            .layout(&mut tree, &renderer, &limits);
        operate(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            focusable::focus_next::<()>(),
        );
        assert_eq!(
            operate(
                &mut element,
                &mut tree,
                &node,
                &renderer,
                focusable::find_focused()
            ),
            Some(id("payment-A"))
        );
        assert_eq!(
            send(
                &mut element,
                &mut tree,
                &node,
                &renderer,
                key(keyboard::key::Named::Enter, true)
            ),
            vec![Intent::Open("A")]
        );
        assert!(send(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            key(keyboard::key::Named::Enter, true)
        )
        .is_empty());
        send(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            key(keyboard::key::Named::Enter, false),
        );
        operate(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            focusable::focus_next::<()>(),
        );
        assert_eq!(
            operate(
                &mut element,
                &mut tree,
                &node,
                &renderer,
                focusable::find_focused()
            ),
            Some(id("payment-B"))
        );
        assert_eq!(
            send(
                &mut element,
                &mut tree,
                &node,
                &renderer,
                key(keyboard::key::Named::Space, true)
            ),
            vec![Intent::Open("B")]
        );
        send(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            key(keyboard::key::Named::Space, false),
        );

        element = make(true);
        tree.diff(element.as_widget());
        node = element
            .as_widget_mut()
            .layout(&mut tree, &renderer, &limits);
        operate(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            focusable::focus::<()>(id("payment-B")),
        );
        assert_eq!(
            send(
                &mut element,
                &mut tree,
                &node,
                &renderer,
                key(keyboard::key::Named::Enter, true)
            ),
            vec![Intent::Open("B")]
        );
        send(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            key(keyboard::key::Named::Enter, false),
        );

        element = iced::widget::column![row(Some("payment-A"), Some(Intent::Open("A")))].into();
        tree.diff(element.as_widget());
        node = element
            .as_widget_mut()
            .layout(&mut tree, &renderer, &limits);
        assert_ne!(
            operate(
                &mut element,
                &mut tree,
                &node,
                &renderer,
                focusable::find_focused()
            ),
            Some(id("payment-B"))
        );
    }

    #[test]
    fn compact_amount_cells_keep_primary_and_secondary_right_aligned() {
        let inspection = capture::inspect(
            "activity-row-alignment",
            row(Some("payment-A"), Some(Intent::Open("A"))),
            (1024, 768),
            "dark",
            coincube_ui::theme::Theme::dark(),
        );
        assert!(inspection.right_aligned("1,000 SATS", "$0.50"));
    }
}
