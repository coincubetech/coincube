//! The spec's fixture, loaded unchanged, plus the rules around it.
//!
//! `testdata/ring-mark-v1-vectors.json` is a byte-for-byte copy of
//! `plans/cube-avatars/ring-mark-v1-vectors.json` in the company brain.
//! Never edit it here: a change to the algorithm is a new spec version.

use serde::Deserialize;

use super::*;
use crate::theme::palette::ShellPalette;

const FIXTURE: &str = include_str!("testdata/ring-mark-v1-vectors.json");

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    spec: String,
    vectors: Vec<Vector>,
    initials: Vec<InitialsCase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Vector {
    cube_id: String,
    variation: u32,
    seed: String,
    fnv_state: String,
    draws: Vec<u32>,
    arc_count: usize,
    orange_index: usize,
    arcs: Vec<FixtureArc>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureArc {
    radius: f64,
    start: f64,
    sweep: f64,
    large_arc: bool,
    orange: bool,
    svg_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitialsCase {
    display_name: Option<String>,
    email: Option<String>,
    initials: String,
}

fn fixture() -> Fixture {
    serde_json::from_str(FIXTURE).expect("the ring mark fixture parses")
}

#[test]
fn fixture_is_version_1_with_every_case() {
    let fixture = fixture();
    assert_eq!(fixture.spec, "SPEC-cube-ring-mark-v1.md");
    assert_eq!(VERSION, 1);
    assert_eq!(fixture.vectors.len(), 8);
    assert_eq!(fixture.initials.len(), 21);
}

#[test]
fn every_mark_vector_matches() {
    for vector in fixture().vectors {
        let seed = seed(&vector.cube_id, vector.variation).expect("fixture ids are UUIDs");
        assert_eq!(seed, vector.seed, "seed for {}", vector.cube_id);

        let mut generator = Generator::new(&seed);
        assert_eq!(
            format!("{:#010x}", generator.state()),
            vector.fnv_state,
            "fnvState for {seed}"
        );
        let draws: Vec<u32> = (0..vector.draws.len())
            .map(|_| generator.next_draw())
            .collect();
        assert_eq!(draws, vector.draws, "draws for {seed}");

        let mark = RingMark::from_seed(&seed);
        assert_eq!(mark.arcs.len(), vector.arc_count, "arcCount for {seed}");
        assert_eq!(mark.orange, vector.orange_index, "orangeIndex for {seed}");
        assert_eq!(mark.arcs.len(), vector.arcs.len());
        for (i, (arc, expected)) in mark.arcs.iter().zip(&vector.arcs).enumerate() {
            assert_eq!(arc.radius, expected.radius, "radius {i} for {seed}");
            assert!(
                (arc.start - expected.start).abs() < 1e-6,
                "start {i} for {seed}: {} vs {}",
                arc.start,
                expected.start
            );
            assert!(
                (arc.sweep - expected.sweep).abs() < 1e-6,
                "sweep {i} for {seed}: {} vs {}",
                arc.sweep,
                expected.sweep
            );
            assert_eq!(
                arc.large_arc(),
                expected.large_arc,
                "largeArc {i} for {seed}"
            );
            assert_eq!(i == mark.orange, expected.orange, "orange {i} for {seed}");
            // The study's own path, to two decimals: a check on the drawing
            // geometry (centre, direction, flags), not only on the numbers.
            assert_eq!(arc.svg_path(), expected.svg_path, "svgPath {i} for {seed}");
        }
    }
}

#[test]
fn every_initials_case_matches() {
    for case in fixture().initials {
        assert_eq!(
            initials(case.display_name.as_deref(), case.email.as_deref()),
            case.initials,
            "initials for {:?} / {:?}",
            case.display_name,
            case.email
        );
    }
}

#[test]
fn variation_zero_is_the_plain_id() {
    let id = "9fb93534-081c-4253-b1e2-73a87984a8ab";
    let seed = seed(id, 0).unwrap();
    assert_eq!(seed, id);
    assert!(!seed.contains(':'));
    assert_eq!(super::seed(id, 3).unwrap(), format!("{id}:3"));
}

#[test]
fn an_uppercase_id_gives_the_lowercase_seed() {
    assert_eq!(
        seed("9FB93534-081C-4253-B1E2-73A87984A8AB", 1).unwrap(),
        "9fb93534-081c-4253-b1e2-73a87984a8ab:1"
    );
}

#[test]
fn a_malformed_id_gives_no_seed() {
    for id in [
        "",
        "not-a-uuid",
        // The study's shortened ids are not Cube ids.
        "8f1c2a7e-41d0",
        // Hyphen in the wrong place.
        "9fb93534081c-4253-b1e2-73a87984a8ab-",
        // A non-hex letter.
        "9fb93534-081c-4253-b1e2-73a87984a8ag",
        // Braced and with whitespace.
        "{9fb93534-081c-4253-b1e2-73a87984a8ab}",
        " 9fb93534-081c-4253-b1e2-73a87984a8ab",
        // A numeric server row id must never be a seed.
        "42",
        // Non-ASCII that lowercases to the right length.
        "9fb93534-081c-4253-b1e2-73a87984a8aé",
    ] {
        assert_eq!(seed(id, 0), None, "{id:?}");
    }
}

#[test]
fn initials_follow_the_spec_examples() {
    assert_eq!(initials(Some("Ama Adeyemi"), None), "AA");
    assert_eq!(initials(None, Some("kofi.mensah@example.com")), "KM");
    assert_eq!(initials(None, Some("zainab@example.com")), "Z");
    assert_eq!(initials(Some("(Sam) Rivera"), None), "R");
    assert_eq!(initials(Some("ßara Nunes"), None), "ßN");
    assert_eq!(initials(Some("👋"), Some("zara.obi@example.com")), "ZO");
    assert_eq!(initials(None, Some("123@example.com")), "");
    // An email with no `@` is all local part.
    assert_eq!(initials(None, Some("ada_obi")), "AO");
    // Decomposed "é" (e + combining acute) stays one cluster, not normalised.
    assert_eq!(initials(Some("e\u{301}mile"), None), "E\u{301}");
}

fn shell_svgs(id: &str) -> (String, String) {
    let mark = RingMark::from_seed(&seed(id, 0).unwrap());
    let dark = ShellPalette::dark();
    let light = ShellPalette::light();
    (
        svg(Some(&mark), dark.ink, dark.accent),
        svg(Some(&mark), light.ink, light.accent),
    )
}

/// The SVG for the five base vectors, in both themes. Set
/// `RING_MARK_UPDATE_SNAPSHOTS=1` to rewrite the snapshot files, then compare
/// the result by eye with the fixture's `svgPath` before committing.
#[test]
fn base_vectors_match_their_svg_snapshots() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/component/ring_mark/testdata/snapshots");
    let update = std::env::var_os("RING_MARK_UPDATE_SNAPSHOTS").is_some();
    for vector in fixture().vectors.iter().filter(|v| v.variation == 0) {
        let (dark, light) = shell_svgs(&vector.cube_id);
        for (theme, svg) in [("dark", dark), ("light", light)] {
            let path = dir.join(format!("{}-{theme}.svg", vector.cube_id));
            if update {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(&path, format!("{svg}\n")).unwrap();
            }
            let expected = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("snapshot {}: {e}", path.display()));
            assert_eq!(svg, expected.trim_end(), "snapshot {}", path.display());
        }
    }
}

#[test]
fn svg_uses_the_theme_colours_and_one_orange_arc() {
    let (dark, light) = shell_svgs("3994693e-a3ae-4e68-9323-451b4a95902a");
    assert_eq!(dark.matches("stroke=\"#f3821d\"").count(), 1);
    assert_eq!(dark.matches("stroke=\"#f5f0e8\"").count(), 1);
    assert!(dark.contains(r##"<circle cx="32" cy="32" r="2.6" fill="#f5f0e8"/>"##));
    assert_eq!(light.matches("stroke=\"#f3821d\"").count(), 1);
    assert!(light.contains("fill=\"#202020\""));
    // No background and no enclosing ring: that is what a person has.
    assert!(!dark.contains("<rect"));
    assert_eq!(dark.matches("<circle").count(), 1);
}

#[test]
fn no_mark_draws_the_dot_alone() {
    let palette = ShellPalette::dark();
    let svg = svg(None, palette.ink, palette.accent);
    assert!(!svg.contains("<path"));
    assert_eq!(svg.matches("<circle").count(), 1);
}

#[test]
fn layers_split_the_orange_arc_from_the_ink() {
    let seed = seed("1aa615a9-4b6a-4551-a061-ae14a1dbf721", 0).unwrap();
    let mark = RingMark::from_seed(&seed);
    let layers = build_layers(Some(&seed));
    let text_of = |handle: &iced_svg::Handle| match handle.data() {
        iced::advanced::svg::Data::Bytes(bytes) => String::from_utf8(bytes.to_vec()).unwrap(),
        iced::advanced::svg::Data::Path(_) => unreachable!("layers are built from memory"),
    };
    let ink = text_of(&layers.ink);
    let accent = text_of(layers.accent.as_ref().expect("a mark has an orange arc"));
    // Three arcs: two in the ink layer with the dot, one in the accent layer.
    assert_eq!(ink.matches("<path").count(), 2);
    assert_eq!(ink.matches("<circle").count(), 1);
    assert_eq!(accent.matches("<path").count(), 1);
    assert!(accent.contains(&mark.arcs[mark.orange].svg_path()));
    assert!(!accent.contains("<circle"));
    // Rebuilding gives the same handle ids, so the renderer's cache hits.
    let again = build_layers(Some(&seed));
    assert_eq!(layers.ink.id(), again.ink.id());
    assert_eq!(
        layers.accent.as_ref().map(iced_svg::Handle::id),
        again.accent.as_ref().map(iced_svg::Handle::id)
    );
    assert_eq!(mark.arcs.len(), 3);
    assert!(build_layers(None).accent.is_none());
}

#[test]
fn widgets_build_at_their_sizes() {
    let seed = seed("a7213dfe-8932-4293-a0dd-5386c9cf0800", 2);
    let _: Element<'_, ()> = ring_mark(seed.as_deref(), 36.0);
    let _: Element<'_, ()> = ring_mark_muted(seed.as_deref(), 44.0);
    let _: Element<'_, ()> = ring_mark(None, MIN_SIZE);
    let _: Element<'_, ()> = person_mark(Some("Ama Adeyemi"), None, 36.0);
    let _: Element<'_, ()> = person_mark(None, None, 64.0);
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "at least 24px")]
fn a_mark_below_the_minimum_asserts_in_debug() {
    let _: Element<'_, ()> = ring_mark(None, 20.0);
}

#[test]
fn person_ring_is_thin_and_translucent() {
    let svg = person_ring_svg();
    assert!(svg.contains(r#"r="29""#));
    assert!(svg.contains(r#"stroke-width="2.4""#));
    assert!(svg.contains(r#"stroke-opacity="0.28""#));
    assert!(svg.contains(r#"fill="none""#));
}

/// Draws `element` on the software renderer and returns its RGBA pixels.
fn render(element: Element<'_, ()>, theme: &Theme, side: u32) -> Vec<u8> {
    use iced::advanced::renderer::{self, Headless};
    use iced::advanced::widget::Tree;
    use iced::advanced::{layout, mouse, Layout};
    use iced::{Rectangle, Size};

    for bytes in font::load() {
        iced::advanced::graphics::text::font_system()
            .write()
            .unwrap()
            .load_font(bytes);
    }
    let mut element = element;
    let mut renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
        font::REGULAR,
        iced::Pixels(16.0),
        Some("tiny-skia"),
    ))
    .expect("software renderer");
    let size = Size::new(side as f32, side as f32);
    let mut tree = Tree::new(element.as_widget());
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, size),
    );
    element.as_widget().draw(
        &tree,
        &mut renderer,
        theme,
        &renderer::Style {
            text_color: theme.colors.shell.ink,
        },
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &Rectangle::with_size(size),
    );
    renderer.screenshot(Size::new(side, side), 1.0, theme.colors.shell.background)
}

/// Pixels close to `color`, ignoring anti-aliased edges.
fn count_near(pixels: &[u8], color: Color) -> usize {
    let [r, g, b, _] = color.into_rgba8();
    pixels
        .chunks_exact(4)
        .filter(|p| p[0].abs_diff(r) < 24 && p[1].abs_diff(g) < 24 && p[2].abs_diff(b) < 24)
        .count()
}

/// The base vectors at 56 px, the spec's golden size, in both themes: the
/// accent layer and the ink layer each paint in their own theme colour, and
/// the muted variant paints no accent. Set `RING_MARK_PREVIEW_DIR` to save
/// the renders as PNGs for a by-eye check against the fixture's `svgPath`.
#[test]
fn marks_render_in_the_theme_colours() {
    const SIDE: u32 = 56;
    let preview = std::env::var_os("RING_MARK_PREVIEW_DIR");
    for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        let shell = theme.colors.shell;
        for vector in fixture().vectors.iter().filter(|v| v.variation == 0) {
            let seed = seed(&vector.cube_id, 0);
            let pixels = render(ring_mark(seed.as_deref(), SIDE as f32), &theme, SIDE);
            assert!(
                count_near(&pixels, shell.accent) > 20,
                "{name} {}: accent",
                vector.cube_id
            );
            assert!(
                count_near(&pixels, shell.ink) > 20,
                "{name} {}: ink",
                vector.cube_id
            );
            if let Some(dir) = &preview {
                image::save_buffer(
                    std::path::Path::new(dir).join(format!("{}-{name}.png", vector.cube_id)),
                    &pixels,
                    SIDE,
                    SIDE,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }

            let muted = render(ring_mark_muted(seed.as_deref(), SIDE as f32), &theme, SIDE);
            assert_eq!(
                count_near(&muted, shell.accent),
                0,
                "{name}: muted has no accent"
            );
            assert!(
                count_near(&muted, shell.muted) > 20,
                "{name}: muted paints muted"
            );
        }

        let dot = render(ring_mark(None, SIDE as f32), &theme, SIDE);
        assert_eq!(
            count_near(&dot, shell.accent),
            0,
            "{name}: the fallback has no arc"
        );
        let dot_pixels = count_near(&dot, shell.ink);
        assert!(
            dot_pixels > 0 && dot_pixels < 40,
            "{name}: the fallback is the dot alone"
        );

        let person = render(person_mark(Some("Ama Adeyemi"), None, 64.0), &theme, 64);
        assert_eq!(
            count_near(&person, shell.accent),
            0,
            "{name}: a person has no accent"
        );
        assert!(
            count_near(&person, shell.ink) > 20,
            "{name}: initials in ink"
        );
        if let Some(dir) = &preview {
            image::save_buffer(
                std::path::Path::new(dir).join(format!("person-{name}.png")),
                &person,
                64,
                64,
                image::ColorType::Rgba8,
            )
            .unwrap();
        }
    }
}

#[test]
fn only_a_real_seed_draws_a_mark() {
    let id = "9fb93534-081c-4253-b1e2-73a87984a8ab";
    for seed in [
        id.to_owned(),
        format!("{id}:1"),
        format!("{id}:17"),
        format!("{id}:2147483647"),
    ] {
        assert!(is_seed(&seed), "{seed}");
    }
    for bad in [
        String::new(),
        "42".to_owned(),
        id.to_uppercase(),
        format!("{id}:0"),
        format!("{id}:01"),
        format!("{id}:"),
        format!("{id}:-1"),
        format!("{id}:x"),
        format!("{id}:1:2"),
        format!("{id}:99999999999"),
    ] {
        assert!(!is_seed(&bad), "{bad:?}");
    }
    // Every seed `seed()` makes passes.
    for vector in fixture().vectors {
        assert!(is_seed(&seed(&vector.cube_id, vector.variation).unwrap()));
    }
}

/// A bad seed and no seed share the dot, whichever is drawn first: the cache
/// never hands one a mark made from the other.
#[test]
fn a_bad_seed_draws_the_dot_and_never_poisons_the_cache() {
    let _: Element<'_, ()> = ring_mark(Some(""), 36.0);
    assert!(layers(None).accent.is_none());
    assert!(layers(Some("")).accent.is_none());
    assert!(layers(Some("9FB93534-081C-4253-B1E2-73A87984A8AB"))
        .accent
        .is_none());
    assert!(layers(Some("9fb93534-081c-4253-b1e2-73a87984a8ab"))
        .accent
        .is_some());
}
