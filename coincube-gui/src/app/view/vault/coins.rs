use std::collections::HashMap;

use iced::{widget::Space, Alignment, Length};

use coincube_ui::component::amount::BitcoinDisplayUnit;
use coincube_ui::{
    component::{amount::*, badge, button, form, text::*},
    icon::{self, coins_icon},
    theme,
    widget::{Button, Column, Container, Element, Row, RowExt},
};

use crate::{
    app::{
        cache::Cache,
        menu::Menu,
        view::{message::Message, placeholder, vault::label},
    },
    daemon::model::{remaining_sequence, Coin},
    services::entangled::Entanglement,
};

/// Presence is not ancestry proof: a missing transaction may still replay.
pub fn entangled_badge<'a, T: 'a>(status: Entanglement) -> Option<Container<'a, T>> {
    match status {
        Entanglement::Entangled => Some(badge::badge_pill(
            "  Before fork · both chains  ",
            "This deposit transaction has been seen on both Bitcoin and BTCB2. It may be replayable. Use the split wizard to verify and separate shared coins.",
        )),
        Entanglement::Unknown => Some(badge::badge_pill(
            "  Replay status not checked  ",
            "The other-chain lookup has not completed. It retries after sync. This coin has not been verified for replay protection.",
        )),
        Entanglement::NotEntangled => Some(badge::badge_pill(
            "  Replay protection unverified  ",
            "This transaction was not found on the other chain. That alone does not prove it cannot replay. The split wizard checks its ancestry before using it as a chain-specific deposit.",
        )),
    }
}

/// A saved coin's block height proves it was confirmed, even while the
/// daemon's chain tip has not been loaded (or is briefly behind the coin).
fn confirmation_status(block_height: Option<i32>, tip: u32) -> String {
    match block_height {
        None => "Unconfirmed".to_string(),
        Some(height) if height >= 0 && tip > 0 && (height as u32) <= tip => {
            let confirmations = tip - height as u32 + 1;
            format!(
                "{} confirmation{}",
                confirmations,
                if confirmations == 1 { "" } else { "s" }
            )
        }
        Some(_) => "Confirmed · confirmations unavailable".to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn coins_view<'a>(
    cache: &Cache,
    coins: &'a [Coin],
    timelock: Option<u16>,
    selected: &[usize],
    labels: &'a HashMap<String, String>,
    labels_editing: &'a HashMap<String, form::Value<String>>,
    bitcoin_unit: BitcoinDisplayUnit,
    blake2b: bool,
) -> Element<'a, Message> {
    Column::new()
        .push(Container::new(h3("Coin control").bold()).width(Length::Fill))
        .push(p1_regular("Inspect coins, labels, confirmations and recovery timing before choosing inputs for a spend.")
            .style(theme::text::secondary))
        .push(coins.is_empty().then(|| {
            placeholder(
                coins_icon().size(80),
                "No coins yet",
                "Coins will appear here after this Vault receives bitcoin.",
            )
        }))
        .push(
            Column::new()
                .spacing(10)
                .push(coins.iter().enumerate().fold(
                    Column::new().spacing(10),
                    |col, (i, coin)| {
                        col.push(coin_list_view(
                            coin,
                            timelock,
                            cache.blockheight().max(0) as u32,
                            i,
                            selected.contains(&i),
                            labels,
                            labels_editing,
                            bitcoin_unit,
                            cache.balance_amounts_masked(),
                            blake2b.then(|| cache.entanglement_of(&coin.outpoint.txid)),
                            cache.split_deposit.is_some_and(|(outpoint, expires)| {
                                outpoint == coin.outpoint && std::time::Instant::now() < expires
                            }),
                        ))
                    },
                )),
        )
        .align_x(Alignment::Center)
        .spacing(35)
        .into()
}

#[allow(clippy::collapsible_else_if)]
#[allow(clippy::too_many_arguments)]
fn coin_list_view<'a>(
    coin: &'a Coin,
    timelock: Option<u16>,
    blockheight: u32,
    index: usize,
    collapsed: bool,
    labels: &'a HashMap<String, String>,
    labels_editing: &'a HashMap<String, form::Value<String>>,
    bitcoin_unit: BitcoinDisplayUnit,
    mask_amounts: bool,
    entangled: Option<Entanglement>,
    verified_deposit: bool,
) -> Container<'a, Message> {
    let outpoint = coin.outpoint.to_string();
    let address = coin.address.to_string();
    let txid = coin.outpoint.txid.to_string();
    let amount: Element<'a, Message> = if mask_amounts {
        text("••••••").into()
    } else {
        amount_with_unit(&coin.amount, bitcoin_unit).into()
    };
    Container::new(
        Column::new()
            .push(
                Button::new(
                    Row::new()
                        .push(
                            Row::new()
                                .push(badge::bitcoin_logo())
                                .push(if !collapsed {
                                    if let Some(label) = labels.get(&outpoint) {
                                        if !label.is_empty() {
                                            Container::new(p1_regular(label)).width(Length::Fill)
                                        } else if let Some(label) = labels.get(&txid) {
                                            Container::new(
                                                Row::new()
                                                    .spacing(5)
                                                    .push(
                                                        // It is not possible to know if a coin is a
                                                        // change coin or not so for now, From is
                                                        // enough
                                                        p1_regular("From")
                                                            .style(theme::text::secondary),
                                                    )
                                                    .push(p1_regular(label)),
                                            )
                                            .width(Length::Fill)
                                        } else {
                                            Container::new(Space::new().width(Length::Fill))
                                                .width(Length::Fill)
                                        }
                                    } else if let Some(label) = labels.get(&txid) {
                                        Container::new(
                                            Row::new()
                                                .spacing(5)
                                                .push(
                                                    // It is not possible to know if a coin is a
                                                    // change coin or not so for now, From is
                                                    // enough
                                                    p1_regular("From")
                                                        .style(theme::text::secondary),
                                                )
                                                .push(p1_regular(label)),
                                        )
                                        .width(Length::Fill)
                                    } else {
                                        Container::new(Space::new().width(Length::Fill))
                                            .width(Length::Fill)
                                    }
                                } else {
                                    Container::new(Space::new().width(Length::Fill))
                                        .width(Length::Fill)
                                })
                                .push_maybe(if coin.spend_info.is_some() {
                                    Some(badge::spent())
                                } else if coin.block_height.is_none() {
                                    Some(badge::unconfirmed())
                                } else {
                                    timelock.map(|timelock| {
                                        coin_sequence_label(
                                            remaining_sequence(coin, blockheight, timelock),
                                            timelock as u32,
                                        )
                                    })
                                })
                                .push_maybe(if verified_deposit {
                                    Some(badge::badge_pill("  Replay-protected split deposit  ",
                                        "The open split wizard verified this exact coin's chain-specific ancestry. It will be combined with the shared coins in a self-transfer. The app rechecks it before signing."))
                                } else {
                                    entangled.and_then(entangled_badge)
                                })
                                .spacing(10)
                                .align_y(Alignment::Center)
                                .width(Length::Fill),
                        )
                        .push(
                            Column::new()
                                .align_x(Alignment::End)
                                .spacing(3)
                                .push(amount)
                                .push(p2_regular(confirmation_status(coin.block_height, blockheight))
                                    .style(theme::text::secondary)),
                        )
                        .push(p2_regular("Details").style(theme::text::secondary))
                        .align_y(Alignment::Center)
                        .spacing(20),
                )
                .style(theme::button::transparent_border)
                .padding(10)
                .on_press(Message::Select(index)),
            )
            .push(if collapsed {
                Some(
                    Column::new()
                        .padding(10)
                        .spacing(5)
                        .push(
                            Container::new(if let Some(label) = labels_editing.get(&outpoint) {
                                label::label_editing(vec![outpoint.clone()], label, P1_SIZE)
                            } else {
                                label::label_editable(
                                    vec![outpoint.clone()],
                                    labels.get(&outpoint),
                                    P1_SIZE,
                                )
                            })
                            .width(Length::Fill),
                        )
                        .push(if coin.spend_info.is_none() {
                            if let (Some(b), Some(timelock)) = (coin.block_height, timelock) {
                                if blockheight > b as u32 + timelock as u32 {
                                    Some(Container::new(
                                        p1_bold("One or more recovery paths are available")
                                            .style(theme::text::error),
                                    ))
                                } else {
                                    Some(Container::new(p1_bold(format!(
                                        "First recovery path will be available in {} blocks",
                                        b as u32 + timelock as u32 - blockheight
                                    ))))
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        })
                        .push(
                            Column::new()
                                .push(
                                    Row::new()
                                        .align_y(Alignment::Center)
                                        .push(
                                            p2_regular("Address label:")
                                                .bold()
                                                .style(theme::text::secondary),
                                        )
                                        .push(if let Some(label) = labels.get(&address) {
                                            p2_regular(label).style(theme::text::secondary)
                                        } else {
                                            p2_regular("No label").style(theme::text::secondary)
                                        })
                                        .spacing(5),
                                )
                                .push(
                                    Row::new()
                                        .align_y(Alignment::Center)
                                        .push(
                                            p2_regular("Address:")
                                                .bold()
                                                .style(theme::text::secondary),
                                        )
                                        .push(
                                            Row::new()
                                                .align_y(Alignment::Center)
                                                .push(
                                                    p2_regular(address.clone())
                                                        .style(theme::text::secondary),
                                                )
                                                .push(
                                                    Button::new(icon::clipboard_icon())
                                                        .on_press(Message::Clipboard(
                                                            address.clone(),
                                                        ))
                                                        .style(theme::button::transparent_border),
                                                ),
                                        )
                                        .spacing(5),
                                )
                                .push(
                                    Row::new()
                                        .align_y(Alignment::Center)
                                        .push(
                                            p2_regular("Deposit transaction label:")
                                                .bold()
                                                .style(theme::text::secondary),
                                        )
                                        .push(if let Some(label) = labels.get(&txid) {
                                            p2_regular(label).style(theme::text::secondary)
                                        } else {
                                            p2_regular("No label").style(theme::text::secondary)
                                        })
                                        .spacing(5),
                                )
                                .push(
                                    Row::new()
                                        .align_y(Alignment::Center)
                                        .push(
                                            p2_regular("Outpoint:")
                                                .bold()
                                                .style(theme::text::secondary),
                                        )
                                        .push(
                                            Row::new()
                                                .align_y(Alignment::Center)
                                                .push(
                                                    p2_regular(format!("{}", coin.outpoint))
                                                        .style(theme::text::secondary),
                                                )
                                                .push(
                                                    Button::new(icon::clipboard_icon())
                                                        .on_press(Message::Clipboard(
                                                            coin.outpoint.to_string(),
                                                        ))
                                                        .style(theme::button::transparent_border),
                                                ),
                                        )
                                        .spacing(5),
                                )
                                .push(coin.block_height.map(|b| {
                                    Row::new()
                                        .push(
                                            p2_regular("Block height:")
                                                .bold()
                                                .style(theme::text::secondary),
                                        )
                                        .push(
                                            p2_regular(format!("{}", b))
                                                .style(theme::text::secondary),
                                        )
                                        .spacing(5)
                                })),
                        )
                        .push(if let Some(info) = coin.spend_info {
                            Column::new()
                                .push(
                                    Row::new()
                                        .push(
                                            p2_regular("Spend txid:")
                                                .bold()
                                                .style(theme::text::secondary),
                                        )
                                        .push(p2_regular(format!("{}", info.txid)))
                                        .spacing(5),
                                )
                                .push(if let Some(height) = info.height {
                                    Row::new()
                                        .push(
                                            p2_regular("Spend block height:")
                                                .bold()
                                                .style(theme::text::secondary),
                                        )
                                        .push(p2_regular(format!("{}", height)))
                                        .spacing(5)
                                } else {
                                    Row::new().push(
                                        p2_regular("Not in a block")
                                            .bold()
                                            .style(theme::text::secondary),
                                    )
                                })
                                .spacing(5)
                        } else {
                            Column::new().push(
                                Row::new().push(Space::new().width(Length::Fill)).push({
                                    let (icon, label) =
                                        (Some(icon::arrow_repeat()), "Refresh coin");
                                    let refresh_btn = if timelock.is_some_and(|timelock| {
                                        remaining_sequence(coin, blockheight, timelock) == 0
                                    }) {
                                        button::primary(icon, label)
                                    } else {
                                        button::secondary(icon, label)
                                    };
                                    refresh_btn.on_press(Message::Menu(Menu::Vault(
                                        crate::app::menu::VaultSubMenu::Coins(Some(vec![
                                            coin.outpoint,
                                        ])),
                                    )))
                                }),
                            )
                        }),
                )
            } else {
                None
            }),
    )
    .style(theme::card::simple)
}

pub fn coin_sequence_label<'a, T: 'a>(seq: u32, timelock: u32) -> Container<'a, T> {
    if seq == 0 {
        Container::new(
            Row::new()
                .spacing(5)
                .push(icon::clock_icon().width(Length::Fixed(20.0)))
                .push(p2_regular("Expired"))
                .align_y(Alignment::Center),
        )
        .padding(10)
        .style(theme::pill::warning)
    } else if seq < timelock * 10 / 100 {
        Container::new(
            Row::new()
                .spacing(5)
                .push(icon::clock_icon().width(Length::Fixed(20.0)))
                .push(p2_regular(expire_message(seq)))
                .align_y(Alignment::Center),
        )
        .padding(10)
        .style(theme::pill::simple)
    } else {
        Container::new(
            Row::new()
                .spacing(5)
                .push(icon::clock_icon().width(Length::Fixed(20.0)))
                .push(p2_regular(expire_message(seq)).style(theme::text::secondary))
                .align_y(Alignment::Center),
        )
        .padding(10)
        .style(theme::pill::simple)
    }
}

pub fn expire_message(sequence: u32) -> String {
    if sequence <= 144 {
        "Expires today".to_string()
    } else if sequence <= 2 * 144 {
        "Expires in ≈ 2 days".to_string()
    } else {
        format!("Expires in {}", expire_message_units(sequence).join(","))
    }
}

/// returns y,m,d
pub fn expire_message_units(sequence: u32) -> Vec<String> {
    let mut n_minutes = sequence * 10;
    let n_years = n_minutes / 525960;
    n_minutes -= n_years * 525960;
    let n_months = n_minutes / 43830;
    n_minutes -= n_months * 43830;
    let n_days = n_minutes / 1440;

    #[allow(clippy::nonminimal_bool)]
    if n_years != 0 || n_months != 0 || n_days != 0 {
        [(n_years, "year"), (n_months, "month"), (n_days, "day")]
            .iter()
            .filter_map(|(n, u)| {
                if *n != 0 {
                    Some(format!("{} {}{}", n, u, if *n > 1 { "s" } else { "" }))
                } else {
                    None
                }
            })
            .collect()
    } else {
        n_minutes -= n_days * 1440;
        let n_hours = n_minutes / 60;
        n_minutes -= n_hours * 60;
        [(n_hours, "hour"), (n_minutes, "minute")]
            .iter()
            .filter_map(|(n, u)| {
                if *n != 0 {
                    Some(format!("{} {}{}", n, u, if *n > 1 { "s" } else { "" }))
                } else {
                    None
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::preferences::{BalancePrivacy, BalancePrivacyChrome};
    use crate::app::view::shared::landing::capture;
    use coincube_core::miniscript::bitcoin::{bip32::ChildNumber, Address, Amount, OutPoint, Txid};
    use std::str::FromStr;

    #[test]
    fn confirmation_copy_distinguishes_unknown_tip_from_unconfirmed_coin() {
        assert_eq!(confirmation_status(None, 0), "Unconfirmed");
        assert_eq!(
            confirmation_status(Some(100), 0),
            "Confirmed · confirmations unavailable"
        );
        assert_eq!(
            confirmation_status(Some(100), 99),
            "Confirmed · confirmations unavailable"
        );
        assert_eq!(confirmation_status(Some(100), 100), "1 confirmation");
        assert_eq!(confirmation_status(Some(100), 101), "2 confirmations");
    }

    #[test]
    fn hidden_coins_list_masks_amount_without_removing_coin() {
        let coin = Coin {
            amount: Amount::from_sat(98_765_432),
            outpoint: OutPoint::new(Txid::from_str(&format!("{:064x}", 1)).unwrap(), 0),
            address: Address::from_str("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4")
                .unwrap()
                .assume_checked(),
            block_height: Some(100),
            derivation_index: ChildNumber::from_normal_idx(0).unwrap(),
            spend_info: None,
            is_immature: false,
            is_change: false,
            is_from_self: false,
        };
        let coins = [coin];
        let labels = HashMap::new();
        let selected = [];
        for privacy in [BalancePrivacy::Hidden, BalancePrivacy::Shown] {
            let cache = Cache {
                balance_privacy: Some(BalancePrivacyChrome {
                    privacy,
                    busy: false,
                    interactive: false,
                }),
                ..Default::default()
            };
            let mounted = capture::inspect(
                "vault-coins-privacy",
                coins_view(
                    &cache,
                    &coins,
                    None,
                    &selected,
                    &labels,
                    &HashMap::new(),
                    BitcoinDisplayUnit::Sats,
                    false,
                ),
                (1024, 768),
                "dark",
                theme::Theme::dark(),
            );
            assert!(mounted.contains("Coin control"));
            assert!(mounted.contains("Details"));
            assert!(mounted.contains("Confirmed · confirmations unavailable"));
            assert_eq!(
                mounted.contains("98,765,432"),
                privacy == BalancePrivacy::Shown
            );
            assert_eq!(
                mounted.contains("••••••"),
                privacy == BalancePrivacy::Hidden
            );
        }
    }

    #[test]
    fn presence_badges_never_treat_a_missing_transaction_as_replay_protection() {
        assert!(entangled_badge::<Message>(Entanglement::Entangled).is_some());
        assert!(entangled_badge::<Message>(Entanglement::Unknown).is_some());
        assert!(entangled_badge::<Message>(Entanglement::NotEntangled).is_some());
    }

    #[test]
    fn test_expire_message_units() {
        let testcases = [
            (61, vec!["10 hours".to_string(), "10 minutes".to_string()]),
            (1112, vec!["7 days".to_string()]),
            (52600, vec!["1 year".to_string()]),
        ];

        for (seq, result) in testcases {
            assert_eq!(expire_message_units(seq), result);
        }
    }
}
