//! Tenshu page head (`.phead`): the pinned top of every Cube page.
//!
//! The head sits above the page's scrollable, on the same centred column as
//! [`page`](super::page): the title and the Cube line on the left, the page's
//! actions on the right, and an optional row of tabs or steps sitting on a
//! 1px rule that spans the content pane. Only the body under it scrolls.
//! The rule is written up in `design-review/HANDOFF.md`, "v175 delta".
//!
//! The title and the actions share a row while they fit, bottom-aligned
//! (`.hrow { align-items: flex-end }`). In a narrower column the actions move
//! under the title and wrap, so the head never pushes them off the page.

use iced::advanced::text::Shaping;
use iced::advanced::{
    layout, mouse, overlay, renderer,
    widget::{Id, Operation, Tree},
    Clipboard, Layout, Shell, Widget,
};
use iced::widget::text::{IntoFragment, LineHeight, Wrapping};
use iced::{alignment, Alignment, Background, Event, Length, Padding, Rectangle, Size, Vector};

use super::page::{page, PageWidth};
use crate::font;
use crate::icon;
use crate::theme::tenshu::button::{self as button_style, ActionKind, ActionShape};
use crate::theme::tenshu::{radius, size, text as text_style, type_size};
use crate::theme::Theme;
use crate::widget::action::Action;
use crate::widget::{Column, ColumnExt, Container, Element, Renderer, Row};

/// Space above the title (`.phead > .in { padding-top: 24px }`).
pub const HEAD_TOP: f32 = 24.0;
/// Space under the title row when no rule row follows
/// (`.phead > .in { padding-bottom: 20px }`).
pub const HEAD_BOTTOM: f32 = 20.0;
/// Space between the title row and a rule row (`.hsteps { margin-top: 16px }`).
/// The rule row then sits directly on the rule (`margin-bottom: -20px`).
pub const RULE_ROW_GAP: f32 = 16.0;
/// Space above a page body under a head (`.main .pg { padding-top: 24px }`).
pub const BODY_TOP: f32 = 24.0;
/// Gap between the title and the actions (`.hrow { gap: 16px }`), and between
/// the title and actions stacked under it.
pub const ROW_GAP: f32 = 16.0;
/// Gap between actions (`.hact { gap: 8px }`).
pub const ACTION_GAP: f32 = 8.0;
/// `.phead .hact .back { height: 36px }`.
pub const BACK_HEIGHT: f32 = 36.0;
/// The back action's id. A page has at most one.
pub const BACK_ID: &str = "tenshu.page_head.back";
/// `.phead h1 { line-height: 1.25 }`.
const TITLE_LINE_HEIGHT: f32 = 1.25;
/// `.idl { margin-top: 4px }`.
const CONTEXT_GAP: f32 = 4.0;
/// `.idl { gap: 6px }`.
const CONTEXT_MARK_GAP: f32 = 6.0;
/// `.idl { font-size: 13px }`.
const CONTEXT_SIZE: f32 = 13.0;

/// A page head. Build it, then place it with [`PageHead::view`].
pub struct PageHead<'a, M> {
    title: Element<'a, M>,
    context: Option<Element<'a, M>>,
    actions: Vec<Element<'a, M>>,
    back: Option<Element<'a, M>>,
    rule: Option<Element<'a, M>>,
    focus_intent: fn(Id) -> M,
}

/// A head titled `title` (22px semibold). `focus_intent` must apply Iced's
/// whole-tree focus operation for the given `Id`, as [`Action::new`] requires;
/// the head uses it for [`PageHead::back`].
pub fn page_head<'a, M>(
    title: impl IntoFragment<'a>,
    focus_intent: fn(Id) -> M,
) -> PageHead<'a, M> {
    PageHead {
        title: super::text::title(title)
            .line_height(LineHeight::Relative(TITLE_LINE_HEIGHT))
            .wrapping(Wrapping::WordOrGlyph)
            .into(),
        context: None,
        actions: Vec::new(),
        back: None,
        rule: None,
        focus_intent,
    }
}

impl<'a, M: Clone + 'static> PageHead<'a, M> {
    /// The line under the title (`.idl`): a 16px `mark`, usually the Cube's
    /// coin, then `line`, such as "‹wallet or Cube› · ‹network›".
    pub fn context(mut self, mark: impl Into<Element<'a, M>>, line: impl IntoFragment<'a>) -> Self {
        self.context = Some(
            Row::new()
                .spacing(CONTEXT_MARK_GAP)
                .align_y(Alignment::Center)
                .push(
                    Container::new(mark)
                        .width(Length::Fixed(size::ICON_SMALL))
                        .height(Length::Fixed(size::ICON_SMALL))
                        .center_x(Length::Fixed(size::ICON_SMALL))
                        .center_y(Length::Fixed(size::ICON_SMALL)),
                )
                .push(
                    super::text::caption(line)
                        .size(CONTEXT_SIZE)
                        .wrapping(Wrapping::WordOrGlyph),
                )
                .into(),
        );
        self
    }

    /// A page action on the right, after the ones already added.
    pub fn action(mut self, action: impl Into<Element<'a, M>>) -> Self {
        self.actions.push(action.into());
        self
    }

    /// "Back to ‹wallet›", always the last action, whatever order the head
    /// is built in.
    pub fn back(mut self, wallet: impl std::fmt::Display, on_press: M) -> Self {
        self.back = Some(back_action(
            format!("Back to {wallet}"),
            self.focus_intent,
            on_press,
        ));
        self
    }

    /// Tabs or steps under the title, sitting on the head's rule.
    pub fn rule(mut self, row: impl Into<Element<'a, M>>) -> Self {
        self.rule = Some(row.into());
        self
    }

    /// The head on the page column, with its rule across the whole pane.
    /// `gutter` is the same mapping the page body uses (see [`page`]).
    pub fn view(self, gutter: fn(f32) -> f32) -> Element<'a, M> {
        let PageHead {
            title,
            context,
            mut actions,
            back,
            rule,
            focus_intent: _,
        } = self;
        let title_block = Column::new()
            .spacing(CONTEXT_GAP)
            .push(title)
            .push_maybe(context);
        actions.extend(back);
        let actions = (!actions.is_empty()).then(|| -> Element<'a, M> {
            Row::with_children(actions)
                .spacing(ACTION_GAP)
                .align_y(Alignment::Center)
                .wrap()
                .vertical_spacing(ACTION_GAP)
                .into()
        });
        let has_rule = rule.is_some();
        let inner = Column::new()
            .spacing(RULE_ROW_GAP)
            .push(HeadRow {
                children: std::iter::once(title_block.into()).chain(actions).collect(),
            })
            .push_maybe(rule);
        Column::new()
            .push(
                Container::new(
                    page(inner, PageWidth::Reading, gutter)
                        .vertical(HEAD_TOP, if has_rule { 0.0 } else { HEAD_BOTTOM }),
                )
                .width(Length::Fill)
                .style(|theme: &Theme| iced::widget::container::Style {
                    background: Some(Background::Color(theme.colors.shell.background)),
                    ..Default::default()
                }),
            )
            .push(iced::widget::rule::horizontal(1).style(|theme: &Theme| {
                iced::widget::rule::Style {
                    color: theme.colors.shell.line,
                    ..crate::theme::rule::default(theme)
                }
            }))
            .width(Length::Fill)
            .into()
    }
}

/// `.phead .hact .back`: a 36px outlined action with a muted chevron.
fn back_action<'a, M: Clone + 'static>(
    label: String,
    focus_intent: fn(Id) -> M,
    on_press: M,
) -> Element<'a, M> {
    let content = Row::new()
        .spacing(6)
        .align_y(Alignment::Center)
        .push(
            icon::previous_icon()
                .size(size::ICON_SMALL)
                .style(text_style::muted),
        )
        .push(
            iced::widget::text(label)
                .shaping(Shaping::Advanced)
                .font(font::MEDIUM)
                .size(type_size::CHIP),
        );
    Action::new(
        Id::new(BACK_ID),
        Container::new(content)
            .height(Length::Fill)
            .align_y(alignment::Vertical::Center),
        focus_intent,
    )
    .style(|theme, status| {
        button_style::action(theme, status, ActionKind::Secondary, ActionShape::Chip)
    })
    .height(Length::Fixed(BACK_HEIGHT))
    .padding(Padding {
        top: 0.0,
        right: 12.0,
        bottom: 0.0,
        left: 8.0,
    })
    .focus_radius(radius::CHIP)
    .on_press(on_press)
    .into()
}

/// `.hrow`: the title block, then the actions if there are any.
struct HeadRow<'a, M> {
    children: Vec<Element<'a, M>>,
}

/// Where the actions go in a column `width` wide: beside the title when
/// both fit on one line, else under it.
fn actions_beside(width: f32, title_width: f32, actions_width: f32) -> bool {
    title_width + ROW_GAP + actions_width <= width
}

impl<M> Widget<M, Theme, Renderer> for HeadRow<'_, M> {
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let width = limits.max().width;
        let bounded = |width: f32| layout::Limits::new(Size::ZERO, Size::new(width, f32::INFINITY));
        let (title, rest) = self.children.split_first_mut().expect("a title");
        let (title_tree, rest_tree) = tree.children.split_first_mut().expect("a title");
        let Some((actions, actions_tree)) = rest.first_mut().zip(rest_tree.first_mut()) else {
            let title = title
                .as_widget_mut()
                .layout(title_tree, renderer, &bounded(width));
            let size = title.size();
            let width = if width.is_finite() { width } else { size.width };
            return layout::Node::with_children(Size::new(width, size.height), vec![title]);
        };
        let actions_node = actions
            .as_widget_mut()
            .layout(actions_tree, renderer, &bounded(width));
        let actions_size = actions_node.size();
        // The title's one-line width: laid out with no bound, it never wraps.
        let one_line = title
            .as_widget_mut()
            .layout(title_tree, renderer, &bounded(f32::INFINITY))
            .size()
            .width;
        let width = if width.is_finite() {
            width
        } else {
            one_line + ROW_GAP + actions_size.width
        };
        let (title_node, actions_node, height) =
            if actions_beside(width, one_line, actions_size.width) {
                let title_node = title.as_widget_mut().layout(
                    title_tree,
                    renderer,
                    &bounded(width - ROW_GAP - actions_size.width),
                );
                let title_height = title_node.size().height;
                let height = title_height.max(actions_size.height);
                (
                    title_node.move_to((0.0, height - title_height)),
                    actions_node
                        .move_to((width - actions_size.width, height - actions_size.height)),
                    height,
                )
            } else {
                let title_node =
                    title
                        .as_widget_mut()
                        .layout(title_tree, renderer, &bounded(width));
                let top = title_node.size().height + ROW_GAP;
                (
                    title_node,
                    actions_node.move_to((0.0, top)),
                    top + actions_size.height,
                )
            };
        layout::Node::with_children(Size::new(width, height), vec![title_node, actions_node])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, state), layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(state, layout, renderer, operation);
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
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
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
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
        for ((child, state), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: 'a> From<HeadRow<'a, M>> for Element<'a, M> {
    fn from(row: HeadRow<'a, M>) -> Self {
        Element::new(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::tenshu::{ActionButton, PAGE_MAX};
    use iced::advanced::renderer::Headless;
    use iced::advanced::widget::operation::Focusable;

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    enum Msg {
        Focus(Id),
        Back,
        Settings,
        Tools,
    }

    const GUTTER: f32 = 24.0;

    fn gutter(_: f32) -> f32 {
        GUTTER
    }

    fn renderer() -> Renderer {
        for bytes in crate::font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        iced::futures::executor::block_on(<Renderer as Headless>::new(
            crate::font::REGULAR,
            14.into(),
            Some("tiny-skia"),
        ))
        .unwrap()
    }

    #[derive(Default)]
    struct Probe {
        text: Vec<(String, Rectangle)>,
        focus: Vec<(String, Rectangle)>,
    }

    impl Operation for Probe {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn text(&mut self, _id: Option<&Id>, bounds: Rectangle, value: &str) {
            self.text.push((value.to_owned(), bounds));
        }

        fn focusable(&mut self, id: Option<&Id>, bounds: Rectangle, _: &mut dyn Focusable) {
            let name = match id {
                Some(id) if *id == Id::new(BACK_ID) => "back".to_owned(),
                Some(id) if *id == Id::new("settings") => "settings".to_owned(),
                Some(id) if *id == Id::new("tools") => "tools".to_owned(),
                other => format!("{other:?}"),
            };
            self.focus.push((name, bounds));
        }
    }

    impl Probe {
        fn text(&self, value: &str) -> Rectangle {
            self.text
                .iter()
                .find(|(text, _)| text == value)
                .unwrap_or_else(|| panic!("no {value:?} in {:?}", self.text))
                .1
        }

        fn focus(&self, name: &str) -> Rectangle {
            self.focus
                .iter()
                .find(|(focus, _)| focus == name)
                .unwrap_or_else(|| panic!("no {name:?} in {:?}", self.focus))
                .1
        }

        /// The actions in reading order: by line (actions of different
        /// heights share a centre line), then left to right.
        fn reading_order(&self) -> Vec<&str> {
            let mut focus: Vec<_> = self.focus.iter().collect();
            focus.sort_by(|(_, a), (_, b)| {
                (a.center_y().round(), a.x)
                    .partial_cmp(&(b.center_y().round(), b.x))
                    .unwrap()
            });
            focus.into_iter().map(|(name, _)| name.as_str()).collect()
        }
    }

    fn action(id: &'static str, label: &'static str, message: Msg) -> Element<'static, Msg> {
        ActionButton::new(id, label, ActionKind::Secondary, Msg::Focus)
            .compact()
            .on_press(message)
            .into()
    }

    /// Back is added first on purpose: it must still land last.
    fn head() -> PageHead<'static, Msg> {
        page_head("Swap BTC ⇄ USDB", Msg::Focus)
            .context(
                iced::widget::Space::new().width(16).height(16),
                "Everyday · Bitcoin mainnet",
            )
            .back("Spark", Msg::Back)
            .action(action("settings", "Wallet settings", Msg::Settings))
            .action(action("tools", "Tools", Msg::Tools))
    }

    /// Lays `head` out in a pane `width` wide and probes it.
    fn probe(head: PageHead<'static, Msg>, width: f32) -> (Probe, Size) {
        let renderer = renderer();
        let mut element = head.view(gutter);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(width, 768.0)),
        );
        let mut probe = Probe::default();
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut probe);
        (probe, node.size())
    }

    #[test]
    fn back_is_the_last_action_on_the_column_edge_beside_the_title() {
        // Wide enough for the whole 1120px column.
        let pane = PAGE_MAX + 2.0 * GUTTER + 80.0;
        let (probe, _) = probe(head(), pane);
        assert_eq!(probe.reading_order(), ["settings", "tools", "back"]);
        assert!(probe.text.iter().any(|(text, _)| text == "Back to Spark"));

        let column_right = (pane + PAGE_MAX) / 2.0;
        let back = probe.focus("back");
        assert!(
            (back.x + back.width - column_right).abs() < 0.5,
            "Back ends on the column's right edge: {back:?}, edge {column_right}"
        );
        assert_eq!(back.height, BACK_HEIGHT);
        let tools = probe.focus("tools");
        assert!(
            (back.x - (tools.x + tools.width) - ACTION_GAP).abs() < 0.5,
            "{ACTION_GAP}px between actions: {tools:?} {back:?}"
        );

        // `.hrow { align-items: flex-end }`: the actions' bottom is the Cube
        // line's, and the title starts at the column's left edge.
        let title = probe.text("Swap BTC ⇄ USDB");
        let context = probe.text("Everyday · Bitcoin mainnet");
        assert!((title.x - (pane - PAGE_MAX) / 2.0).abs() < 0.5, "{title:?}");
        assert!((title.y - HEAD_TOP).abs() < 0.5, "{title:?}");
        assert!(
            (back.y + back.height - (context.y + context.height)).abs() < 1.0,
            "bottom-aligned: {back:?} {context:?}"
        );
        assert!(context.y > title.y + title.height);
    }

    #[test]
    fn a_narrow_column_moves_the_actions_under_the_title_and_back_stays_last() {
        let pane = 360.0;
        let (probe, size) = probe(head(), pane);
        let context = probe.text("Everyday · Bitcoin mainnet");
        let order = probe.reading_order();
        assert_eq!(order.last(), Some(&"back"), "{order:?}");
        for (name, bounds) in &probe.focus {
            assert!(
                bounds.y >= context.y + context.height + ROW_GAP - 0.5,
                "{name} sits under the title: {bounds:?}"
            );
            assert!(
                bounds.x >= GUTTER - 0.5 && bounds.x + bounds.width <= pane - GUTTER + 0.5,
                "{name} stays inside the gutters: {bounds:?}"
            );
        }
        let lowest = probe
            .focus
            .iter()
            .map(|(_, bounds)| bounds.y + bounds.height)
            .fold(0.0, f32::max);
        // 20px under the last action, then the 1px rule.
        assert!(
            (size.height - (lowest + HEAD_BOTTOM + 1.0)).abs() < 0.5,
            "{size:?}"
        );
    }

    #[test]
    fn a_rule_row_sits_on_the_rule() {
        let (probe, size) = probe(
            page_head::<Msg>("Send", Msg::Focus).rule(crate::component::tenshu::text::body(
                "Details · Review · Sent",
            )),
            1024.0,
        );
        let title = probe.text("Send");
        let steps = probe.text("Details · Review · Sent");
        assert!(
            (steps.y - (title.y + title.height + RULE_ROW_GAP)).abs() < 0.5,
            "{title:?} {steps:?}"
        );
        assert!(
            (size.height - 1.0 - (steps.y + steps.height)).abs() < 0.5,
            "no padding between the row and the 1px rule: {steps:?} {size:?}"
        );
    }

    #[test]
    fn a_head_without_actions_is_the_title_block() {
        let (probe, size) = probe(page_head::<Msg>("Settings", Msg::Focus), 1024.0);
        assert!(probe.focus.is_empty());
        let title = probe.text("Settings");
        assert!(
            (size.height - (HEAD_TOP + title.height + HEAD_BOTTOM + 1.0)).abs() < 0.5,
            "{title:?} {size:?}"
        );
    }
}
