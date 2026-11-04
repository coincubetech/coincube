//! Import or restore (P-Restore) chrome shared by the Launcher, which owns the
//! Overview and the Cube step, and the opened Cube, which owns the imported
//! wallet's Source, Review, Scan and Done steps. Both draw the same header,
//! path list and stepper, so the user sees one flow across the hand-off.

use iced::{widget::Space, Alignment, Length};

use iced::advanced::widget::Id;

use coincube_ui::{
    component::text::*,
    theme,
    widget::{
        action::Action as FocusableAction, Column, ColumnExt, Container, Element, Row, RowExt,
    },
};

use crate::chain::{ChainId, ChainIdExt};

use super::launcher_coin;

/// The Bitcoin import path. A passkey Cube adds its Passkey step.
pub(crate) fn import_steps(passkey: bool) -> &'static [&'static str] {
    if passkey {
        &[
            "Cube", "Passkey", "Backup", "Source", "Keys", "Review", "Scan", "Done",
        ]
    } else {
        &["Cube", "Backup", "Source", "Keys", "Review", "Scan", "Done"]
    }
}

/// The step strip (P-Restore `.steps`): 24px numbered circles with the
/// label 13px to their right, joined by 20px rules. The current step is
/// ringed in accent on the selected fill; a completed one is ringed good and
/// shows a check in place of its number; a step ahead is ringed in line ink
/// with a muted label.
pub(crate) fn stepper<'a, M: 'a>(steps: &[&'static str], active: usize) -> Element<'a, M> {
    steps
        .iter()
        .enumerate()
        .fold(
            Row::new().spacing(10).align_y(Alignment::Center),
            |row, (index, label)| {
                let current = index == active;
                let done = index < active;
                let mark: Element<'a, M> = if done {
                    coincube_ui::icon::check_icon()
                        .size(12)
                        .style(|theme: &theme::Theme| iced::widget::text::Style {
                            color: Some(theme.colors.shell.good),
                        })
                        .into()
                } else {
                    p2_regular((index + 1).to_string())
                        .size(12)
                        .style(move |theme| {
                            if current {
                                theme::text::primary(theme)
                            } else {
                                theme::text::secondary(theme)
                            }
                        })
                        .into()
                };
                let number = Container::new(mark)
                    .width(Length::Fixed(24.0))
                    .height(Length::Fixed(24.0))
                    .center_x(Length::Fixed(24.0))
                    .center_y(Length::Fixed(24.0))
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
                            radius: 12.0.into(),
                        },
                        ..Default::default()
                    });
                let row = row.push(
                    Row::new()
                        .spacing(13)
                        .align_y(Alignment::Center)
                        .push(number)
                        .push(p2_regular(*label).size(13).style(move |theme| {
                            if current || done {
                                theme::text::primary(theme)
                            } else {
                                theme::text::secondary(theme)
                            }
                        })),
                );
                if index + 1 < steps.len() {
                    row.push(rule(20.0, 1.0))
                } else {
                    row
                }
            },
        )
        .into()
}

/// The step rail: the strip in a 52px band over a full-width rule.
pub(crate) fn step_rail<'a, M: 'a>(steps: &[&'static str], active: usize) -> Element<'a, M> {
    Column::new()
        .push(
            Container::new(stepper(steps, active))
                .height(Length::Fixed(RAIL_HEIGHT))
                .center_y(Length::Fixed(RAIL_HEIGHT))
                .padding([0, 28])
                .width(Length::Fill),
        )
        .push(full_rule())
        .into()
}

/// The sheet header's height (`.shead`).
pub(crate) const HEADER_HEIGHT: f32 = 72.0;
/// The step rail's band height.
pub(crate) const RAIL_HEIGHT: f32 = 52.0;

/// The sheet surface (`.sheet`): panel fill, 1px line, 12px radius, the
/// same as the Connect and App preferences sheets.
pub(crate) fn sheet_style(theme: &theme::Theme) -> iced::widget::container::Style {
    let shell = &theme.colors.shell;
    iced::widget::container::Style {
        background: Some(iced::Background::Color(shell.panel)),
        border: iced::Border {
            color: shell.line,
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    }
}

/// A network's tag beside its name: test networks carry their own name,
/// as the Launcher's chips do.
fn network_tag<'a, M: 'a>(chain: ChainId) -> Option<Element<'a, M>> {
    launcher_coin::is_test_network(chain).then(|| {
        Container::new(p2_regular(chain.label()).size(12))
            .padding([2, 6])
            .style(|theme: &theme::Theme| iced::widget::container::Style {
                border: iced::Border {
                    color: theme.colors.shell.muted,
                    width: 1.0,
                    radius: 4.0.into(),
                },
                ..Default::default()
            })
            .into()
    })
}

/// The header's network control: coin mark, network name, test tag and,
/// when it opens the network menu, a chevron. `toggle` is None once the
/// network is fixed, and the control is then a static chip.
pub(crate) fn network_control<'a, M: Clone + 'static>(
    chain: ChainId,
    light: bool,
    toggle: Option<M>,
    open: bool,
    focus: impl FnOnce(Id) -> M,
) -> Element<'a, M> {
    let editable = toggle.is_some();
    let mut chip = Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(coin(chain, light, 16.0))
        .push(p2_regular(chain.label()))
        .push_maybe(network_tag(chain));
    if !editable {
        return Container::new(chip).padding([6, 0]).into();
    }
    chip = chip.push(
        iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(CHEVRON.as_bytes()))
            .width(Length::Fixed(14.0))
            .height(Length::Fixed(14.0))
            .style(|theme: &theme::Theme, _| iced::widget::svg::Style {
                color: Some(theme.colors.shell.muted),
            }),
    );
    FocusableAction::new(Id::new(NETWORK_CONTROL_ID), chip, focus)
        .padding([6, 10])
        .style(move |theme: &theme::Theme, status| {
            let mut style = theme::button::transparent(theme, status);
            if open {
                style.background = Some(iced::Background::Color(theme.colors.shell.selected));
            }
            style.text_color = theme.colors.shell.ink;
            style.border.radius = 8.0.into();
            style
        })
        .on_press_maybe(toggle)
        .into()
}

/// The header's network control.
pub(crate) const NETWORK_CONTROL_ID: &str = "tenshu.wizard:network";
const CHEVRON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M7 10l5 5 5-5"/></svg>"##;

/// The sheet's Close, styled like the Connect and App preferences sheets'.
pub(crate) fn close_button<'a, M: Clone + 'static>(
    id: &'static str,
    on_press: Option<M>,
    focus: impl FnOnce(Id) -> M,
) -> Element<'a, M> {
    FocusableAction::new(
        Id::new(id),
        Container::new(
            Row::new()
                .push(
                    coincube_ui::icon::cross_icon()
                        .size(14)
                        .style(theme::tenshu::text::ink),
                )
                .push(coincube_ui::component::tenshu::text::label("Close"))
                .spacing(6)
                .align_y(Alignment::Center),
        )
        .center_y(Length::Fill),
        focus,
    )
    .height(Length::Fixed(36.0))
    .padding([0, 12])
    .style(|theme, status| {
        theme::tenshu::button::action(
            theme,
            status,
            theme::tenshu::button::ActionKind::Secondary,
            theme::tenshu::button::ActionShape::Compact,
        )
    })
    .on_press_maybe(on_press)
    .into()
}

/// The sheet header: title, a divider and the network control, then Close.
pub(crate) fn sheet_header<'a, M: 'a>(
    title: &'static str,
    network: Element<'a, M>,
    close: Element<'a, M>,
) -> Element<'a, M> {
    Container::new(
        Row::new()
            .align_y(Alignment::Center)
            .spacing(16)
            .push(title_text(title))
            .push(rule(1.0, 20.0))
            .push(network)
            .push(Space::new().width(Length::Fill))
            .push(close),
    )
    .height(Length::Fixed(HEADER_HEIGHT))
    .center_y(Length::Fixed(HEADER_HEIGHT))
    .padding(iced::Padding {
        left: 28.0,
        right: 24.0,
        ..iced::Padding::ZERO
    })
    .width(Length::Fill)
    .into()
}

fn title_text<'a>(title: &'static str) -> iced::widget::Text<'a, theme::Theme> {
    iced::widget::text(title)
        .size(20)
        .font(coincube_ui::font::SEMIBOLD)
}

/// The network menu, dropped under the header's network control: an
/// undrawn copy of the title and divider puts it under the chip.
pub(crate) fn network_menu<'a, M: 'a>(title: &'static str, rows: Element<'a, M>) -> Element<'a, M> {
    let menu = Container::new(rows)
        .width(Length::Fixed(340.0))
        .padding(6)
        .style(|theme: &theme::Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(theme.colors.shell.panel)),
            border: iced::Border {
                color: theme.colors.shell.line,
                width: 1.0,
                radius: 10.0.into(),
            },
            shadow: iced::Shadow {
                color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.35),
                offset: iced::Vector::new(0.0, 18.0),
                blur_radius: 40.0,
            },
            ..Default::default()
        });
    Column::new()
        .push(Space::new().height(Length::Fixed(HEADER_HEIGHT - 12.0)))
        .push(
            Row::new()
                .spacing(16)
                .padding(iced::Padding {
                    left: 28.0,
                    ..iced::Padding::ZERO
                })
                .push(
                    title_text(title).style(|_: &theme::Theme| iced::widget::text::Style {
                        color: Some(iced::Color::TRANSPARENT),
                    }),
                )
                .push(Space::new().width(Length::Fixed(1.0)))
                .push(menu),
        )
        .into()
}

/// A network menu heading ("Live networks", "Test networks").
pub(crate) fn menu_heading<'a, M: 'a>(label: &'a str) -> Element<'a, M> {
    Container::new(p2_regular(label).size(12).style(theme::text::secondary))
        .padding([8, 10])
        .into()
}

/// A wizard step's region: the step rail, the scrolled body and the footer
/// actions, each above a rule.
pub(crate) fn region<'a, M: 'a>(
    rail: Option<Element<'a, M>>,
    body: Element<'a, M>,
    footer: Option<Element<'a, M>>,
) -> Element<'a, M> {
    let mut column = Column::new()
        .width(Length::Fill)
        .height(Length::Fill)
        .push_maybe(rail)
        .push(body);
    if let Some(footer) = footer {
        column = column
            .push(full_rule())
            .push(Container::new(footer).padding([14, 28]).width(Length::Fill));
    }
    column.into()
}

/// The path list beside a path's region, split by a line-coloured rule.
pub(crate) fn with_nav<'a, M: 'a>(nav: Element<'a, M>, region: Element<'a, M>) -> Element<'a, M> {
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
        .push(region)
        .into()
}

/// The wizard sheet: its header over the content, with the network menu
/// (when open) layered on top. A press outside the open menu closes it.
pub(crate) fn sheet<'a, M: Clone + 'a>(
    header: Element<'a, M>,
    content: Element<'a, M>,
    menu: Option<(Element<'a, M>, M)>,
) -> Element<'a, M> {
    let surface = Container::new(
        Column::new()
            .width(Length::Fill)
            .height(Length::Fill)
            .push(header)
            .push(full_rule())
            .push(content),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(sheet_style);
    let stack = iced::widget::Stack::new()
        .width(Length::Fill)
        .height(Length::Fill)
        .push(surface);
    match menu {
        Some((menu, close)) => stack
            .push(
                iced::widget::mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
                    .on_press(close),
            )
            .push(menu)
            .into(),
        None => stack.into(),
    }
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
