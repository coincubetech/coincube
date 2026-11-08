//! Optional decorative overrides for existing Tenshu illustration slots.

use std::sync::LazyLock;

use iced::widget::image::Handle;

use crate::app::settings::preferences::WorldviewMode;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Slot {
    Opening,
    EmptyHistory,
}

static SUITCOINER_OPENING: LazyLock<Handle> = LazyLock::new(|| {
    Handle::from_bytes(include_bytes!("../assets/worldview/suitcoiner-loading.png").as_slice())
});
static SUITCOINER_EMPTY: LazyLock<Handle> = LazyLock::new(|| {
    Handle::from_bytes(include_bytes!("../assets/worldview/suitcoiner-empty.png").as_slice())
});
static LUKE_OPENING: LazyLock<Handle> = LazyLock::new(|| {
    Handle::from_bytes(include_bytes!("../assets/worldview/luke-loading.png").as_slice())
});
static LUKE_EMPTY: LazyLock<Handle> = LazyLock::new(|| {
    Handle::from_bytes(include_bytes!("../assets/worldview/luke-empty.png").as_slice())
});

/// Retain the caller's incumbent Samurai handle when a slot has no override.
pub(crate) fn illustration(worldview: WorldviewMode, slot: Slot, samurai: &Handle) -> &Handle {
    let alternate = match (worldview, slot) {
        (WorldviewMode::Samurai, _) => None,
        (WorldviewMode::Suitcoiner, Slot::Opening) => Some(&SUITCOINER_OPENING),
        (WorldviewMode::Suitcoiner, Slot::EmptyHistory) => Some(&SUITCOINER_EMPTY),
        (WorldviewMode::Luke, Slot::Opening) => Some(&LUKE_OPENING),
        (WorldviewMode::Luke, Slot::EmptyHistory) => Some(&LUKE_EMPTY),
    };
    alternate.map_or(samurai, LazyLock::force)
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_ui::component::{amount::BitcoinDisplayUnit, quote_display};
    use coincube_ui::theme::Theme;

    use crate::app::view::shared::landing::capture;
    use crate::app::view::spark::{SparkTransactionsStatus, SparkTransactionsView};
    use crate::chain::ChainId;
    use crate::home::launcher_presenter::{opening_body, project_opening};

    #[test]
    fn illustration_handles_follow_the_mode_and_preserve_the_original() {
        let fallback = quote_display::image_handle_for_context("loading");
        let mut alternate_ids = std::collections::HashSet::new();
        for slot in [Slot::Opening, Slot::EmptyHistory] {
            assert!(std::ptr::eq(
                illustration(WorldviewMode::Samurai, slot, &fallback),
                &fallback,
            ));
            for mode in [WorldviewMode::Suitcoiner, WorldviewMode::Luke] {
                let selected = illustration(mode, slot, &fallback);
                assert_ne!(selected.id(), fallback.id());
                assert!(alternate_ids.insert(selected.id()));
                assert!(std::ptr::eq(selected, illustration(mode, slot, &fallback)));
            }
        }
    }

    #[test]
    fn worldview_art_fits_existing_opening_and_empty_history_views() {
        let loading = quote_display::image_handle_for_context("loading");
        let empty = quote_display::image_handle_for_context("empty-wallet");
        let loading_quote = quote_display::random_quote("loading");
        let empty_quote = quote_display::random_quote("empty-wallet");
        for (mode, name) in [
            (WorldviewMode::Samurai, "samurai"),
            (WorldviewMode::Suitcoiner, "suitcoiner"),
            (WorldviewMode::Luke, "luke"),
        ] {
            for (appearance, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
                let opened = capture::inspect_filled(
                    &format!("worldview-{}-opening", name),
                    opening_body(
                        project_opening("Everyday", ChainId::Bitcoin),
                        &crate::loader::Step::Connecting,
                        &loading_quote,
                        illustration(mode, Slot::Opening, &loading),
                        false,
                    ),
                    (1024, 768),
                    appearance,
                    theme,
                    (1024, 768),
                );
                assert!(opened.contains("Connecting to daemon"));
                let history = capture::inspect(
                    &format!("worldview-{}-empty-history", name),
                    SparkTransactionsView {
                        status: SparkTransactionsStatus::Loaded(vec![]),
                        recent_transactions: &[],
                        fiat_converter: None,
                        bitcoin_unit: BitcoinDisplayUnit::Sats,
                        show_direction_badges: false,
                        empty_state_quote: &empty_quote,
                        empty_state_image_handle: illustration(mode, Slot::EmptyHistory, &empty),
                        current_page: 0,
                        is_last_page: true,
                        processing: false,
                        mask_amounts: true,
                    }
                    .render(),
                    (1024, 768),
                    appearance,
                    theme,
                );
                assert!(history.contains("Your Spark wallet is ready"));
            }
        }
    }
}
