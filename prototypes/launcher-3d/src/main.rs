mod metrics;
mod model;
mod scene;

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use iced::{
    border, event,
    widget::{
        button, checkbox, column, container, pick_list, pin, responsive, row, scrollable, shader,
        stack, text, Column, Space,
    },
    window, Alignment, Background, Border, Color, Element, Fill, Font, Length, Size, Subscription,
    Task, Theme,
};
use metrics::{Metrics, SharedMetrics};
use model::{Catalog, Filter, Network, Protection, MAX_PER_NETWORK};

fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title("Tenshu · 3D Launcher Lab")
        .subscription(App::subscription)
        .theme(App::theme)
        .font(
            include_bytes!("../../../coincube-ui/static/fonts/IBMPlexSans-Regular.ttf").as_slice(),
        )
        .default_font(Font::with_name("IBM Plex Sans"))
        .window(window::Settings {
            size: Size::new(1440., 900.),
            min_size: Some(Size::new(1024., 700.)),
            ..Default::default()
        })
        .centered()
        .run()
}

#[derive(Debug, Clone)]
pub enum Message {
    Select(u32),
    Filter(Filter),
    CreationNetwork(Network),
    Create,
    Reset(u32),
    Float(bool),
    Benchmark(bool),
    Light(bool),
    Tick,
    Focus(bool),
    Open,
    Clear,
    PrintMetrics,
    Step(i32),
    Reduced(bool),
    Resize(Size),
}

struct App {
    catalog: Catalog,
    selected: Option<u32>,
    filter: Filter,
    creation_network: Network,
    float: bool,
    reduced: bool,
    benchmark: bool,
    light: bool,
    focused: bool,
    started: Instant,
    selected_at: Instant,
    metrics: SharedMetrics,
    metrics_text: String,
    notice: String,
}

impl App {
    fn new() -> Self {
        let mut catalog = Catalog::default();
        catalog.reset(4);
        Self {
            catalog,
            selected: None,
            filter: Filter::All,
            creation_network: Network::Bitcoin,
            float: false,
            reduced: false,
            benchmark: false,
            light: false,
            focused: true,
            started: Instant::now(),
            selected_at: Instant::now(),
            metrics: Arc::new(Mutex::new(Metrics::default())),
            metrics_text: "Start benchmark to sample redraw cadence".into(),
            notice: "Synthetic catalog · Nothing is connected to a wallet, account or network."
                .into(),
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Resize(size) => {
                self.reset_metrics();
                return window::latest().and_then(move |id| window::resize(id, size));
            }
            Message::Reduced(value) => {
                self.reduced = value;
                self.reset_metrics();
            }
            Message::Step(direction) => {
                let visible = self.catalog.visible(self.filter);
                if !visible.is_empty() {
                    let next = self
                        .selected
                        .and_then(|id| visible.iter().position(|c| c.id == id))
                        .map(|i| (i as i32 + direction).rem_euclid(visible.len() as i32) as usize)
                        .unwrap_or(if direction >= 0 { 0 } else { visible.len() - 1 });
                    self.selected = Some(visible[next].id);
                    self.selected_at = Instant::now();
                }
            }
            Message::Select(id) => {
                self.selected = Some(id);
                self.selected_at = Instant::now();
                self.notice="Selection only. Open preview demonstrates the next step without unlocking anything.".into();
            }
            Message::Filter(filter) => {
                self.filter = filter;
                self.selected = self
                    .selected
                    .filter(|id| self.catalog.visible(filter).iter().any(|c| c.id == *id));
                self.reset_metrics();
            }
            Message::CreationNetwork(network) => self.creation_network = network,
            Message::Create => {
                if self.catalog.can_create(self.creation_network) {
                    let Some(id) = self.catalog.create(self.creation_network) else {
                        return Task::none();
                    };
                    self.selected = Some(id);
                    self.selected_at = Instant::now();
                    if !self.filter.accepts(self.creation_network) {
                        self.filter = Filter::All;
                    }
                    self.notice="Created an in-memory synthetic Cube. No files, keys or real wallets were created.".into();
                    self.reset_metrics();
                }
            }
            Message::Reset(count) => {
                self.catalog.reset(count);
                self.selected = None;
                self.notice =
                    format!("Loaded {count} synthetic Cubes; previous fixture edits discarded.");
                self.reset_metrics();
            }
            Message::Float(value) => {
                self.float = value;
                self.reset_metrics();
            }
            Message::Benchmark(value) => {
                self.benchmark = value;
                self.reset_metrics();
            }
            Message::Light(value) => self.light = value,
            Message::Tick => {
                if let Ok(metrics) = self.metrics.lock() {
                    self.metrics_text = metrics.summary();
                }
            }
            Message::Focus(value) => {
                self.focused = value;
                self.reset_metrics();
            }
            Message::Open => {
                if let Some(cube) = self
                    .selected
                    .and_then(|id| self.catalog.cubes.iter().find(|c| c.id == id))
                {
                    self.notice=format!("Preview: {} · {}. The real app would now request PIN/passkey and validate this Cube’s network. No unlock was performed.",cube.name,cube.network);
                }
            }
            Message::Clear => {
                if self.filter != Filter::All {
                    self.filter = Filter::All;
                    self.reset_metrics();
                }
            }
            Message::PrintMetrics => {
                if let Ok(metrics) = self.metrics.lock() {
                    println!("Launcher lab | visible={} | filter={} | float={} | benchmark={} | {} | CPU timings only; GPU execution not measured",self.catalog.visible(self.filter).len(),self.filter,self.float,self.benchmark,metrics.summary());
                    self.notice = "Printed synthetic performance summary to the terminal.".into();
                }
            }
        }
        Task::none()
    }

    fn reset_metrics(&mut self) {
        if let Ok(mut metrics) = self.metrics.lock() {
            *metrics = Metrics::default();
        }
        self.metrics_text = if self.sampling_active() {
            "Collecting fresh samples…"
        } else {
            "Start benchmark to sample redraw cadence"
        }
        .into();
    }

    fn sampling_active(&self) -> bool {
        self.focused && (self.benchmark || (self.float && !self.reduced))
    }

    fn subscription(&self) -> Subscription<Message> {
        let focus = event::listen_with(|event, _, _| match event {
            iced::Event::Window(window::Event::Focused) => Some(Message::Focus(true)),
            iced::Event::Window(window::Event::Unfocused) => Some(Message::Focus(false)),
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(key),
                ..
            }) => match key {
                iced::keyboard::key::Named::ArrowRight | iced::keyboard::key::Named::ArrowDown => {
                    Some(Message::Step(1))
                }
                iced::keyboard::key::Named::ArrowLeft | iced::keyboard::key::Named::ArrowUp => {
                    Some(Message::Step(-1))
                }
                iced::keyboard::key::Named::Enter => Some(Message::Open),
                iced::keyboard::key::Named::F4 => Some(Message::Reset(12)),
                iced::keyboard::key::Named::F5 => Some(Message::Reset(20)),
                iced::keyboard::key::Named::F6 => Some(Message::Resize(Size::new(1024., 700.))),
                iced::keyboard::key::Named::F7 => Some(Message::Resize(Size::new(1440., 900.))),
                _ => None,
            },
            _ => None,
        });
        if self.sampling_active() {
            Subscription::batch([
                focus,
                iced::time::every(Duration::from_millis(500)).map(|_| Message::Tick),
            ])
        } else {
            focus
        }
    }

    fn theme(&self) -> Theme {
        if self.light {
            Theme::Light
        } else {
            Theme::Dark
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let muted = if self.light {
            Color::from_rgb8(82, 84, 78)
        } else {
            Color::from_rgb8(179, 187, 183)
        };
        let visible = Arc::new(self.catalog.visible(self.filter));
        let hidden = self.catalog.cubes.len() - visible.len();
        let header = row![
            column![
                text("Tenshu").size(28),
                text("Cube launcher").size(16).color(muted)
            ]
            .spacing(2),
            Space::new().width(Fill),
            text("3D Launcher Lab · Synthetic data only")
                .size(14)
                .color(muted),
            checkbox(self.light)
                .label("Light theme")
                .on_toggle(Message::Light)
        ]
        .spacing(24)
        .align_y(Alignment::Center);
        let toolbar = row![
            pick_list(Filter::ALL, Some(self.filter), Message::Filter)
                .placeholder("Network filter"),
            text(format!("{} shown · {hidden} hidden", visible.len()))
                .size(14)
                .color(muted),
            button("Clear filters")
                .on_press(Message::Clear)
                .style(button::text),
            Space::new().width(Fill),
            text(format!(
                "{}/{} on",
                self.catalog.network_count(self.creation_network),
                MAX_PER_NETWORK
            ))
            .size(14)
            .color(muted),
            pick_list(
                Network::ALL,
                Some(self.creation_network),
                Message::CreationNetwork
            ),
            button("+ Create Cube")
                .on_press_maybe(
                    self.catalog
                        .can_create(self.creation_network)
                        .then_some(Message::Create)
                )
                .style(primary)
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        let load = row![
            text("Fixture load").size(14).color(muted),
            button("Empty")
                .on_press(Message::Reset(0))
                .style(button::secondary),
            button("1")
                .on_press(Message::Reset(1))
                .style(button::secondary),
            button("4")
                .on_press(Message::Reset(4))
                .style(button::secondary),
            button("12")
                .on_press(Message::Reset(12))
                .style(button::secondary),
            button("20")
                .on_press(Message::Reset(20))
                .style(button::secondary),
            Space::new().width(Fill),
            checkbox(self.float)
                .label("Float")
                .on_toggle(Message::Float),
            checkbox(self.benchmark)
                .label("Benchmark")
                .on_toggle(Message::Benchmark),
            checkbox(self.reduced)
                .label("Reduce motion")
                .on_toggle(Message::Reduced)
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        let scene = scene::Scene {
            cubes: visible.clone(),
            selected: self.selected,
            benchmark: self.benchmark && self.focused,
            float: self.float && self.focused && !self.reduced,
            reduced: self.reduced,
            light: self.light,
            started: self.started,
            selected_at: if self.focused {
                self.selected_at
            } else {
                self.started - Duration::from_secs(1)
            },
            metrics: self.metrics.clone(),
        };
        let selected = self.selected;
        let light = self.light;
        let has_catalog = !self.catalog.cubes.is_empty();
        let stage: Element<'_, Message> = responsive(move |size| {
            let mut layers: Vec<Element<'_, Message>> =
                vec![shader(scene.clone()).width(Fill).height(Fill).into()];
            if visible.is_empty() {
                let title = if has_catalog {
                    "No Cubes match this filter"
                } else {
                    "A place for your Cubes"
                };
                let subtitle = if has_catalog {
                    "Clear filters to see the rest of the synthetic catalog."
                } else {
                    "Create your first synthetic Cube. It will appear on the floor."
                };
                let action = if has_catalog {
                    button("Show all Cubes").on_press(Message::Clear)
                } else {
                    button("+ Create Cube").on_press(Message::Create)
                };
                layers.push(
                    container(
                        column![
                            text(title).size(26),
                            text(subtitle).size(16).color(muted),
                            action.style(primary)
                        ]
                        .spacing(16)
                        .align_x(Alignment::Center),
                    )
                    .center(Fill)
                    .into(),
                );
            } else if visible.len() <= 8 {
                for (cube, point) in visible
                    .iter()
                    .zip(scene::label_positions(visible.len(), size))
                {
                    // Keep a single selected label when projected slots are too tight.
                    // The regular Iced catalog always supplies every name/network.
                    if size.width / (visible.len() as f32) < 220.0 && selected != Some(cube.id) {
                        continue;
                    }
                    let label = column![
                        text(cube.name.clone()).size(15),
                        text(cube.network.to_string()).size(12).color(muted)
                    ]
                    .spacing(2)
                    .align_x(Alignment::Center);
                    let id = cube.id;
                    let selected = selected == Some(id);
                    let label = button(label)
                        .on_press(Message::Select(id))
                        .padding([7, 10])
                        .style(move |_, status| label_style(light, selected, status));
                    layers.push(
                        pin(label)
                            .x((point.x - 100.).clamp(0., (size.width - 200.).max(0.)))
                            .y((point.y + 6.).clamp(0., (size.height - 70.).max(0.)))
                            .into(),
                    );
                }
            }
            stack(layers).width(Fill).height(Fill).into()
        })
        .width(Fill)
        .height(Fill)
        .into();
        let catalog = self.catalog_view(muted);
        let workspace = row![
            container(stage)
                .width(Fill)
                .height(Fill)
                .style(move |_| container::Style {
                    background: Some(Background::Color(if light {
                        Color::from_rgb8(227, 223, 213)
                    } else {
                        Color::from_rgb8(16, 20, 22)
                    })),
                    border: Border {
                        radius: 10.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            catalog
        ]
        .spacing(20)
        .height(Fill);
        let details = if let Some(cube) = self
            .selected
            .and_then(|id| self.catalog.cubes.iter().find(|c| c.id == id))
        {
            let protection_color = if cube.protection == Protection::NeedsBackup {
                if self.light {
                    Color::from_rgb8(125, 64, 10)
                } else {
                    Color::from_rgb8(255, 192, 123)
                }
            } else {
                muted
            };
            row![
                column![
                    text(format!("{} · {}", cube.name, cube.network)).size(17),
                    row![
                        text(cube.protection.to_string())
                            .size(14)
                            .color(protection_color),
                        text(if cube.has_vault {
                            "Vault configured · fixture"
                        } else {
                            "No Vault configured"
                        })
                        .size(14)
                        .color(muted)
                    ]
                    .spacing(22)
                ]
                .spacing(6),
                Space::new().width(Fill),
                text("Locked · fixture").size(14).color(muted),
                button("Open preview")
                    .on_press(Message::Open)
                    .style(primary)
            ]
            .spacing(18)
            .align_y(Alignment::Center)
        } else {
            row![
                text("Select a Cube · arrow keys move selection · Enter opens preview.")
                    .size(16)
                    .color(muted),
                Space::new().width(Fill),
                text("No real balances or keys").size(14).color(muted)
            ]
            .align_y(Alignment::Center)
        };
        let performance = row![
            text(self.metrics_text.clone()).size(13).color(muted),
            Space::new().width(Fill),
            button("Print metrics")
                .on_press(Message::PrintMetrics)
                .style(button::text)
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        container(column![header,toolbar,load,workspace,details,text(&self.notice).size(13).color(muted),performance,text("Metrics: redraw cadence and CPU scene preparation/encoding only; GPU execution, presentation latency and power are not measured. Animation/benchmark pause when unfocused.").size(12).color(muted)].spacing(18)).padding(24).width(Fill).height(Fill).into()
    }

    fn catalog_view(&self, muted: Color) -> Element<'_, Message> {
        let mut list = Column::new().spacing(7);
        for cube in self.catalog.visible(self.filter) {
            let selected = self.selected == Some(cube.id);
            let light = self.light;
            let badge = match cube.protection {
                Protection::NeedsBackup => "Backup not confirmed",
                Protection::Unknown => "Backup status unknown",
                Protection::Confirmed => {
                    if cube.has_vault {
                        "Vault configured"
                    } else {
                        "No Vault configured"
                    }
                }
            };
            list = list.push(
                button(
                    column![
                        text(cube.name).size(16),
                        text(cube.network.to_string()).size(13).color(muted),
                        text(badge).size(12).color(muted)
                    ]
                    .spacing(4),
                )
                .width(Fill)
                .padding(12)
                .on_press(Message::Select(cube.id))
                .style(move |_, status| label_style(light, selected, status)),
            );
        }
        container(
            column![
                text("Your Cubes").size(19),
                text("All entries are synthetic. Network color is supported by a text label.")
                    .size(13)
                    .color(muted),
                scrollable(list).height(Fill)
            ]
            .spacing(14),
        )
        .width(Length::Fixed(260.))
        .height(Fill)
        .into()
    }
}

fn primary(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::primary(theme, status);
    style.background = Some(Background::Color(
        if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            Color::from_rgb8(243, 168, 98)
        } else {
            Color::from_rgb8(243, 130, 29)
        },
    ));
    style.text_color = Color::from_rgb8(25, 25, 23);
    style.border = Border {
        radius: 6.into(),
        ..Default::default()
    };
    if matches!(status, button::Status::Disabled) {
        style.background = Some(Background::Color(Color::from_rgb8(100, 100, 95)));
    }
    style
}

fn label_style(light: bool, selected: bool, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(Background::Color(if selected {
            if light {
                Color::from_rgb8(247, 225, 200)
            } else {
                Color::from_rgb8(58, 43, 29)
            }
        } else if hovered {
            if light {
                Color::from_rgb8(220, 215, 205)
            } else {
                Color::from_rgb8(42, 47, 47)
            }
        } else if light {
            Color::from_rgba8(248, 246, 240, 0.9)
        } else {
            Color::from_rgba8(23, 28, 29, 0.9)
        })),
        text_color: if light {
            Color::from_rgb8(28, 31, 28)
        } else {
            Color::from_rgb8(245, 240, 232)
        },
        border: Border {
            color: if selected {
                Color::from_rgb8(243, 130, 29)
            } else {
                Color::TRANSPARENT
            },
            width: if selected { 1.0 } else { 0.0 },
            radius: border::Radius::from(7.),
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_or_pausing_sampling_clears_the_collecting_status() {
        let mut app = App::new();
        let collecting = "Collecting fresh samples…";
        let idle = "Start benchmark to sample redraw cadence";
        let _ = app.update(Message::Benchmark(true));
        assert_eq!(app.metrics_text, collecting);
        let _ = app.update(Message::Benchmark(false));
        assert_eq!(app.metrics_text, idle);
        let _ = app.update(Message::Float(true));
        assert_eq!(app.metrics_text, collecting);
        let _ = app.update(Message::Reduced(true));
        assert_eq!(app.metrics_text, idle);
        let _ = app.update(Message::Benchmark(true));
        assert_eq!(app.metrics_text, collecting);
        let _ = app.update(Message::Focus(false));
        assert_eq!(app.metrics_text, idle);
        let _ = app.update(Message::Focus(true));
        assert_eq!(app.metrics_text, collecting);
    }

    #[test]
    fn clearing_a_filter_discards_timings_from_the_previous_scene_load() {
        let mut app = App::new();
        let _ = app.update(Message::Filter(Filter::Bitcoin));
        let before = app.catalog.visible(app.filter).len();
        let at = Instant::now();
        {
            let mut metrics = app.metrics.lock().unwrap();
            metrics.redraw(at);
            metrics.redraw(at + Duration::from_millis(20));
            metrics.prepared(0.1);
            metrics.encoded(0.2);
        }
        assert!(app.metrics.lock().unwrap().summary().starts_with("50.0"));

        let _ = app.update(Message::Clear);

        assert!(app.catalog.visible(app.filter).len() > before);
        assert_eq!(app.metrics.lock().unwrap().renders, 0);
        assert_eq!(
            app.metrics.lock().unwrap().summary(),
            "Start benchmark to sample redraw cadence"
        );
    }
}
