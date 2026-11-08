//! Decorative artwork selected at draw time from the active Tenshu theme.

use std::sync::LazyLock;

use iced::advanced::{layout, renderer, widget::Tree, Layout, Widget};
use iced::widget::image::{Handle, Image};
use iced::{mouse, ContentFit, Length, Rectangle, Size};

use crate::theme::{Theme, WorldviewMode};
use crate::widget::Element;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Opening,
    EmptyHistory,
    Sent,
    Received,
    Restored,
    KitProtection,
    KitRemoval,
    Pin,
}

macro_rules! embedded {
    ($name:literal) => {{
        static HANDLE: LazyLock<Option<Handle>> = LazyLock::new(|| {
            decode_png(include_bytes!(concat!(
                "../static/images/worldview/",
                $name,
                ".png"
            )))
        });
        HANDLE.as_ref()
    }};
}

// WGPU can measure decoded handles on first use. Encoded PNG handles enter
// its asynchronous load queue and can briefly fall back to the Samurai image.
fn decode_png(bytes: &[u8]) -> Option<Handle> {
    let rgba = ::image::load_from_memory_with_format(bytes, ::image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    Some(Handle::from_rgba(
        rgba.width(),
        rgba.height(),
        rgba.into_raw(),
    ))
}

fn alternate(mode: WorldviewMode, slot: Slot) -> Option<&'static Handle> {
    use Slot::*;
    use WorldviewMode::*;

    match (mode, slot) {
        (Samurai, _) => None,
        (Suitcoiner, Opening) => embedded!("suitcoiner-loading"),
        (Suitcoiner, EmptyHistory) => embedded!("suitcoiner-empty"),
        (Suitcoiner, Sent) => embedded!("suitcoiner-sent"),
        (Suitcoiner, Received) => embedded!("suitcoiner-received"),
        (Suitcoiner, Restored) => embedded!("suitcoiner-restored"),
        (Suitcoiner, KitProtection) => embedded!("suitcoiner-protected"),
        (Suitcoiner, KitRemoval) => embedded!("suitcoiner-withdrawn"),
        (Suitcoiner, Pin) => embedded!("suitcoiner-pin"),
        (Luke, Opening) => embedded!("luke-loading"),
        (Luke, EmptyHistory) => embedded!("luke-empty"),
        (Luke, Sent) => embedded!("luke-sent"),
        (Luke, Received) => embedded!("luke-received"),
        (Luke, Restored) => embedded!("luke-restored"),
        (Luke, KitProtection) => embedded!("luke-protected"),
        (Luke, KitRemoval) => embedded!("luke-withdrawn"),
        (Luke, Pin) => embedded!("luke-pin"),
    }
}

/// Use the same bounds as the incumbent image in every worldview. The
/// alternate handle affects only the pixels inside that existing slot.
pub fn image<'a, M: 'a>(
    slot: Option<Slot>,
    incumbent: Handle,
    width: Length,
    height: Length,
    fit: ContentFit,
) -> Element<'a, M> {
    Element::new(Illustration {
        slot,
        incumbent,
        width,
        height,
        fit,
    })
}

struct Illustration {
    slot: Option<Slot>,
    incumbent: Handle,
    width: Length,
    height: Length,
    fit: ContentFit,
}

impl Illustration {
    fn image(&self, handle: &Handle) -> Image {
        Image::new(handle.clone())
            .width(self.width)
            .height(self.height)
            .content_fit(self.fit)
    }
}

impl<M> Widget<M, Theme, iced::Renderer> for Illustration {
    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        <Image as Widget<M, Theme, iced::Renderer>>::layout(
            &mut self.image(&self.incumbent),
            tree,
            renderer,
            limits,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced::advanced::image::Renderer as _;

        let handle = self
            .slot
            .and_then(|slot| alternate(theme.worldview(), slot))
            .filter(|handle| renderer.measure_image(handle).is_some())
            .unwrap_or(&self.incumbent);
        <Image as Widget<M, Theme, iced::Renderer>>::draw(
            &self.image(handle),
            tree,
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mounted_illustration_draws_the_selected_worldview() {
        use iced::advanced::renderer::Headless;

        let incumbent = Handle::from_rgba(1, 1, vec![255, 255, 255, 255]);
        let mut element = image::<()>(
            Some(Slot::Opening),
            incumbent,
            Length::Fixed(64.0),
            Length::Fixed(64.0),
            ContentFit::Contain,
        );
        let mut tree = Tree::new(element.as_widget());
        let mut renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            crate::font::REGULAR,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("software renderer");
        let size = Size::new(64.0, 64.0);
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let viewport = Rectangle::with_size(size);
        let mut captures = std::collections::HashSet::new();
        for mode in [
            WorldviewMode::Samurai,
            WorldviewMode::Suitcoiner,
            WorldviewMode::Luke,
        ] {
            let theme = Theme::dark().with_worldview(mode);
            element.as_widget().draw(
                &tree,
                &mut renderer,
                &theme,
                &renderer::Style {
                    text_color: iced::Color::WHITE,
                },
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &viewport,
            );
            let pixels = renderer.screenshot(Size::new(64, 64), 1.0, iced::Color::BLACK);
            assert!(pixels.chunks_exact(4).any(|pixel| pixel[..3] != [0, 0, 0]));
            assert!(
                captures.insert(pixels),
                "{mode:?} should draw distinct pixels"
            );
            iced::advanced::Renderer::reset(&mut renderer, viewport);
        }
    }

    #[test]
    fn corrupt_artwork_has_no_alternate() {
        assert!(decode_png(b"invalid PNG").is_none());
    }

    #[test]
    fn each_worldview_slot_has_its_own_decoded_alternate() {
        let mut ids = std::collections::HashSet::new();
        for slot in [
            Slot::Opening,
            Slot::EmptyHistory,
            Slot::Sent,
            Slot::Received,
            Slot::Restored,
            Slot::KitProtection,
            Slot::KitRemoval,
            Slot::Pin,
        ] {
            assert!(alternate(WorldviewMode::Samurai, slot).is_none());
            for mode in [WorldviewMode::Suitcoiner, WorldviewMode::Luke] {
                let handle = alternate(mode, slot).unwrap();
                let Handle::Rgba {
                    width,
                    height,
                    pixels,
                    ..
                } = handle
                else {
                    panic!("Bundled {mode:?} {slot:?} must be measurable on first draw");
                };
                assert!(*width > 0 && *height > 0);
                assert_eq!(pixels.len(), (*width * *height * 4) as usize);
                assert!(ids.insert(handle.id()));
                assert!(std::ptr::eq(handle, alternate(mode, slot).unwrap()));
            }
        }
    }
}
