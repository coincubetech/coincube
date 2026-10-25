//! Behaviour tests and captures for the second set of Tenshu controls: scope
//! chips, section tabs, the section list, radio rows, disclosure, the copy
//! field, the select and its menu, and the field details found against the
//! live artifact (two-sided hint line, notice, hidden label).
//!
//! Set `TENSHU_GALLERY_DIR` to write `tenshu-choices-*` (Settings, App
//! preferences, Activity and Receive compositions) and
//! `tenshu-select-open-*` (the select menu over the Activity filters). All
//! data is synthetic.

use iced::advanced::layout::Layout;
use iced::advanced::renderer as core_renderer;
use iced::advanced::widget::operation::focusable;
use iced::advanced::widget::Id;
use iced::advanced::{clipboard, Shell};
use iced::keyboard::key;
use iced::{alignment, mouse, Event, Length, Point, Rectangle, Size, Vector};

use super::{near, sample, save, shell, themes, Mounted, Msg, WINDOWS};
use crate::component::tenshu::*;
use crate::icon;
use crate::theme::tenshu::space;
use crate::theme::Theme;
use crate::widget::{Column, Container, Element, Row};

fn selected(name: &'static str) -> impl Fn(u8) -> Msg {
    move |value| Msg::Selected(name, value)
}

fn toggled(name: &'static str) -> impl FnOnce(bool) -> Msg {
    move |value| Msg::Toggled(name, value)
}

fn input(name: &'static str) -> impl Fn(String) -> Msg {
    move |value| Msg::Input(name, value)
}

fn padded(element: impl Into<Element<'static, Msg>>) -> Element<'static, Msg> {
    Container::new(element)
        .padding(12)
        .width(Length::Fill)
        .into()
}

fn ids(mounted: &mut Mounted) -> Vec<Id> {
    mounted
        .collect()
        .focusables
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

fn bounds_of(mounted: &mut Mounted, id: &'static str) -> Rectangle {
    mounted
        .collect()
        .focusables
        .into_iter()
        .find(|(found, _)| *found == Id::new(id))
        .unwrap_or_else(|| panic!("{id} is not a focus stop"))
        .1
}

fn text_bounds(mounted: &mut Mounted, text: &str) -> Rectangle {
    mounted
        .collect()
        .texts
        .into_iter()
        .find(|(_, found)| found == text)
        .unwrap_or_else(|| panic!("missing text {text:?}"))
        .0
}

fn apply(mounted: &mut Mounted, messages: &[Msg]) {
    for message in messages {
        if let Msg::Focus(id) = message {
            mounted.run(focusable::focus::<()>(id.clone()));
        }
    }
}

fn press(mounted: &mut Mounted, named: key::Named) -> Vec<Msg> {
    let mut messages = mounted.key(named, true, false);
    messages.extend(mounted.key(named, false, false));
    messages
}

// ----- Overlay helpers (the select menu) -------------------------------------

/// Whether the mount currently produces an overlay, and its bounds.
fn overlay_bounds(mounted: &mut Mounted) -> Option<Rectangle> {
    let Mounted {
        element,
        tree,
        node,
        renderer,
        size,
    } = mounted;
    let viewport = Rectangle::with_size(*size);
    let mut overlay = element.as_widget_mut().overlay(
        tree,
        Layout::new(node),
        renderer,
        &viewport,
        Vector::ZERO,
    )?;
    // Parents wrap child overlays in a group spanning the window; the menu
    // is the innermost node.
    let mut layout = overlay.as_overlay_mut().layout(renderer, *size);
    while layout.children().len() == 1 {
        layout = layout.children()[0].clone();
    }
    Some(layout.bounds())
}

fn overlay_event(mounted: &mut Mounted, event: Event, cursor: mouse::Cursor) -> Vec<Msg> {
    let Mounted {
        element,
        tree,
        node,
        renderer,
        size,
    } = mounted;
    let viewport = Rectangle::with_size(*size);
    let mut messages = Vec::new();
    if let Some(mut overlay) =
        element
            .as_widget_mut()
            .overlay(tree, Layout::new(node), renderer, &viewport, Vector::ZERO)
    {
        let layout = overlay.as_overlay_mut().layout(renderer, *size);
        overlay.as_overlay_mut().update(
            &event,
            Layout::new(&layout),
            cursor,
            renderer,
            &mut clipboard::Null,
            &mut Shell::new(&mut messages),
        );
    }
    messages
}

fn overlay_click(mounted: &mut Mounted, at: Point) -> Vec<Msg> {
    let cursor = mouse::Cursor::Available(at);
    let mut messages = overlay_event(
        mounted,
        Event::Mouse(mouse::Event::CursorMoved { position: at }),
        cursor,
    );
    messages.extend(overlay_event(
        mounted,
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        cursor,
    ));
    messages
}

/// Pixels with the overlay (if any) drawn over the base layer.
fn pixels_with_overlay(mounted: &mut Mounted, theme: &Theme) -> Vec<u8> {
    use iced::advanced::Renderer as _;
    let Mounted {
        element,
        tree,
        node,
        renderer,
        size,
    } = mounted;
    let viewport = Rectangle::with_size(*size);
    let style = core_renderer::Style {
        text_color: theme.colors.shell.ink,
    };
    renderer.reset(viewport);
    element.as_widget().draw(
        tree,
        renderer,
        theme,
        &style,
        Layout::new(node),
        mouse::Cursor::Unavailable,
        &viewport,
    );
    if let Some(mut overlay) =
        element
            .as_widget_mut()
            .overlay(tree, Layout::new(node), renderer, &viewport, Vector::ZERO)
    {
        let layout = overlay.as_overlay_mut().layout(renderer, *size);
        renderer.with_layer(viewport, |renderer| {
            overlay.as_overlay().draw(
                renderer,
                theme,
                &style,
                Layout::new(&layout),
                mouse::Cursor::Unavailable,
            );
        });
    }
    iced::advanced::renderer::Headless::screenshot(
        renderer,
        Size::new(size.width as u32, size.height as u32),
        1.0,
        theme.colors.shell.background,
    )
}

// ----- Choice forms -----------------------------------------------------------

fn scopes(enabled: bool) -> Element<'static, Msg> {
    choice_chips(
        [
            segment("scope-cube", 0u8, "Cube · Everyday").icon(icon::cube_icon()),
            segment("scope-device", 1, "This device").icon(icon::display_icon()),
            segment("scope-vault", 2, "Vault · Family savings").icon(icon::shield_icon()),
            segment("scope-spark", 3, "Spark · Daily spending").icon(icon::lightning_icon()),
        ],
        Some(0),
        Msg::Focus,
    )
    .on_select_maybe(enabled.then(|| selected("scope")))
    .into()
}

fn section_tabs() -> Element<'static, Msg> {
    tabs(
        [
            segment("tab-display", 0u8, "Display & appearance"),
            segment("tab-backup", 1, "Backup & protection"),
            segment("tab-checks", 2, "Payment checks"),
            segment("tab-ai", 3, "AI access").disabled(),
            segment("tab-data", 4, "Data & exports"),
        ],
        Some(0),
        Msg::Focus,
    )
    .on_select(selected("tab"))
    .into()
}

fn sections() -> Element<'static, Msg> {
    nav_list(
        [
            segment("nav-appearance", 0u8, "Appearance").icon(icon::sun_icon()),
            segment("nav-launcher", 1, "Launcher").icon(icon::cube_icon()),
            segment("nav-balances", 2, "Balances & motion").icon(icon::eye_icon()),
            segment("nav-developer", 3, "Developer").icon(icon::display_icon()),
        ],
        Some(0),
        Msg::Focus,
    )
    .on_select(selected("nav"))
    .into()
}

fn phone_keys() -> Element<'static, Msg> {
    radio_list(
        [
            segment("key-a", 0u8, "Alex's key · a1b2c3d4")
                .description("Signs on this device. Ready to pair."),
            segment("key-b", 1, "Recovery key · e5f6a7b8")
                .description("Held by Keychain on a phone."),
            segment("key-c", 2, "Hardware key · c9d0e1f2")
                .description("A hardware device cannot pair with a phone.")
                .disabled(),
        ],
        Some(0),
        Msg::Focus,
    )
    .on_select(selected("key"))
    .into()
}

#[test]
fn every_choice_form_selects_by_pointer_keys_and_arrows() {
    for (form, element, names, group) in [
        (
            "chips",
            scopes(true),
            vec!["scope-cube", "scope-device", "scope-vault", "scope-spark"],
            "scope",
        ),
        (
            "tabs",
            section_tabs(),
            vec!["tab-display", "tab-backup", "tab-checks", "tab-data"],
            "tab",
        ),
        (
            "nav",
            sections(),
            vec![
                "nav-appearance",
                "nav-launcher",
                "nav-balances",
                "nav-developer",
            ],
            "nav",
        ),
        ("radio", phone_keys(), vec!["key-a", "key-b"], "key"),
    ] {
        let mut mounted = Mounted::new(padded(element), Size::new(900.0, 400.0));
        assert_eq!(
            ids(&mut mounted),
            names.iter().map(|name| Id::new(name)).collect::<Vec<_>>(),
            "{form}: every enabled option is a Tab stop, disabled ones are not"
        );
        let values: Vec<u8> = match form {
            "tabs" => vec![0, 1, 2, 4],
            _ => (0..names.len() as u8).collect(),
        };
        for (name, value) in names.iter().copied().zip(values.iter().copied()) {
            let bounds = bounds_of(&mut mounted, name);
            assert_eq!(
                mounted.click(bounds.center()),
                vec![Msg::Focus(Id::new(name)), Msg::Selected(group, value)],
                "{form} {name}: a click requests focus and selects once"
            );
            mounted.focus(name);
            assert_eq!(
                press(&mut mounted, key::Named::Space),
                vec![Msg::Selected(group, value)],
                "{form} {name}: Space selects once"
            );
        }
        mounted.focus(names[0]);
        let messages = mounted.key(key::Named::ArrowDown, true, false);
        assert_eq!(
            messages,
            vec![
                Msg::Focus(Id::new(names[1])),
                Msg::Selected(group, values[1])
            ],
            "{form}: arrows move to the next enabled option"
        );
        apply(&mut mounted, &messages);
        assert_eq!(mounted.focused(), Some(Id::new(names[1])));
        mounted.focus(names[0]);
        assert_eq!(
            mounted.key(key::Named::ArrowLeft, true, false),
            vec![
                Msg::Focus(Id::new(names.last().unwrap())),
                Msg::Selected(group, *values.last().unwrap())
            ],
            "{form}: arrows wrap"
        );
    }

    let mut chips = Mounted::new(padded(scopes(false)), Size::new(900.0, 200.0));
    assert!(
        ids(&mut chips).is_empty(),
        "a choice without an intent is inert"
    );
    let cube = chips.node.children()[0].bounds();
    assert!(chips.click(cube.center()).is_empty());

    let mut radios = Mounted::new(padded(phone_keys()), Size::new(600.0, 400.0));
    let texts = radios.texts();
    for text in [
        "Signs on this device. Ready to pair.",
        "A hardware device cannot pair with a phone.",
    ] {
        assert!(
            texts.contains(&text.to_string()),
            "radio description {text:?}"
        );
    }
}

#[test]
fn current_tab_is_underlined_and_current_chip_outlined_in_the_accent() {
    for (name, theme) in themes() {
        let shell = theme.colors.shell;
        let mut mounted = Mounted::new(
            padded(
                Column::new()
                    .spacing(16)
                    .push(section_tabs())
                    .push(scopes(true)),
            ),
            Size::new(900.0, 200.0),
        );
        let display = bounds_of(&mut mounted, "tab-display");
        let backup = bounds_of(&mut mounted, "tab-backup");
        let cube = bounds_of(&mut mounted, "scope-cube");
        let device = bounds_of(&mut mounted, "scope-device");
        let pixels = mounted.pixels(&theme, mouse::Cursor::Unavailable);
        let underline = sample(
            &pixels,
            900,
            display.center_x(),
            display.y + display.height - 1.0,
        );
        let plain = sample(
            &pixels,
            900,
            backup.center_x(),
            backup.y + backup.height - 1.0,
        );
        assert!(
            near(underline, shell.accent),
            "{name}: current tab {underline:?}"
        );
        assert!(!near(plain, shell.accent), "{name}: other tabs {plain:?}");
        // The chip's 1px outline may straddle two rows; the nearer one is
        // much closer to the accent than the other chips' line.
        let accent = shell.accent.into_rgba8();
        let distance = |pixel: [u8; 3]| -> u32 {
            (0..3)
                .map(|i| u32::from(pixel[i].abs_diff(accent[i])))
                .sum()
        };
        let edge = |bounds: Rectangle| {
            (-1..=1)
                .map(|dy| {
                    distance(sample(
                        &pixels,
                        900,
                        bounds.center_x(),
                        bounds.y + dy as f32 + 0.5,
                    ))
                })
                .min()
                .unwrap()
        };
        assert!(
            edge(cube) * 2 < edge(device),
            "{name}: current chip outline {} vs {}",
            edge(cube),
            edge(device)
        );
    }
}

// ----- Disclosure and copy field ----------------------------------------------

fn expert(expanded: bool) -> Element<'static, Msg> {
    padded(
        disclosure("expert", "Expert", expanded, Msg::Focus)
            .caption("Resource preset, logs, paths")
            .on_toggle(toggled("expert"))
            .content(body("Sanitized logs are written to this device only.")),
    )
}

#[test]
fn disclosure_toggles_and_shows_content_only_when_expanded() {
    for expanded in [false, true] {
        let mut mounted = Mounted::new(expert(expanded), Size::new(600.0, 200.0));
        assert_eq!(ids(&mut mounted), vec![Id::new("expert")]);
        let texts = mounted.texts();
        assert!(texts.contains(&"Resource preset, logs, paths".to_string()));
        assert_eq!(
            texts.contains(&"Sanitized logs are written to this device only.".to_string()),
            expanded,
            "content is laid out only while expanded"
        );
        let row = bounds_of(&mut mounted, "expert");
        assert_eq!(
            mounted.click(row.center()),
            vec![
                Msg::Focus(Id::new("expert")),
                Msg::Toggled("expert", !expanded)
            ]
        );
        mounted.focus("expert");
        assert_eq!(
            press(&mut mounted, key::Named::Enter),
            vec![Msg::Toggled("expert", !expanded)]
        );
    }
}

fn invoice(copied: bool) -> Element<'static, Msg> {
    padded(
        copy_field(
            "invoice-copy",
            "Lightning invoice",
            "lnbc50u1p…synthetic…7xk",
            Msg::Focus,
        )
        .hint("Share this invoice to receive sats via Lightning")
        .copied(copied)
        .on_copy(Msg::Pressed("copy")),
    )
}

#[test]
fn copy_field_copies_by_pointer_or_key_and_confirms_in_text() {
    let mut mounted = Mounted::new(invoice(false), Size::new(600.0, 160.0));
    assert_eq!(
        ids(&mut mounted),
        vec![Id::new("invoice-copy")],
        "only the Copy button is a focus stop; the value never edits"
    );
    let texts = mounted.texts();
    for text in [
        "Lightning invoice",
        "lnbc50u1p…synthetic…7xk",
        "Copy",
        "Share this invoice to receive sats via Lightning",
    ] {
        assert!(texts.contains(&text.to_string()), "{text}");
    }
    let button = bounds_of(&mut mounted, "invoice-copy");
    assert_eq!(
        mounted.click(button.center()),
        vec![Msg::Focus(Id::new("invoice-copy")), Msg::Pressed("copy")]
    );
    mounted.focus("invoice-copy");
    assert_eq!(
        press(&mut mounted, key::Named::Enter),
        vec![Msg::Pressed("copy")]
    );
    let value = text_bounds(&mut mounted, "lnbc50u1p…synthetic…7xk");
    assert!(
        mounted.click(value.center()).is_empty(),
        "the value is inert"
    );
    mounted.rebuild(invoice(true));
    assert!(mounted.texts().contains(&"Copied".to_string()));
}

// ----- Select -----------------------------------------------------------------

const DIRECTIONS: [(u8, &str); 4] = [(0, "All"), (1, "Received"), (2, "Sent"), (3, "Transfers")];

fn direction(current: u8, enabled: bool) -> Element<'static, Msg> {
    padded(
        Row::new()
            .spacing(12)
            .push(
                select(
                    "direction",
                    "Direction",
                    DIRECTIONS,
                    Some(current),
                    Msg::Focus,
                )
                .width(Length::Fixed(180.0))
                .on_select_maybe(enabled.then(|| selected("direction"))),
            )
            .push(
                ActionButton::new("after", "After", ActionKind::Secondary, Msg::Focus)
                    .compact()
                    .on_press(Msg::Pressed("after")),
            ),
    )
}

#[test]
fn select_opens_and_selects_by_pointer() {
    let mut mounted = Mounted::new(direction(0, true), Size::new(600.0, 400.0));
    assert_eq!(
        ids(&mut mounted),
        vec![Id::new("direction"), Id::new("after")]
    );
    assert!(mounted.texts().contains(&"All".to_string()));
    assert!(overlay_bounds(&mut mounted).is_none(), "closed at rest");

    let label = text_bounds(&mut mounted, "Direction");
    assert_eq!(
        mounted.click(label.center()),
        vec![Msg::Focus(Id::new("direction"))],
        "the label requests focus for the select"
    );

    let field = bounds_of(&mut mounted, "direction");
    let messages = mounted.event(
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        mouse::Cursor::Available(field.center()),
    );
    assert_eq!(messages, vec![Msg::Focus(Id::new("direction"))]);
    let menu = overlay_bounds(&mut mounted).expect("a press opens the menu");
    assert!(
        menu.y >= field.y + field.height,
        "the menu opens below the field"
    );
    assert_eq!(menu.width, field.width);

    // Third row: Sent.
    let sent = Point::new(menu.center_x(), menu.y + 4.0 + 32.0 * 2.0 + 16.0);
    assert_eq!(
        overlay_click(&mut mounted, sent),
        vec![Msg::Selected("direction", 2)]
    );
    assert!(
        overlay_bounds(&mut mounted).is_none(),
        "selecting closes the menu"
    );

    // Pressing elsewhere closes an open menu without selecting.
    mounted.click(field.center());
    assert!(overlay_bounds(&mut mounted).is_some());
    let outside = mounted.event(
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        mouse::Cursor::Available(Point::new(590.0, 390.0)),
    );
    assert!(outside.is_empty());
    assert!(overlay_bounds(&mut mounted).is_none());
}

#[test]
fn select_opens_moves_and_selects_by_keyboard() {
    let mut mounted = Mounted::new(direction(1, true), Size::new(600.0, 400.0));
    mounted.focus("direction");
    assert!(
        press(&mut mounted, key::Named::Enter).is_empty(),
        "Enter opens without selecting"
    );
    assert!(overlay_bounds(&mut mounted).is_some());
    assert!(mounted.key(key::Named::ArrowDown, true, false).is_empty());
    assert!(mounted.key(key::Named::ArrowDown, true, true).is_empty());
    assert_eq!(
        press(&mut mounted, key::Named::Enter),
        vec![Msg::Selected("direction", 3)],
        "the highlight starts on the current option and moves by arrows"
    );
    assert!(overlay_bounds(&mut mounted).is_none());

    assert!(
        press(&mut mounted, key::Named::ArrowUp).is_empty(),
        "↑ opens"
    );
    assert!(press(&mut mounted, key::Named::Home).is_empty());
    assert_eq!(
        press(&mut mounted, key::Named::Space),
        vec![Msg::Selected("direction", 0)]
    );

    press(&mut mounted, key::Named::ArrowDown);
    press(&mut mounted, key::Named::End);
    assert!(
        press(&mut mounted, key::Named::Escape).is_empty(),
        "Escape closes"
    );
    assert!(overlay_bounds(&mut mounted).is_none());
    assert_eq!(mounted.focused(), Some(Id::new("direction")), "focus stays");

    press(&mut mounted, key::Named::Enter);
    assert!(mounted.key(key::Named::Tab, true, false).is_empty());
    assert!(
        overlay_bounds(&mut mounted).is_none(),
        "Tab closes the menu"
    );
}

#[test]
fn disabled_select_is_inert_and_skipped_by_tab() {
    let mut mounted = Mounted::new(direction(0, false), Size::new(600.0, 400.0));
    assert_eq!(ids(&mut mounted), vec![Id::new("after")]);
    let field = mounted
        .collect()
        .texts
        .into_iter()
        .find(|(_, t)| t == "All")
        .unwrap()
        .0;
    assert!(mounted.click(field.center()).is_empty());
    assert!(overlay_bounds(&mut mounted).is_none());
    let label = text_bounds(&mut mounted, "Direction");
    assert!(mounted.click(label.center()).is_empty());
}

// ----- Field details from the live artifact ----------------------------------

#[test]
fn field_shows_notice_and_two_sided_hint_line_and_can_hide_its_label() {
    let mut mounted = Mounted::new(
        padded(
            Column::new()
                .spacing(16)
                .push(
                    text_field("amount", "Amount", "5000", Msg::Focus)
                        .suffix("sats")
                        .notice("This invoice sets its own amount.")
                        .hint("≈ $4.14")
                        .trailing_hint("Available 25,053 SATS"),
                )
                .push(
                    search_field("regions", "Search regions", "", Msg::Focus)
                        .size(FieldSize::Regular)
                        .placeholder("Search regions...")
                        .hide_label()
                        .on_input(input("regions")),
                ),
        ),
        Size::new(600.0, 300.0),
    );
    let collected = mounted.collect();
    let find = |text: &str| {
        collected
            .texts
            .iter()
            .find(|(_, found)| found == text)
            .map(|(bounds, _)| *bounds)
    };
    let fiat = find("≈ $4.14").expect("hint");
    let available = find("Available 25,053 SATS").expect("trailing hint");
    assert!(
        (fiat.y - available.y).abs() < 0.5,
        "both hints share one line"
    );
    assert!(
        available.x > fiat.x + fiat.width,
        "the trailing hint sits to the right"
    );
    assert!(find("This invoice sets its own amount.").is_some());
    assert!(
        find("Search regions").is_none(),
        "a hidden label is not drawn"
    );
    assert!(
        collected
            .focusables
            .iter()
            .any(|(id, _)| *id == Id::new("regions")),
        "a hidden-label field still takes focus"
    );
}

// ----- Composition --------------------------------------------------------------

fn choices_page() -> Element<'static, Msg> {
    let settings = Column::new()
        .spacing(space::M)
        .push(title("Settings"))
        .push(scopes(true))
        .push(section_tabs());
    let prefs = card(
        Row::new()
            .spacing(space::L)
            .push(Container::new(sections()).width(Length::Fixed(232.0)))
            .push(
                Column::new()
                    .width(Length::Fill)
                    .push(section("Appearance"))
                    .push(
                        setting_row(
                            "Currency",
                            select(
                                "currency",
                                "Currency",
                                [
                                    (0u8, "USD · US dollar"),
                                    (1, "EUR · Euro"),
                                    (2, "NGN · Naira"),
                                ],
                                Some(0),
                                Msg::Focus,
                            )
                            .hide_label()
                            .width(Length::Fixed(240.0))
                            .on_select(selected("currency")),
                        )
                        .without_divider(),
                    ),
            ),
    );
    let pairing = Column::new()
        .spacing(space::S)
        .push(section("Key for the phone"))
        .push(phone_keys());
    let filters = Row::new()
        .spacing(space::M)
        .align_y(iced::Alignment::End)
        .push(
            select(
                "filter-wallet",
                "Wallet",
                [
                    (0u8, "All wallets"),
                    (1, "Spark · Daily spending"),
                    (2, "Vault · Family savings"),
                ],
                Some(0),
                Msg::Focus,
            )
            .width(Length::Fixed(170.0))
            .on_select(selected("wallet")),
        )
        .push(
            select(
                "filter-direction",
                "Direction",
                DIRECTIONS,
                Some(0),
                Msg::Focus,
            )
            .width(Length::Fixed(130.0))
            .on_select(selected("direction")),
        )
        .push(
            select(
                "filter-status",
                "Status",
                [
                    (0u8, "All"),
                    (1, "Pending"),
                    (2, "Confirmed"),
                    (3, "Failed"),
                ],
                Some(0),
                Msg::Focus,
            )
            .width(Length::Fixed(130.0))
            .on_select(selected("status")),
        )
        .push(
            search_field("filter-search", "Search label or id", "roof", Msg::Focus)
                .placeholder("Label, txid, payment hash, swap ID")
                .width(Length::Fixed(200.0))
                .on_input(input("filter-search"))
                .on_clear("filter-search-clear", Msg::Cleared("filter-search")),
        )
        .wrap()
        .vertical_spacing(space::M);
    let receive = card(
        Column::new()
            .spacing(space::L)
            .push(
                text_field("receive-amount", "Amount", "5,000", Msg::Focus)
                    .suffix("sats")
                    .hint("≈ $4.14")
                    .trailing_hint("Receiving into Spark")
                    .on_input(input("receive-amount")),
            )
            .push(
                copy_field(
                    "receive-copy",
                    "Lightning invoice",
                    "lnbc50u1p…synthetic…7xk",
                    Msg::Focus,
                )
                .hint("Share this invoice to receive sats via Lightning")
                .on_copy(Msg::Pressed("copy")),
            ),
    );
    let advanced = disclosure("node-expert", "Expert", true, Msg::Focus)
        .caption("Resource preset, logs, paths")
        .on_toggle(toggled("node-expert"))
        .content(
            setting_row(
                "Sanitized logs",
                toggle("logs", true, Msg::Focus)
                    .switch()
                    .on_toggle(toggled("logs")),
            )
            .description("Write logs without addresses or amounts.")
            .without_divider(),
        );
    Column::new()
        .spacing(space::XL)
        .max_width(860)
        .push(settings)
        .push(prefs)
        .push(filters)
        .push(pairing)
        .push(receive)
        .push(advanced)
        .into()
}

#[test]
fn gallery_renders_choices_in_both_themes_and_window_sizes() {
    for (name, theme) in themes() {
        for (width, height) in WINDOWS {
            let window = Size::new(width as f32, height as f32);
            let mut mounted = Mounted::new(shell(choices_page()), window);
            let collected = mounted.collect();
            for expected in [
                "Cube · Everyday",
                "Display & appearance",
                "Appearance",
                "USD · US dollar",
                "All wallets",
                "Key for the phone",
                "Lightning invoice",
                "Expert",
                "Receiving into Spark",
            ] {
                assert!(
                    collected.texts.iter().any(|(_, text)| text == expected),
                    "{name} {width}x{height}: missing {expected:?}"
                );
            }
            for (bounds, text) in &collected.texts {
                assert!(
                    bounds.x + bounds.width <= window.width + 0.5,
                    "{name} {width}x{height}: {text:?} overflows the window"
                );
            }
            let stops: Vec<Id> = collected
                .focusables
                .iter()
                .map(|(id, _)| id.clone())
                .collect();
            for disabled in ["tab-ai", "key-c"] {
                assert!(!stops.contains(&Id::new(disabled)), "{name}: {disabled}");
            }
            for stop in [
                "currency",
                "filter-wallet",
                "receive-copy",
                "node-expert",
                "logs",
            ] {
                assert!(stops.contains(&Id::new(stop)), "{name}: {stop}");
            }
            mounted.focus("tab-backup");
            let pixels = mounted.pixels(&theme, mouse::Cursor::Unavailable);
            save(
                &format!("tenshu-choices-{name}-{width}x{height}"),
                &pixels,
                width,
                height,
            );
        }

        // The select menu, open over the Activity filters.
        let mut filters = Mounted::new(
            Container::new(
                Column::new()
                    .spacing(space::M)
                    .push(section("Activity"))
                    .push(
                        Row::new()
                            .spacing(space::M)
                            .push(
                                select(
                                    "filter-wallet",
                                    "Wallet",
                                    [
                                        (0u8, "All wallets"),
                                        (1, "Spark · Daily spending"),
                                        (2, "Vault · Family savings"),
                                        (3, "Liquid · Travel"),
                                    ],
                                    Some(1),
                                    Msg::Focus,
                                )
                                .width(Length::Fixed(220.0))
                                .on_select(selected("wallet")),
                            )
                            .push(
                                select(
                                    "filter-direction",
                                    "Direction",
                                    DIRECTIONS,
                                    Some(0),
                                    Msg::Focus,
                                )
                                .width(Length::Fixed(160.0))
                                .on_select(selected("direction")),
                            ),
                    ),
            )
            .padding(24)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_y(alignment::Vertical::Top)
            .style(crate::theme::tenshu::surface::page),
            Size::new(520.0, 320.0),
        );
        filters.focus("filter-wallet");
        press(&mut filters, key::Named::Enter);
        press(&mut filters, key::Named::ArrowDown);
        let menu = overlay_bounds(&mut filters).expect("open menu");
        let pixels = pixels_with_overlay(&mut filters, &theme);
        let highlight = sample(&pixels, 520, menu.x + 8.0, menu.y + 4.0 + 32.0 * 2.0 + 16.0);
        assert!(
            near(highlight, theme.colors.shell.selected),
            "{name}: highlighted row {highlight:?}"
        );
        let field = bounds_of(&mut filters, "filter-wallet");
        let border = sample(&pixels, 520, field.center_x(), field.y + 1.0);
        assert!(
            near(border, theme.colors.shell.focus),
            "{name}: select focus {border:?}"
        );
        save(&format!("tenshu-select-open-{name}"), &pixels, 520, 320);
    }
}
