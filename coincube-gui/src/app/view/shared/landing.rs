//! Synthetic mounted-presenter inspection for privacy and Launcher regressions.
//! This test-only probe does not depend on the Tenshu landing components.

pub(crate) mod capture {
    use coincube_ui::font;
    use coincube_ui::{theme, widget::Element};
    use iced::advanced::widget::Id;
    use iced::advanced::{
        layout,
        renderer::Headless,
        widget::{operation, operation::focusable, operation::Focusable, Operation, Tree},
        Layout,
    };
    use iced::Length;
    use iced::{keyboard, mouse, Event, Rectangle, Size};

    #[derive(Default)]
    struct TextProbe {
        entries: Vec<(String, Rectangle)>,
        focus: Vec<(Id, Rectangle)>,
    }

    impl Operation for TextProbe {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn text(&mut self, _id: Option<&Id>, bounds: Rectangle, value: &str) {
            self.entries.push((value.to_owned(), bounds));
        }

        fn focusable(&mut self, id: Option<&Id>, bounds: Rectangle, _: &mut dyn Focusable) {
            if let Some(id) = id {
                self.focus.push((id.clone(), bounds));
            }
        }
    }

    pub struct Inspection {
        pub text: Vec<(String, Rectangle)>,
        pub focus: Vec<(Id, Rectangle)>,
    }

    impl Inspection {
        pub fn contains(&self, needle: &str) -> bool {
            self.text.iter().any(|(value, _)| value.contains(needle))
        }

        pub fn has_focus(&self, id: Id) -> bool {
            self.focus.iter().any(|(actual, _)| actual == &id)
        }
    }

    /// Draws the actual production presenter at the dashboard's measured
    /// 8/10 content width. No App/Gui, live wallet, session or OS window exists.
    /// Screenshots are optional and always labeled content-only synthetic.
    pub fn inspect<M: 'static>(
        scenario: &str,
        content: Element<'_, M>,
        outer: (u32, u32),
        appearance: &str,
        theme: theme::Theme,
    ) -> Inspection {
        let sidebar = if outer.0 <= 1100 { 208.0 } else { 248.0 };
        let width = (((outer.0 as f32 - sidebar) * 0.8).floor() as u32).max(1);
        inspect_at_width(scenario, content, outer, appearance, theme, width)
    }

    /// The Launcher fills the window rather than the dashboard's 8/10 pane.
    /// Its content-only fixture uses the actual 1024px minimum width.
    pub fn inspect_full_window<M: 'static>(
        scenario: &str,
        content: Element<'_, M>,
        outer: (u32, u32),
        appearance: &str,
        theme: theme::Theme,
    ) -> Inspection {
        inspect_at_width(scenario, content, outer, appearance, theme, outer.0)
    }

    fn inspect_at_width<M: 'static>(
        scenario: &str,
        content: Element<'_, M>,
        outer: (u32, u32),
        appearance: &str,
        theme: theme::Theme,
        width: u32,
    ) -> Inspection {
        for bytes in font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        let height = outer.1.saturating_sub(100).max(1);
        let size = Size::new(width as f32, height as f32);
        let mut renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            font::REGULAR,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("software renderer for a synthetic R06a presenter");
        let mut element: Element<'_, M> = iced::widget::scrollable(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        assert!(
            node.size().width <= size.width + 1.0,
            "{}: root width overflow",
            scenario
        );
        let mut probe = TextProbe::default();
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut probe);
        for (value, bounds) in &probe.entries {
            assert!(
                bounds.x >= -1.0 && bounds.x + bounds.width <= width as f32 + 1.0,
                "{} {} {:?}: text {:?} at {:?} exceeds {}px content width",
                scenario,
                appearance,
                outer,
                value,
                bounds,
                width
            );
        }
        if let Ok(directory) = std::env::var("R06A_CAPTURE_DIR") {
            std::fs::create_dir_all(&directory).expect("create disposable capture directory");
            let viewport = Rectangle::with_size(size);
            element.as_widget_mut().update(
                &mut tree,
                &iced::Event::Window(iced::window::Event::RedrawRequested(
                    std::time::Instant::now(),
                )),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut iced::advanced::Shell::new(&mut Vec::new()),
                &viewport,
            );
            element.as_widget().draw(
                &tree,
                &mut renderer,
                &theme,
                &iced::advanced::renderer::Style {
                    text_color: theme.colors.text.primary,
                },
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &viewport,
            );
            let pixels = renderer.screenshot(
                Size::new(width, height),
                1.0,
                theme.colors.general.background,
            );
            let filename = format!(
                "{scenario}-{appearance}-{}x{}-content-only-synthetic.png",
                outer.0, outer.1
            );
            image::save_buffer(
                std::path::Path::new(&directory).join(filename),
                &pixels,
                width,
                height,
                image::ColorType::Rgba8,
            )
            .expect("save synthetic R06a presenter capture");
        }
        Inspection {
            text: probe.entries,
            focus: probe.focus,
        }
    }

    /// Exercise a real presenter tree with the shell's focus operation and
    /// either Enter or a complete primary-pointer click at the Action bounds.
    pub fn activate<M: Clone + 'static>(
        content: Element<'_, M>,
        target: Id,
        outer: (u32, u32),
        pointer: bool,
    ) -> Vec<M> {
        for bytes in font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        let sidebar = if outer.0 <= 1100 { 208.0 } else { 248.0 };
        let size = Size::new(
            ((outer.0 as f32 - sidebar) * 0.8).floor(),
            outer.1.saturating_sub(100) as f32,
        );
        let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            font::REGULAR,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut element: Element<'_, M> = iced::widget::scrollable(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let mut operation: Box<dyn Operation<()>> =
            Box::new(focusable::focus::<()>(target.clone()));
        loop {
            element.as_widget_mut().operate(
                &mut tree,
                Layout::new(&node),
                &renderer,
                &mut operation::black_box(operation.as_mut()),
            );
            match operation.finish() {
                operation::Outcome::None | operation::Outcome::Some(()) => break,
                operation::Outcome::Chain(next) => operation = next,
            }
        }
        let mut probe = TextProbe::default();
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut probe);
        let bounds = probe
            .focus
            .iter()
            .find_map(|(id, bounds)| (id == &target).then_some(*bounds))
            .expect("production Action focus target");
        let cursor = if pointer {
            mouse::Cursor::Available(bounds.center())
        } else {
            mouse::Cursor::Unavailable
        };
        let events = if pointer {
            vec![
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ]
        } else {
            let key = keyboard::Key::Named(keyboard::key::Named::Enter);
            let physical_key =
                keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified);
            vec![
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: key.clone(),
                    modified_key: key.clone(),
                    physical_key,
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::empty(),
                    text: None,
                    repeat: false,
                }),
                Event::Keyboard(keyboard::Event::KeyReleased {
                    key: key.clone(),
                    modified_key: key,
                    physical_key,
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::empty(),
                }),
            ]
        };
        let mut messages = Vec::new();
        for event in events {
            element.as_widget_mut().update(
                &mut tree,
                &event,
                Layout::new(&node),
                cursor,
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut iced::advanced::Shell::new(&mut messages),
                &Rectangle::with_size(size),
            );
        }
        messages
    }
}
