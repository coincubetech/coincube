//! Cube ring marks and person initials, version 1.
//!
//! A Cube's avatar is drawn by the app, never stored as a picture: two or
//! three arcs on circles of decreasing size, one of them orange, around a
//! centre dot. The arcs come from a small seeded generator whose seed is the
//! Cube's UUID and its variation number, so every member of a Cube draws the
//! same mark on COINCUBE Desktop and COINCUBE Mobile.
//!
//! People are never drawn as a ring mark: they are one or two initials in a
//! thin ring ([`person_mark`]).
//!
//! The rules are `SPEC-cube-ring-mark-v1.md` in the company brain
//! (`plans/cube-avatars/`). Version 1 is frozen once shipped: any change to a
//! number here redraws every Cube for every member, so a different algorithm
//! is a new spec version, never an edit to this one. The spec's fixture is
//! copied unchanged into `ring_mark/testdata/` and asserted in the tests.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;

use iced::widget::{stack, svg as iced_svg, text};
use iced::{alignment, Color, Length};
use unicode_segmentation::UnicodeSegmentation;

use crate::font;
use crate::theme::Theme;
use crate::widget::{Container, Element};

/// The spec version this module draws.
pub const VERSION: u32 = 1;

/// The side of the box every mark is laid out on, before scaling.
const BOX: f64 = 64.0;
const CENTRE: f64 = 32.0;
const ARC_STROKE: f64 = 2.6;
const DOT_RADIUS: f64 = 2.6;
const PERSON_RING_RADIUS: f64 = 29.0;
const PERSON_RING_STROKE: f64 = 2.4;
const PERSON_RING_OPACITY: f64 = 0.28;
const PERSON_TEXT_SIZE: f32 = 22.0;
/// The initials sit one unit below the centre on the 64 box (spec section 7).
const PERSON_TEXT_DROP: f32 = 1.0;

/// The smallest side a Cube mark may be drawn at (spec section 6). Below it
/// the inner arc and the dot begin to merge.
pub const MIN_SIZE: f32 = 24.0;

/// FNV-1a 32-bit offset basis and prime.
const FNV_OFFSET: u32 = 2_166_136_261;
const FNV_PRIME: u32 = 16_777_619;
/// Each draw is `0..DRAW_RANGE`.
const DRAW_RANGE: u32 = 100_000;
/// The start angle's scale. `6.28` is deliberate, not 2π: it is what the
/// study drew, and "correcting" it would change every mark (spec section 5).
#[allow(clippy::approx_constant)]
const START_SCALE: f64 = 6.28;

/// The seed for a Cube's mark (spec section 3), or `None` when `cube_id` is
/// not a UUID. The id is lowercased first; variation 0 is the plain id and
/// any other variation appends `:` and its decimal digits.
///
/// Only the Cube's UUID may go in here: never its name, its Lightning address
/// or the server's numeric row id (master plan invariant RM2).
pub fn seed(cube_id: &str, variation: u32) -> Option<String> {
    let id = cube_id.to_ascii_lowercase();
    if !is_uuid_shape(&id) {
        return None;
    }
    Some(if variation == 0 {
        id
    } else {
        format!("{id}:{variation}")
    })
}

/// Whether `seed` is one [`seed`] makes: a lowercase UUID, alone or followed
/// by `:` and a decimal variation above 0 with no leading zero. The widgets
/// draw only from such a seed, so a raw id passed by mistake (uppercase, or
/// a numeric row id) draws the fallback dot, never a mark from another seed.
pub fn is_seed(seed: &str) -> bool {
    let (id, variation) = match seed.split_once(':') {
        Some((id, variation)) => (id, Some(variation)),
        None => (seed, None),
    };
    is_uuid_shape(id)
        && variation.is_none_or(|v| {
            !v.is_empty()
                && !v.starts_with('0')
                && v.bytes().all(|b| b.is_ascii_digit())
                && v.parse::<u32>().is_ok()
        })
}

/// 36 ASCII characters: lowercase hex in groups of 8-4-4-4-12, hyphenated.
fn is_uuid_shape(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, &b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

/// The spec's generator (section 4): FNV-1a over the seed's bytes, then
/// xorshift32 for each draw. All arithmetic is wrapping `u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generator {
    state: u32,
}

impl Generator {
    pub fn new(seed: &str) -> Self {
        let state = seed.bytes().fold(FNV_OFFSET, |h, b| {
            (h ^ u32::from(b)).wrapping_mul(FNV_PRIME)
        });
        Self { state }
    }

    /// The current state; right after [`Generator::new`] it is the FNV hash.
    pub fn state(&self) -> u32 {
        self.state
    }

    /// The next draw `u`, in `0..100000`.
    pub fn next_draw(&mut self) -> u32 {
        let mut h = self.state;
        h ^= h << 13;
        h ^= h >> 17;
        h ^= h << 5;
        self.state = h;
        h % DRAW_RANGE
    }
}

/// One arc of a mark, on the 64 box. Angles are radians from the positive
/// x-axis, increasing clockwise on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RingArc {
    pub radius: f64,
    pub start: f64,
    pub sweep: f64,
}

impl RingArc {
    /// Whether an SVG path needs the large-arc flag for this arc.
    pub fn large_arc(&self) -> bool {
        self.sweep > std::f64::consts::PI
    }

    /// The arc as an SVG path on the 64 box, coordinates to two decimals.
    pub fn svg_path(&self) -> String {
        let (x0, y0) = point(self.radius, self.start);
        let (x1, y1) = point(self.radius, self.start + self.sweep);
        format!(
            "M{x0:.2} {y0:.2}A{r} {r} 0 {large} 1 {x1:.2} {y1:.2}",
            r = self.radius,
            large = u8::from(self.large_arc()),
        )
    }
}

fn point(radius: f64, angle: f64) -> (f64, f64) {
    (CENTRE + radius * angle.cos(), CENTRE + radius * angle.sin())
}

/// A Cube's mark: its arcs, outermost first, and which one is orange.
#[derive(Debug, Clone, PartialEq)]
pub struct RingMark {
    pub arcs: Vec<RingArc>,
    pub orange: usize,
}

impl RingMark {
    /// The mark for a seed made by [`seed`] (spec section 5).
    pub fn from_seed(seed: &str) -> Self {
        let mut generator = Generator::new(seed);
        let k = 2 + (generator.next_draw() * 2) / DRAW_RANGE;
        let orange = ((generator.next_draw() * k) / DRAW_RANGE) as usize;
        let arcs = (0..k)
            .map(|i| {
                let ua = generator.next_draw();
                let ul = generator.next_draw();
                // Divide, then multiply, then add, as written in the spec.
                RingArc {
                    radius: 27.0 - 8.5 * f64::from(i),
                    start: (f64::from(ua) / f64::from(DRAW_RANGE)) * START_SCALE,
                    sweep: 2.2 + (f64::from(ul) / f64::from(DRAW_RANGE)) * 3.2,
                }
            })
            .collect();
        Self { arcs, orange }
    }
}

/// The mark's shapes on the 64 box, without the `<svg>` wrapper, so a caller
/// building a larger SVG (the Launcher's faces) can place it in a `<g>`.
/// With no mark, the centre dot alone (the spec's fallback).
pub fn svg_body(mark: Option<&RingMark>, ink: Color, accent: Color) -> String {
    let ink = hex(ink);
    let accent = hex(accent);
    let mut body = String::new();
    if let Some(mark) = mark {
        for (i, arc) in mark.arcs.iter().enumerate() {
            let stroke = if i == mark.orange { &accent } else { &ink };
            let _ = write!(
                body,
                r#"<path d="{}" fill="none" stroke="{stroke}" stroke-width="{ARC_STROKE}" stroke-linecap="round"/>"#,
                arc.svg_path(),
            );
        }
    }
    let _ = write!(
        body,
        r#"<circle cx="{CENTRE}" cy="{CENTRE}" r="{DOT_RADIUS}" fill="{ink}"/>"#
    );
    body
}

/// A complete SVG document for the mark on the 64 box, in the colours given.
/// Callers pass theme tokens (`theme.colors.shell.ink` and `.accent`), never
/// literals.
pub fn svg(mark: Option<&RingMark>, ink: Color, accent: Color) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {BOX} {BOX}">{}</svg>"#,
        svg_body(mark, ink, accent)
    )
}

/// `#rrggbb` for an opaque colour.
fn hex(color: Color) -> String {
    let [r, g, b, _] = color.into_rgba8();
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// How a Cube mark is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    /// Ink arcs and dot, one accent arc.
    Shell,
    /// Every part in the muted colour: a Cube that lives on another computer.
    Muted,
}

/// The two monochrome layers of a mark: everything drawn in ink, and the
/// orange arc. Each layer is tinted by the theme at draw time, so one cached
/// pair serves both themes.
#[derive(Clone)]
struct Layers {
    ink: iced_svg::Handle,
    accent: Option<iced_svg::Handle>,
}

/// Keeps the layers for the marks on screen. Bounded: a Launcher shows a
/// handful of Cubes, so the cache is simply cleared when it grows past this.
const LAYER_CACHE_LIMIT: usize = 256;

thread_local! {
    static LAYER_CACHE: RefCell<HashMap<Option<String>, Layers>> = RefCell::new(HashMap::new());
}

fn layers(seed: Option<&str>) -> Layers {
    // Anything that is not a seed draws the dot, and is cached as `None`.
    let seed = seed.filter(|seed| is_seed(seed));
    let key = seed.map(str::to_owned);
    LAYER_CACHE.with(|cache| {
        if let Some(layers) = cache.borrow().get(&key) {
            return layers.clone();
        }
        let layers = build_layers(seed);
        let mut cache = cache.borrow_mut();
        if cache.len() >= LAYER_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, layers.clone());
        layers
    })
}

fn build_layers(seed: Option<&str>) -> Layers {
    let mark = seed.map(RingMark::from_seed);
    // Black stands in for each layer's colour; the theme's tint replaces it.
    let black = Color::BLACK;
    let ink_only = mark.as_ref().map(|mark| RingMark {
        arcs: mark
            .arcs
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != mark.orange)
            .map(|(_, arc)| *arc)
            .collect(),
        // Out of range: no arc in this layer is the orange one.
        orange: usize::MAX,
    });
    let ink = svg(ink_only.as_ref(), black, black);
    let accent = mark.as_ref().map(|mark| {
        let path = mark.arcs[mark.orange].svg_path();
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {BOX} {BOX}"><path d="{path}" fill="none" stroke="{}" stroke-width="{ARC_STROKE}" stroke-linecap="round"/></svg>"#,
            hex(black)
        )
    });
    Layers {
        ink: iced_svg::Handle::from_memory(ink.into_bytes()),
        accent: accent.map(|svg| iced_svg::Handle::from_memory(svg.into_bytes())),
    }
}

/// The side a Cube mark is actually drawn at: never below [`MIN_SIZE`].
/// Debug builds assert, so a too-small call site is caught in development;
/// release builds clamp.
fn mark_size(size: f32) -> f32 {
    debug_assert!(
        size >= MIN_SIZE,
        "a Cube ring mark must be at least {MIN_SIZE}px, got {size}"
    );
    size.max(MIN_SIZE)
}

/// A Cube's mark at `size` px square, in the theme's ink and accent. `seed`
/// comes from [`seed`]; with `None`, or anything [`is_seed`] refuses, only
/// the centre dot is drawn, never a mark from a made-up seed.
///
/// The mark is decoration beside the Cube's name: always show the name with
/// it. Wherever a Cube is chosen or confirmed, the name and network do that
/// job (master plan invariant RM6).
pub fn ring_mark<'a, M: 'a>(seed: Option<&str>, size: f32) -> Element<'a, M> {
    draw_mark(seed, size, Tone::Shell)
}

/// A Cube's mark with every part in the muted colour, for a Cube that lives
/// on another computer.
pub fn ring_mark_muted<'a, M: 'a>(seed: Option<&str>, size: f32) -> Element<'a, M> {
    draw_mark(seed, size, Tone::Muted)
}

fn draw_mark<'a, M: 'a>(seed: Option<&str>, size: f32, tone: Tone) -> Element<'a, M> {
    let size = Length::Fixed(mark_size(size));
    let Layers { ink, accent } = layers(seed);
    let ink_layer = iced_svg(ink)
        .width(size)
        .height(size)
        .style(move |theme: &Theme, _| iced_svg::Style {
            color: Some(match tone {
                Tone::Shell => theme.colors.shell.ink,
                Tone::Muted => theme.colors.shell.muted,
            }),
        });
    let mut layers = stack![ink_layer].width(size).height(size);
    if let Some(accent) = accent {
        layers = layers.push(iced_svg(accent).width(size).height(size).style(
            move |theme: &Theme, _| iced_svg::Style {
                color: Some(match tone {
                    Tone::Shell => theme.colors.shell.accent,
                    Tone::Muted => theme.colors.shell.muted,
                }),
            },
        ));
    }
    layers.into()
}

/// The thin ring drawn around a person's initials (spec section 7). Its
/// opacity is in the SVG: the theme's tint replaces colour, not alpha.
fn person_ring_svg() -> String {
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {BOX} {BOX}"><circle cx="{CENTRE}" cy="{CENTRE}" r="{PERSON_RING_RADIUS}" fill="none" stroke="#000000" stroke-opacity="{PERSON_RING_OPACITY}" stroke-width="{PERSON_RING_STROKE}"/></svg>"##
    )
}

thread_local! {
    static PERSON_RING: iced_svg::Handle = iced_svg::Handle::from_memory(person_ring_svg().into_bytes());
}

/// A person at `size` px square: their initials (see [`initials`]) in a thin
/// ring, in IBM Plex Sans 600. With no initials the ring is drawn alone;
/// a "?" is never drawn. People are never drawn as a ring mark, and Cubes are
/// never drawn as initials (master plan invariant RM7).
pub fn person_mark<'a, M: 'a>(
    display_name: Option<&str>,
    email: Option<&str>,
    size: f32,
) -> Element<'a, M> {
    let side = Length::Fixed(size);
    let scale = size / BOX as f32;
    let ring = iced_svg(PERSON_RING.with(Clone::clone))
        .width(side)
        .height(side)
        .style(|theme: &Theme, _| iced_svg::Style {
            color: Some(theme.colors.shell.ink),
        });
    let letters = text(initials(display_name, email))
        .font(font::SEMIBOLD)
        .size(PERSON_TEXT_SIZE * scale)
        .line_height(text::LineHeight::Relative(1.0))
        .align_x(alignment::Horizontal::Center)
        .align_y(alignment::Vertical::Center)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|theme: &Theme| text::Style {
            color: Some(theme.colors.shell.ink),
        });
    let letters = Container::new(letters)
        .width(side)
        .height(side)
        .padding(iced::Padding {
            top: 2.0 * PERSON_TEXT_DROP * scale,
            ..iced::Padding::ZERO
        });
    stack![ring, letters].width(side).height(side).into()
}

/// The one or two initials shown for a person (spec section 7): from the
/// display name when it gives any letters, otherwise from the part of the
/// email before the first `@`. Empty when neither gives a letter.
pub fn initials(display_name: Option<&str>, email: Option<&str>) -> String {
    let from_name = display_name
        .map(|name| initials_of_words(name.trim().split(char::is_whitespace)))
        .unwrap_or_default();
    if !from_name.is_empty() {
        return from_name;
    }
    email
        .map(|email| {
            let local = email.split('@').next().unwrap_or_default();
            initials_of_words(local.split(['.', '_', '-', '+']))
        })
        .unwrap_or_default()
}

/// The uppercased first grapheme of each of the first two words that begin
/// with an alphabetic code point. Empty words are skipped.
fn initials_of_words<'s>(words: impl Iterator<Item = &'s str>) -> String {
    words
        .filter_map(|word| word.graphemes(true).next())
        .filter(|first| first.chars().next().is_some_and(char::is_alphabetic))
        .take(2)
        .map(uppercase_grapheme)
        .collect()
}

/// The cluster uppercased, unless that does not give a single cluster (for
/// example "ß" becomes "SS"), in which case the cluster as it was.
fn uppercase_grapheme(cluster: &str) -> String {
    let upper = cluster.to_uppercase();
    if upper.graphemes(true).count() == 1 {
        upper
    } else {
        cluster.to_owned()
    }
}

#[cfg(test)]
mod tests;
