//! Keyboard support around the incumbent Iced controls, without changing their layout.
//!
//! Iced 0.14 buttons, checkboxes and pick lists do not provide a complete Tab path.
//! Focus is owned by the app; this adapter captures keys before global shortcuts and
//! reuses Iced's pick-list rendering and menu for both pointer and keyboard input.
use iced::advanced::Renderer as _;
use iced::{
    advanced::{layout, mouse, overlay, renderer, widget::Tree, Clipboard, Layout, Shell, Widget},
    keyboard::{key::Named, Key, Modifiers},
    widget::{overlay::menu, pick_list},
    Background, Border, Color, Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector,
};

use crate::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Theme,
    Filter,
    Clear,
    CreationNetwork,
    Create,
    Fixture(u32),
    Float,
    Benchmark,
    Reduced,
    EmptyAction,
    Catalog,
    Open,
    PrintMetrics,
}

pub enum Kind {
    Action(Message),
    Catalog,
    Choice {
        labels: Vec<String>,
        actions: Vec<Message>,
        selected: usize,
    },
}

#[derive(Default)]
struct State {
    open: bool,
    hovered: Option<usize>,
    menu: menu::State,
}

impl State {
    /// `Some` means the key belongs to this control, even if it emits no message.
    fn key(
        &mut self,
        key: &Key,
        modifiers: Modifiers,
        repeat: bool,
        kind: &Kind,
    ) -> Option<Option<Message>> {
        if modifiers.command() || modifiers.control() || modifiers.alt() {
            return None;
        }
        let activate = matches!(key, Key::Named(Named::Enter | Named::Space))
            || matches!(key, Key::Character(c) if c == " ");
        match kind {
            Kind::Action(action) if activate => Some((!repeat).then(|| action.clone())),
            Kind::Catalog => match key {
                Key::Named(Named::ArrowRight | Named::ArrowDown) => Some(Some(Message::Step(1))),
                Key::Named(Named::ArrowLeft | Named::ArrowUp) => Some(Some(Message::Step(-1))),
                _ if activate => Some((!repeat).then_some(Message::Open)),
                _ => None,
            },
            Kind::Choice {
                actions, selected, ..
            } if !actions.is_empty() => {
                match key {
                    Key::Named(Named::Tab) => {
                        self.open = false;
                        None // Let the app advance focus, including Shift-Tab.
                    }
                    Key::Named(Named::Escape) if self.open => {
                        self.open = false;
                        Some(None)
                    }
                    _ if activate => {
                        if repeat {
                            return Some(None);
                        }
                        if self.open {
                            self.open = false;
                            Some(self.hovered.and_then(|i| actions.get(i)).cloned())
                        } else {
                            self.open = true;
                            self.hovered = Some(*selected);
                            Some(None)
                        }
                    }
                    Key::Named(Named::ArrowDown | Named::ArrowUp | Named::Home | Named::End) => {
                        self.hovered = Some(match key {
                            Key::Named(Named::Home) => 0,
                            Key::Named(Named::End) => actions.len() - 1,
                            _ if !self.open => *selected,
                            Key::Named(Named::ArrowUp) => {
                                self.hovered.unwrap_or(*selected).saturating_sub(1)
                            }
                            _ => (self.hovered.unwrap_or(*selected) + 1).min(actions.len() - 1),
                        });
                        self.open = true;
                        Some(None)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

pub fn control<'a>(
    target: Control,
    focused: Option<Control>,
    light: bool,
    kind: Kind,
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    Element::new(KeyboardControl {
        target,
        focused: focused == Some(target),
        light,
        kind,
        content: content.into(),
        menu_class: <Theme as menu::Catalog>::default(),
    })
}

pub fn choice<'a, T: Copy + PartialEq + ToString + 'a>(
    target: Control,
    focused: Option<Control>,
    light: bool,
    options: &'a [T],
    selected: T,
    on_select: fn(T) -> Message,
) -> Element<'a, Message> {
    let kind = Kind::Choice {
        labels: options.iter().map(ToString::to_string).collect(),
        actions: options.iter().copied().map(on_select).collect(),
        selected: options
            .iter()
            .position(|option| *option == selected)
            .unwrap_or(0),
    };
    control(
        target,
        focused,
        light,
        kind,
        pick_list(options, Some(selected), on_select),
    )
}

struct KeyboardControl<'a> {
    target: Control,
    focused: bool,
    light: bool,
    kind: Kind,
    content: Element<'a, Message>,
    menu_class: <Theme as menu::Catalog>::Class<'a>,
}

impl Widget<Message, Theme, Renderer> for KeyboardControl<'_> {
    fn tag(&self) -> iced::advanced::widget::tree::Tag {
        iced::advanced::widget::tree::Tag::of::<State>()
    }
    fn state(&self) -> iced::advanced::widget::tree::State {
        iced::advanced::widget::tree::State::new(State::default())
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
        if !self.focused {
            tree.state.downcast_mut::<State>().open = false;
        }
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if shell.is_event_captured() {
            return;
        }
        let state = tree.state.downcast_mut::<State>();
        if matches!(event, Event::Window(iced::window::Event::Unfocused)) {
            state.open = false;
        }
        if self.focused {
            if let Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key,
                modifiers,
                repeat,
                ..
            }) = event
            {
                if let Some(message) = state.key(key, *modifiers, *repeat, &self.kind) {
                    if let Some(message) = message {
                        shell.publish(message);
                    }
                    shell.capture_event();
                    shell.request_redraw();
                    return;
                }
            }
        }
        let press = match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                Some(cursor.is_over(layout.bounds()))
            }
            Event::Touch(iced::touch::Event::FingerPressed { position, .. }) => {
                Some(layout.bounds().contains(*position))
            }
            _ => None,
        };
        if let Some(inside) = press {
            if inside {
                shell.publish(Message::ControlFocus(self.target));
            }
            if let Kind::Choice { selected, .. } = self.kind {
                let was_open = state.open;
                state.open = inside && !was_open;
                state.hovered = Some(selected);
                if inside || was_open {
                    shell.capture_event();
                    shell.request_redraw();
                }
                return;
            }
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        if self.focused {
            let bounds = layout.bounds();
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle {
                        x: bounds.x - 3.,
                        y: bounds.y - 3.,
                        width: bounds.width + 6.,
                        height: bounds.height + 6.,
                    },
                    border: Border {
                        color: if self.light {
                            Color::from_rgb8(28, 31, 28)
                        } else {
                            Color::from_rgb8(245, 240, 232)
                        },
                        width: 2.,
                        radius: 8.into(),
                    },
                    ..Default::default()
                },
                Background::Color(Color::TRANSPARENT),
            );
        }
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        _renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        let state = tree.state.downcast_mut::<State>();
        if !state.open {
            return None;
        }
        let Kind::Choice {
            labels, actions, ..
        } = &self.kind
        else {
            return None;
        };
        let bounds = layout.bounds();
        Some(
            menu::Menu::new(
                &mut state.menu,
                labels,
                &mut state.hovered,
                |label| {
                    state.open = false;
                    actions[labels
                        .iter()
                        .position(|option| *option == label)
                        .expect("menu contains only supplied options")]
                    .clone()
                },
                None,
                &self.menu_class,
            )
            .width(bounds.width)
            .padding(5)
            .overlay(
                layout.position() + translation,
                *viewport,
                bounds.height,
                Length::Shrink,
            ),
        )
    }
}

/// App shortcuts only receive keys that a widget has not already consumed.
pub fn global_message(event: Event, status: iced::event::Status) -> Option<Message> {
    match event {
        Event::Window(iced::window::Event::Focused) => Some(Message::Focus(true)),
        Event::Window(iced::window::Event::Unfocused) => Some(Message::Focus(false)),
        Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: Key::Named(key),
            modifiers,
            repeat,
            ..
        }) if status == iced::event::Status::Ignored
            && !modifiers.command()
            && !modifiers.control()
            && !modifiers.alt() =>
        {
            match key {
                Named::Tab => Some(Message::Traverse(modifiers.shift())),
                Named::ArrowLeft
                | Named::ArrowRight
                | Named::ArrowUp
                | Named::ArrowDown
                | Named::Enter => Some(Message::SceneShortcut(key)),
                Named::F4 if !repeat => Some(Message::Reset(12)),
                Named::F5 if !repeat => Some(Message::Reset(20)),
                Named::F6 if !repeat => Some(Message::Resize(Size::new(1024., 700.))),
                Named::F7 if !repeat => Some(Message::Resize(Size::new(1440., 900.))),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::Filter, App};

    fn press(state: &mut State, key: Named, kind: &Kind) -> Option<Option<Message>> {
        state.key(&Key::Named(key), Modifiers::empty(), false, kind)
    }
    fn filter_choice() -> Kind {
        Kind::Choice {
            labels: Filter::ALL.iter().map(ToString::to_string).collect(),
            actions: Filter::ALL.iter().copied().map(Message::Filter).collect(),
            selected: 0,
        }
    }
    fn event(key: Named) -> Event {
        Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: Key::Named(key),
            modified_key: Key::Named(key),
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat: false,
        })
    }

    #[test]
    fn filter_keyboard_commits_only_on_enter_and_never_opens_a_cube() {
        let mut app = App::new();
        let _ = app.update(Message::Step(1));
        let selected = app.selected;
        let notice = app.notice.clone();
        let kind = filter_choice();
        let mut state = State::default();
        assert!(matches!(press(&mut state, Named::Enter, &kind), Some(None)));
        assert!(matches!(
            press(&mut state, Named::ArrowDown, &kind),
            Some(None)
        ));
        assert_eq!(app.filter, Filter::All);
        let Some(Some(commit)) = press(&mut state, Named::Enter, &kind) else {
            panic!("expected filter commit")
        };
        let _ = app.update(commit);
        assert_eq!(app.filter, Filter::Live);
        assert_eq!(app.selected, selected);
        assert_eq!(app.notice, notice);
        assert!(!state.open);
    }

    #[test]
    fn picker_escape_and_tab_discard_pending_choice_and_home_end_are_bounded() {
        let kind = filter_choice();
        let mut state = State::default();
        let _ = press(&mut state, Named::End, &kind);
        assert_eq!(state.hovered, Some(4));
        let _ = press(&mut state, Named::ArrowDown, &kind);
        assert_eq!(state.hovered, Some(4));
        assert!(matches!(
            press(&mut state, Named::Escape, &kind),
            Some(None)
        ));
        assert!(!state.open);
        let _ = press(&mut state, Named::Home, &kind);
        let _ = press(&mut state, Named::ArrowUp, &kind);
        assert_eq!(state.hovered, Some(0));
        assert!(press(&mut state, Named::Tab, &kind).is_none());
        assert!(!state.open);
    }

    #[test]
    fn captured_keys_and_command_shortcuts_do_not_reach_scene_fallback() {
        for key in [Named::ArrowDown, Named::Enter, Named::Tab, Named::F5] {
            assert!(global_message(event(key), iced::event::Status::Captured).is_none());
        }
        let mut modified = event(Named::Enter);
        if let Event::Keyboard(iced::keyboard::Event::KeyPressed { modifiers, .. }) = &mut modified
        {
            *modifiers = Modifiers::CTRL;
        }
        assert!(global_message(modified, iced::event::Status::Ignored).is_none());
        assert!(matches!(
            global_message(event(Named::Tab), iced::event::Status::Ignored),
            Some(Message::Traverse(false))
        ));
    }

    #[test]
    fn buttons_and_toggles_activate_once_with_space_or_enter() {
        let mut state = State::default();
        let kind = Kind::Action(Message::Create);
        assert!(matches!(
            press(&mut state, Named::Enter, &kind),
            Some(Some(Message::Create))
        ));
        assert!(matches!(
            state.key(&Key::Named(Named::Enter), Modifiers::empty(), true, &kind),
            Some(None)
        ));
        let mut app = App::new();
        let kind = Kind::Action(Message::Float(true));
        let Some(Some(toggle)) = press(&mut state, Named::Space, &kind) else {
            panic!("expected toggle")
        };
        let _ = app.update(toggle);
        assert!(app.float);
    }
}
