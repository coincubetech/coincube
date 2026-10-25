//! Synthetic mounted-presenter inspection for privacy regressions.
//! This test-only probe does not depend on the Tenshu landing components.

pub(crate) mod capture {
    use coincube_ui::{font, theme, widget::Element};
    use iced::advanced::{
        layout,
        renderer::Headless,
        widget::{Operation, Tree},
        Layout,
    };
    use iced::{widget::scrollable, Length, Rectangle, Size};

    #[derive(Default)]
    struct TextProbe(Vec<String>);

    impl Operation for TextProbe {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn text(
            &mut self,
            _id: Option<&iced::advanced::widget::Id>,
            _bounds: Rectangle,
            value: &str,
        ) {
            self.0.push(value.to_owned());
        }
    }

    pub struct Inspection {
        text: Vec<String>,
    }

    impl Inspection {
        pub fn contains(&self, needle: &str) -> bool {
            self.text.iter().any(|value| value.contains(needle))
        }
    }

    /// Mounts the real widget tree in iced's software renderer and reads its
    /// text operation. No wallet, window, network service or secret is used.
    pub fn inspect<M: 'static>(
        _scenario: &str,
        content: Element<'_, M>,
        outer: (u32, u32),
        _appearance: &str,
        _theme: theme::Theme,
    ) -> Inspection {
        for bytes in font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        let size = Size::new(outer.0 as f32, outer.1 as f32);
        let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            font::REGULAR,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("software renderer for synthetic privacy presenter");
        let mut element: Element<'_, M> = scrollable(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let mut probe = TextProbe::default();
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut probe);
        Inspection { text: probe.0 }
    }
}
