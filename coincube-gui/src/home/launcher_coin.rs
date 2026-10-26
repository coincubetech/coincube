//! The Launcher network mark uses the supplied coin art in every presentation.
//! Embedding the bytes keeps animated SVG faces and static widgets independent
//! of a filesystem path at runtime.

use std::{fmt::Write, sync::OnceLock};

use base64::Engine;

use crate::chain::ChainId;

const BITCOIN: &[u8] = include_bytes!("../../assets/launcher/bitcoin-coin-orange.png");
const BLAKE2B: &[u8] = include_bytes!("../../assets/launcher/bitcoin-blake2b-coin-gold.png");

fn image_href(chain: ChainId) -> &'static str {
    static BITCOIN_HREF: OnceLock<String> = OnceLock::new();
    static BLAKE2B_HREF: OnceLock<String> = OnceLock::new();
    let (slot, bytes) = if chain.is_blake2b() {
        (&BLAKE2B_HREF, BLAKE2B)
    } else {
        (&BITCOIN_HREF, BITCOIN)
    };
    slot.get_or_init(|| {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    })
}

pub(super) fn is_test_network(chain: ChainId) -> bool {
    !matches!(chain, ChainId::Bitcoin | ChainId::BitcoinBlake2b)
}

/// A mark at the caller's own coordinates. `currentColor` takes the face or
/// widget ink supplied by its enclosing SVG. Test networks retain their name
/// label beside the mark in the native layouts.
pub(super) fn fragment(
    chain: ChainId,
    x: f32,
    y: f32,
    size: f32,
    test_border: f32,
    ring_padding: f32,
) -> String {
    let mut mark = String::new();
    let (inset, opacity) = if is_test_network(chain) {
        let _ = write!(
            mark,
            "<circle cx=\"{:.2}\" cy=\"{:.2}\" r=\"{:.2}\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"{test_border:.2}\" stroke-dasharray=\"2.5 2.5\"/>",
            x + size / 2.0,
            y + size / 2.0,
            (size - test_border) / 2.0
        );
        (test_border + ring_padding, " opacity=\"0.6\"")
    } else {
        (0.0, "")
    };
    let inner = size - inset * 2.0;
    let _ = write!(
        mark,
        "<image x=\"{:.2}\" y=\"{:.2}\" width=\"{inner:.2}\" height=\"{inner:.2}\" href=\"{}\"{opacity}/>",
        x + inset,
        y + inset,
        image_href(chain)
    );
    mark
}

pub(super) fn widget_source(chain: ChainId, size: f32, test_border: f32, light: bool) -> String {
    let ink = if light { "#30302d" } else { "#ecebe6" };
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {size:.2} {size:.2}\" color=\"{ink}\">{}</svg>",
        fragment(chain, 0.0, 0.0, size, test_border, size * 0.018)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supplied_coins_follow_chain_family_and_test_ring_rules() {
        for chain in ChainId::ALL {
            let svg = widget_source(chain, 16.0, 1.5, false);
            assert!(svg.contains("<image "));
            assert_eq!(
                svg.contains("stroke-dasharray=\"2.5 2.5\""),
                is_test_network(chain)
            );
            assert_eq!(svg.contains("opacity=\"0.6\""), is_test_network(chain));
            assert_eq!(
                image_href(chain),
                image_href(if chain.is_blake2b() {
                    ChainId::BitcoinBlake2b
                } else {
                    ChainId::Bitcoin
                })
            );
        }
        assert_ne!(
            image_href(ChainId::Bitcoin),
            image_href(ChainId::BitcoinBlake2b)
        );
        assert!(
            widget_source(ChainId::Testnet4, 16.0, 1.5, false).contains("stroke-width=\"1.50\"")
        );
        assert!(
            widget_source(ChainId::BitcoinBlake2bTestnet4, 18.0, 1.0, true)
                .contains("stroke-width=\"1.00\"")
        );
        // On a Cube face the padding is measured against the face, while
        // compact detail marks use their own fixed padding.
        let floor = fragment(ChainId::Testnet4, 64.0, 20.0, 52.0, 1.5, 126.0 * 0.018);
        assert!(floor.contains("<image x=\"67.77\" y=\"23.77\""));
        let details = fragment(ChainId::Testnet4, 28.0, 64.0, 18.0, 1.0, 1.5);
        assert!(details.contains("<image x=\"30.50\" y=\"66.50\""));
        assert!(BITCOIN.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(BLAKE2B.starts_with(b"\x89PNG\r\n\x1a\n"));
    }
}
