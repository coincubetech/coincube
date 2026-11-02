//! Import or restore (P-Restore) chrome shared by the Launcher, which owns the
//! Overview and the Cube step, and the opened Cube, which owns the imported
//! wallet's Source, Review, Scan and Done steps. Both draw the same header,
//! path list and stepper, so the user sees one flow across the hand-off.

use iced::{widget::Space, Alignment, Length};

use coincube_ui::{
    component::text::*,
    theme,
    widget::{Column, Container, Element, Row, RowExt},
};

use crate::chain::ChainId;

use super::launcher_coin;

/// The Bitcoin import path. A passkey Cube adds its Passkey step.
pub(crate) fn import_steps(passkey: bool) -> &'static [&'static str] {
    if passkey {
        &[
            "Cube", "Passkey", "Backup", "Source", "Review", "Scan", "Done",
        ]
    } else {
        &["Cube", "Backup", "Source", "Review", "Scan", "Done"]
    }
}

/// The step strip: numbered circles joined by rules, done steps edged good
/// and the current one filled.
pub(crate) fn stepper<'a, M: 'a>(steps: &[&'static str], active: usize) -> Element<'a, M> {
    steps
        .iter()
        .enumerate()
        .fold(
            Row::new().spacing(3).align_y(Alignment::Center),
            |row, (index, label)| {
                let current = index == active;
                let done = index < active;
                let number = Container::new(p2_regular((index + 1).to_string()).size(12))
                    .width(Length::Fixed(22.0))
                    .height(Length::Fixed(22.0))
                    .center_x(Length::Fixed(22.0))
                    .center_y(Length::Fixed(22.0))
                    .style(move |theme: &theme::Theme| iced::widget::container::Style {
                        background: current
                            .then_some(iced::Background::Color(theme.colors.shell.selected)),
                        border: iced::Border {
                            color: if current {
                                theme.colors.shell.accent
                            } else if done {
                                theme.colors.shell.good
                            } else {
                                theme.colors.shell.line
                            },
                            width: 1.0,
                            radius: 11.0.into(),
                        },
                        ..Default::default()
                    });
                let row = row.push(
                    Row::new()
                        .spacing(4)
                        .align_y(Alignment::Center)
                        .push(number)
                        .push(p2_regular(*label).size(11).style(move |theme| {
                            if current || done {
                                theme::text::primary(theme)
                            } else {
                                theme::text::secondary(theme)
                            }
                        })),
                );
                if index + 1 < steps.len() {
                    row.push(rule(18.0, 1.0))
                } else {
                    row
                }
            },
        )
        .into()
}

/// The network's coin mark.
pub(crate) fn coin<'a, M: 'a>(chain: ChainId, light: bool, size: f32) -> Element<'a, M> {
    iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(
        launcher_coin::widget_source(chain, size, 1.0, light).into_bytes(),
    ))
    .width(Length::Fixed(size))
    .height(Length::Fixed(size))
    .into()
}

/// A line-coloured bar: a rule when wide, a divider when tall.
pub(crate) fn rule<'a, M: 'a>(width: f32, height: f32) -> Container<'a, M> {
    Container::new(Space::new())
        .width(Length::Fixed(width))
        .height(Length::Fixed(height))
        .style(|theme: &theme::Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(theme.colors.shell.line)),
            ..Default::default()
        })
}

/// "Import or restore · (coin) network", then whatever trails it.
pub(crate) fn header<'a, M: 'a>(
    chain: ChainId,
    network: &'a str,
    light: bool,
    trailing: Option<Element<'a, M>>,
) -> Element<'a, M> {
    Row::new()
        .align_y(Alignment::Center)
        .spacing(12)
        .push(p1_bold("Import or restore"))
        .push(rule(1.0, 20.0))
        .push(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(coin(chain, light, 16.0))
                .push(p2_regular(network).style(theme::text::secondary)),
        )
        .push(Space::new().width(Length::Fill))
        .push_maybe(trailing)
        .into()
}

/// The path list's width and padding.
pub(crate) fn nav<'a, M: 'a>() -> Column<'a, M> {
    Column::new()
        .spacing(4)
        .padding([16, 12])
        .width(Length::Fixed(220.0))
}

/// A path list row's style: the current path sits on the selected fill.
pub(crate) fn nav_style(
    current: bool,
) -> impl Fn(&theme::Theme, iced::widget::button::Status) -> iced::widget::button::Style {
    move |theme, status| {
        let mut style = theme::button::transparent(
            theme,
            if current {
                iced::widget::button::Status::Active
            } else {
                status
            },
        );
        if current {
            style.background = Some(iced::Background::Color(theme.colors.shell.selected));
            style.text_color = theme.colors.shell.ink;
        }
        style
    }
}

/// The header over the path list beside the path's own region.
pub(crate) fn split<'a, M: 'a>(
    header: Element<'a, M>,
    nav: Element<'a, M>,
    region: Element<'a, M>,
) -> Element<'a, M> {
    Column::new()
        .width(Length::Fill)
        .height(Length::Fill)
        .push(Container::new(header).padding([16, 28]).width(Length::Fill))
        .push(full_rule())
        .push(
            Row::new()
                .height(Length::Fill)
                .push(nav)
                .push(
                    Container::new(Space::new())
                        .width(Length::Fixed(1.0))
                        .height(Length::Fill)
                        .style(|theme: &theme::Theme| iced::widget::container::Style {
                            background: Some(iced::Background::Color(theme.colors.shell.line)),
                            ..Default::default()
                        }),
                )
                .push(region),
        )
        .into()
}

/// A full-width horizontal rule.
pub(crate) fn full_rule<'a, M: 'a>() -> Element<'a, M> {
    Container::new(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|theme: &theme::Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(theme.colors.shell.line)),
            ..Default::default()
        })
        .into()
}

/// A network's full name ("Bitcoin mainnet"), as the header shows it.
pub(crate) fn network_name(chain: ChainId) -> &'static str {
    super::launcher_chrome::network_name(chain)
}

/// Every text a view lays out at 1440×900, in tree order.
#[cfg(test)]
pub(crate) async fn rendered_labels<M>(element: Element<'_, M>) -> Vec<String> {
    use iced::advanced::{
        layout,
        renderer::Headless,
        widget::{Id, Operation, Tree},
        Layout,
    };

    #[derive(Default)]
    struct Labels(Vec<String>);
    impl Operation for Labels {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn text(&mut self, _: Option<&Id>, _: iced::Rectangle, text: &str) {
            self.0.push(text.to_owned());
        }
    }
    let renderer = <iced::Renderer as Headless>::new(
        coincube_ui::font::REGULAR,
        iced::Pixels(14.0),
        Some("tiny-skia"),
    )
    .await
    .expect("software renderer");
    let mut element = element;
    let mut tree = Tree::new(element.as_widget());
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(iced::Size::ZERO, iced::Size::new(1440.0, 900.0)),
    );
    let mut labels = Labels::default();
    element
        .as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
    labels.0
}
