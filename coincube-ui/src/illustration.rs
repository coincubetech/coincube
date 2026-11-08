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
        static HANDLE: LazyLock<Handle> = LazyLock::new(|| {
            Handle::from_bytes(
                include_bytes!(concat!("../static/images/worldview/", $name, ".png")).as_slice(),
            )
        });
        LazyLock::force(&HANDLE)
    }};
}

fn alternate(mode: WorldviewMode, slot: Slot) -> Option<&'static Handle> {
    use Slot::*;
    use WorldviewMode::*;

    Some(match (mode, slot) {
        (Samurai, _) => return None,
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
    })
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
    fn each_worldview_slot_has_its_own_cached_alternate() {
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
                assert!(ids.insert(handle.id()));
                assert!(std::ptr::eq(handle, alternate(mode, slot).unwrap()));
            }
        }
    }
}
