//! Tools › Split coins: a checklist of what is still on both chains, this
//! chain's split key, and the Bitcoin Blake2b copy (Tenshu canvas v157,
//! `P-Advanced` split section). Splitting itself happens in Coin control's
//! Split coins overlay; every action here leads there or to the copy wizard.

use std::sync::Arc;

use coincube_core::miniscript::bitcoin::Amount;
use coincube_ui::{
    component::{amount::format_u64_as_string, button, text::*},
    theme,
    widget::{Column, Container, Element, Row},
};
use iced::{Alignment, Length, Task};

use crate::{
    app::{
        cache::Cache,
        menu::Menu,
        message::Message,
        state::State,
        view::{self, Message as ViewMessage},
        wallet::Wallet,
    },
    chain::ChainId,
    daemon::Daemon,
    services::split_coins,
};

/// One checklist line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubItem {
    pub title: String,
    pub detail: String,
    pub state: ItemState,
    /// The action button, if any: its label and what it sends.
    pub action: Option<(&'static str, HubAction)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    Done,
    ToDo,
    Optional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubAction {
    /// Open Coin control's Split coins overlay.
    Split,
    /// Start the Bitcoin Blake2b copy wizard.
    CreateCopy,
}

fn chain_name(chain: ChainId) -> &'static str {
    if chain.is_blake2b() {
        "Bitcoin Blake2b"
    } else {
        "Bitcoin"
    }
}

fn sats(amount: Amount) -> String {
    format!("{} sats", format_u64_as_string(amount.to_sat(), ","))
}

/// The facts the checklist is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubFacts {
    pub here: ChainId,
    pub both_chains: usize,
    pub both_total: Amount,
    pub splitting: usize,
    pub key: Option<Amount>,
    pub signature_available: bool,
    pub copy_exists: bool,
}

pub fn items(facts: HubFacts) -> Vec<HubItem> {
    let here = facts.here;
    let coins = HubItem {
        title: "Coins on both chains".into(),
        detail: if facts.both_chains > 0 {
            format!(
                "{} · {}. Split them from either chain; one side is enough.",
                if facts.both_chains == 1 {
                    "1 coin".to_string()
                } else {
                    format!("{} coins", facts.both_chains)
                },
                sats(facts.both_total)
            )
        } else if facts.splitting > 0 {
            "Splitting. The coin rows show the confirmations.".into()
        } else {
            "None. Every coin is split.".into()
        },
        state: if facts.both_chains > 0 {
            ItemState::ToDo
        } else {
            ItemState::Done
        },
        action: (facts.both_chains > 0).then_some(("Split coins", HubAction::Split)),
    };
    let signature = here.is_blake2b() && facts.signature_available;
    let key = HubItem {
        title: format!("Split key on {}", chain_name(here)),
        detail: match facts.key {
            Some(amount) => format!(
                "{} · found only on {}. It comes back with every split, so one is enough.",
                sats(amount),
                chain_name(here)
            ),
            None if signature => {
                "Not needed: this Vault can split with a replay-protected signature.".into()
            }
            None => format!(
                "Needed to split from {}. The Split coins window shows where to send one.",
                chain_name(here)
            ),
        },
        state: if facts.key.is_some() || signature {
            ItemState::Done
        } else {
            ItemState::ToDo
        },
        action: (facts.key.is_none() && !signature)
            .then_some(("Get a split key", HubAction::Split)),
    };
    if here.is_blake2b() {
        vec![
            coins,
            HubItem {
                title: "Replay-protected signature".into(),
                detail: if facts.signature_available {
                    "Available: this Vault can sign it. No split key needed.".into()
                } else {
                    "Not available: this Vault has a hardware signer that can't make this signature. Use a split key.".into()
                },
                state: if facts.signature_available {
                    ItemState::Done
                } else {
                    ItemState::Optional
                },
                action: None,
            },
            key,
            HubItem {
                title: "Or split from Bitcoin".into(),
                detail: "A split in the Bitcoin Cube separates these coins here too.".into(),
                state: ItemState::Optional,
                action: None,
            },
        ]
    } else {
        vec![
            coins,
            key,
            HubItem {
                title: "Bitcoin Blake2b copy".into(),
                detail: if facts.copy_exists {
                    "Created. Open it from the Cube switcher.".into()
                } else {
                    "Optional. Needed to spend your Bitcoin Blake2b coins, or to split them from that side.".into()
                },
                state: if facts.copy_exists {
                    ItemState::Done
                } else {
                    ItemState::Optional
                },
                action: (!facts.copy_exists).then_some(("Create copy", HubAction::CreateCopy)),
            },
        ]
    }
}

pub fn hub_view<'a>(here: ChainId, items: Vec<HubItem>) -> Element<'a, ViewMessage> {
    let intro = format!(
        "Coins you held before the fork exist on both Bitcoin and Bitcoin Blake2b. Until one of them is spent in a way the other chain rejects, a transaction on one chain can be copied to the other and move both. Splitting from either chain is enough; you can also do both. This Cube is on {}.",
        chain_name(here)
    );
    let list = items
        .into_iter()
        .fold(Column::new().spacing(12), |list, item| {
            let mark = match item.state {
                ItemState::Done => p1_bold("✓").style(theme::text::success),
                ItemState::ToDo => p1_bold("•").style(theme::text::warning),
                ItemState::Optional => p1_bold("○").style(theme::text::secondary),
            };
            let action = item.action.map(|(label, action)| {
                button::secondary(None, label).on_press(match action {
                    HubAction::Split => ViewMessage::SplitFirst,
                    HubAction::CreateCopy => ViewMessage::StartClaimBlake2b,
                })
            });
            list.push(
                Container::new(
                    Row::new()
                        .spacing(14)
                        .align_y(Alignment::Center)
                        .push(Container::new(mark).width(Length::Fixed(20.0)))
                        .push(
                            Column::new()
                                .spacing(4)
                                .width(Length::Fill)
                                .push(p1_bold(item.title))
                                .push(p2_regular(item.detail).style(theme::text::secondary)),
                        )
                        .push(action),
                )
                .padding(16)
                .style(theme::card::simple)
                .width(Length::Fill),
            )
        });
    Column::new()
        .spacing(20)
        .push(h3("Split coins").bold())
        .push(p1_regular(intro).style(theme::text::secondary))
        .push(list)
        .into()
}

/// Tools › Split coins. Stateless: everything comes from the cache.
pub struct SplitHubPanel {
    wallet: Arc<Wallet>,
}

impl SplitHubPanel {
    pub fn new(wallet: Arc<Wallet>) -> Self {
        Self { wallet }
    }

    pub fn facts(&self, cache: &Cache) -> HubFacts {
        let both: Vec<_> = cache
            .coins()
            .iter()
            .filter(|coin| coin.spend_info.is_none() && coin.block_height.is_some())
            .filter(|coin| cache.chain_status(&coin.outpoint).needs_split())
            .collect();
        HubFacts {
            here: self.wallet.chain,
            both_chains: both.len(),
            both_total: both.iter().map(|coin| coin.amount).sum(),
            splitting: cache.splitting.len(),
            key: cache.split_keys().first().map(|(coin, _)| coin.amount),
            signature_available: split_coins::signature_available(&self.wallet),
            copy_exists: cache.btcb2_already_claimed,
        }
    }
}

impl State for SplitHubPanel {
    fn view<'a>(&'a self, menu: &'a Menu, cache: &'a Cache) -> Element<'a, ViewMessage> {
        let facts = self.facts(cache);
        view::dashboard(menu, cache, hub_view(facts.here, items(facts)))
    }

    fn update(
        &mut self,
        _daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        _cache: &Cache,
        _message: Message,
    ) -> Task<Message> {
        Task::none()
    }
}

impl From<SplitHubPanel> for Box<dyn State> {
    fn from(s: SplitHubPanel) -> Box<dyn State> {
        Box::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> HubFacts {
        HubFacts {
            here: ChainId::Bitcoin,
            both_chains: 3,
            both_total: Amount::from_sat(1_200_000),
            splitting: 0,
            key: None,
            signature_available: false,
            copy_exists: false,
        }
    }

    #[test]
    fn bitcoin_hub_lists_coins_key_and_optional_copy() {
        let items = items(facts());
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].action, Some(("Split coins", HubAction::Split)));
        assert_eq!(items[0].state, ItemState::ToDo);
        assert_eq!(items[1].action, Some(("Get a split key", HubAction::Split)));
        assert_eq!(items[2].title, "Bitcoin Blake2b copy");
        assert_eq!(
            items[2].state,
            ItemState::Optional,
            "the copy is never required on Bitcoin"
        );
        assert_eq!(
            items[2].action,
            Some(("Create copy", HubAction::CreateCopy))
        );
    }

    #[test]
    fn blake2b_hub_offers_the_signature_instead_of_a_key() {
        let items = items(HubFacts {
            here: ChainId::BitcoinBlake2b,
            signature_available: true,
            ..facts()
        });
        assert_eq!(items.len(), 4);
        assert_eq!(items[1].title, "Replay-protected signature");
        assert_eq!(items[1].state, ItemState::Done);
        assert_eq!(items[2].state, ItemState::Done, "no key needed");
        assert_eq!(items[2].action, None);
        assert_eq!(items[3].title, "Or split from Bitcoin");
    }

    #[test]
    fn nothing_left_reads_done() {
        let items = items(HubFacts {
            both_chains: 0,
            both_total: Amount::ZERO,
            key: Some(Amount::from_sat(30_000)),
            copy_exists: true,
            ..facts()
        });
        assert!(items.iter().all(|item| item.state == ItemState::Done));
        assert!(items.iter().all(|item| item.action.is_none()));
    }

    #[test]
    fn hub_mounts_its_checklist() {
        use crate::app::view::shared::landing::capture;
        let mounted = capture::inspect(
            "vault-split-coins-hub",
            hub_view(ChainId::Bitcoin, items(facts())),
            (1024, 768),
            "dark",
            coincube_ui::theme::Theme::dark(),
        );
        assert!(mounted.contains("Split coins"));
        assert!(mounted.contains("Coins on both chains"));
        assert!(mounted.contains("3 coins · 1,200,000 sats"));
        assert!(mounted.contains("Split key on Bitcoin"));
        assert!(mounted.contains("Create copy"));
    }
}
