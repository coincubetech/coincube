//! Import or restore's Keys step: the descriptor names its signers and the
//! user says where each key is. Recovery words are checked against the
//! signer's fingerprint, origin and xpub in both branches and are stored in
//! this Cube once the wallet is attached. A hardware wallet is checked
//! against the connected device and keeps its seed. In a multisig wallet the
//! signers past the threshold may sign elsewhere (Robert, 2026-11-02, #914).

use coincube_core::{
    imported_keys::ImportedAccount,
    miniscript::{DescriptorPublicKey, ForEachKey},
    wallet_descriptor::ImportedWalletDescriptor,
};
use coincube_ui::{
    component::{button, text::*},
    theme,
    widget::{Column, Element, Row},
};
use iced::{widget::text_input, Alignment, Length};
use zeroize::Zeroizing;

use crate::app::view;

use super::ImportMessage;

/// Where the user keeps one signer's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Words,
    Hardware,
    /// Another wallet or person signs; Tenshu saves the PSBT for them.
    Elsewhere,
}

impl KeySource {
    fn label(self) -> &'static str {
        match self {
            Self::Words => "Recovery words",
            Self::Hardware => "Hardware wallet",
            Self::Elsewhere => "Signs elsewhere",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum KeyStatus {
    Missing,
    Checking,
    Verified,
    Failed(String),
}

/// One signer from the descriptor. Words stay zeroizing and are cleared once
/// stored; nothing here reaches Cube Settings.
pub(super) struct KeySlot {
    pub(super) account: ImportedAccount,
    pub(super) source: KeySource,
    pub(super) words: Zeroizing<String>,
    pub(super) passphrase: Zeroizing<String>,
    pub(super) status: KeyStatus,
}

impl KeySlot {
    /// A key this Cube can sign with.
    pub(super) fn provided(&self) -> bool {
        self.status == KeyStatus::Verified && self.source != KeySource::Elsewhere
    }
}

pub(super) struct KeysState {
    pub(super) slots: Vec<KeySlot>,
    pub(super) threshold: usize,
}

impl KeysState {
    pub(super) fn new(descriptor: &ImportedWalletDescriptor) -> Self {
        let slots = signers(descriptor)
            .into_iter()
            .map(|account| KeySlot {
                account,
                source: KeySource::Words,
                words: Zeroizing::new(String::new()),
                passphrase: Zeroizing::new(String::new()),
                status: KeyStatus::Missing,
            })
            .collect::<Vec<_>>();
        let threshold = signing_threshold(descriptor).clamp(1, slots.len().max(1));
        Self { slots, threshold }
    }

    pub(super) fn multisig(&self) -> bool {
        self.slots.len() > 1
    }

    pub(super) fn provided(&self) -> usize {
        self.slots.iter().filter(|slot| slot.provided()).count()
    }

    /// Enough keys to sign, and every other signer settled.
    pub(super) fn ready(&self) -> bool {
        self.provided() >= self.threshold
            && self.slots.iter().all(|slot| {
                slot.provided() || (slot.source == KeySource::Elsewhere && self.multisig())
            })
    }

    pub(super) fn checking(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| slot.status == KeyStatus::Checking)
    }

    /// Review's one-line account of the keys.
    pub(super) fn summary(&self) -> String {
        let mut sources = Vec::new();
        for source in [KeySource::Words, KeySource::Hardware] {
            let count = self
                .slots
                .iter()
                .filter(|slot| slot.source == source && slot.provided())
                .count();
            if count > 0 {
                sources.push(if source == KeySource::Words {
                    "recovery words"
                } else {
                    "hardware wallet"
                });
            }
        }
        let mut line = format!(
            "Keys: {} of {} provided · {} · verified against the fingerprint",
            self.provided(),
            self.slots.len(),
            sources.join(" and ")
        );
        let elsewhere = self
            .slots
            .iter()
            .filter(|slot| slot.source == KeySource::Elsewhere)
            .count();
        if elsewhere > 0 {
            line.push_str(&format!(" · {elsewhere} signs elsewhere"));
        }
        line
    }
}

/// Each xpub signer with an origin, in descriptor order, that appears in both
/// branches. The descriptor parser already refuses a key missing from a branch.
pub(super) fn signers(descriptor: &ImportedWalletDescriptor) -> Vec<ImportedAccount> {
    let mut accounts: Vec<ImportedAccount> = Vec::new();
    descriptor.receive_descriptor().for_each_key(|key| {
        if let DescriptorPublicKey::XPub(key) = key {
            if let Some((fingerprint, origin)) = &key.origin {
                let account = ImportedAccount {
                    fingerprint: *fingerprint,
                    origin: origin.clone(),
                    xpub: key.xkey,
                };
                if !accounts.contains(&account)
                    && coincube_core::imported_keys::account_in_both_branches(descriptor, &account)
                {
                    accounts.push(account);
                }
            }
        }
        true
    });
    accounts
}

/// Signatures a spend needs: `k` of a `multi`/`sortedmulti`, else one. The
/// imported shapes are `pkh`, `wpkh`, `sh(wpkh)` and `wsh((sorted)multi)`.
pub(super) fn signing_threshold(descriptor: &ImportedWalletDescriptor) -> usize {
    descriptor
        .receive_descriptor()
        .to_string()
        .split_once("multi(")
        .and_then(|(_, rest)| rest.split_once(','))
        .and_then(|(k, _)| k.trim().parse().ok())
        .unwrap_or(1)
}

fn message(message: ImportMessage) -> view::Message {
    view::Message::ImportedBitcoin(message)
}

/// The Keys step's form and action row.
pub(super) fn step<'a>(
    keys: &'a KeysState,
    hardware_connected: impl Fn(&ImportedAccount) -> bool,
    error: Option<&'a String>,
) -> (Element<'a, view::Message>, Element<'a, view::Message>) {
    let count = keys.slots.len();
    let lead = if keys.multisig() {
        format!(
            "The descriptor names {count} signers and needs {} to spend. Provide at least {} \
             of them so this Cube can sign; mark the others as signing elsewhere. Each key \
             is checked against its fingerprint before it is accepted.",
            keys.threshold, keys.threshold
        )
    } else {
        "The descriptor names 1 signer. Provide its key so this Cube can sign with the \
         wallet. The key is checked against its fingerprint before it is accepted."
            .to_string()
    };
    let mut content = Column::new()
        .spacing(20)
        .push(h3("Add the wallet's keys"))
        .push(p1_regular(lead).style(theme::text::secondary));
    let busy = keys.checking();
    for (index, slot) in keys.slots.iter().enumerate() {
        let mut card = Column::new().spacing(12).push(p1_bold(format!(
            "Key {} of {count} · fingerprint {} · path {}",
            index + 1,
            slot.account.fingerprint,
            slot.account.origin
        )));
        card = card.push(match &slot.status {
            KeyStatus::Verified if slot.source == KeySource::Elsewhere => {
                p2_regular("Signs elsewhere").style(theme::text::secondary)
            }
            KeyStatus::Verified => p2_regular(format!(
                "Verified · matches {} · {}",
                slot.account.fingerprint,
                slot.source.label().to_lowercase()
            ))
            .style(theme::text::success),
            KeyStatus::Checking => p2_regular("Checking…").style(theme::text::secondary),
            KeyStatus::Failed(reason) => p2_regular(reason.clone()).style(theme::text::error),
            KeyStatus::Missing => p2_regular("Not provided yet").style(theme::text::secondary),
        });
        let mut sources = Row::new().spacing(8);
        let mut choices = vec![KeySource::Words, KeySource::Hardware];
        if keys.multisig() {
            choices.push(KeySource::Elsewhere);
        }
        for source in choices {
            let pick = if slot.source == source {
                button::primary(None, source.label())
            } else {
                button::secondary(None, source.label())
            };
            sources = sources.push(pick.width(Length::Shrink).on_press_maybe(
                (!busy).then_some(message(ImportMessage::KeySourcePicked(index, source))),
            ));
        }
        card = card
            .push(p2_regular("Where is this key?").style(theme::text::secondary))
            .push(sources);
        card = match slot.source {
            // Verified words leave the screen; changing them starts over.
            KeySource::Words if slot.status == KeyStatus::Verified => card.push(
                button::secondary(None, "Enter different words")
                    .width(Length::Shrink)
                    .on_press_maybe((!busy).then_some(message(ImportMessage::KeySourcePicked(
                        index,
                        KeySource::Words,
                    )))),
            ),
            KeySource::Words => card
                .push(p1_bold("Recovery words"))
                .push(
                    text_input("12 or 24 words, separated by spaces", slot.words.as_str())
                        .on_input(move |value| {
                            message(ImportMessage::KeyWordsEdited(
                                index,
                                super::SecretInput::new(value),
                            ))
                        })
                        .padding(10),
                )
                .push(
                    caption(
                        "Words are checked here and stored inside this Cube, protected by its \
                         PIN, like any Vault key.",
                    )
                    .style(theme::text::secondary),
                )
                .push(p1_bold("Passphrase (if the wallet used one)"))
                .push(
                    text_input("Optional", slot.passphrase.as_str())
                        .on_input(move |value| {
                            message(ImportMessage::KeyPassphraseEdited(
                                index,
                                super::SecretInput::new(value),
                            ))
                        })
                        .secure(true)
                        .padding(10)
                        .width(Length::Fixed(320.0)),
                )
                .push(
                    button::secondary(None, "Verify key")
                        .width(Length::Shrink)
                        .on_press_maybe(
                            (!busy
                                && slot.status != KeyStatus::Verified
                                && !slot.words.trim().is_empty())
                            .then_some(message(ImportMessage::VerifyWords(index))),
                        ),
                ),
            KeySource::Hardware => {
                let connected = hardware_connected(&slot.account);
                card.push(
                    caption(if connected {
                        "The seed stays on the device. It signs whenever this wallet spends."
                    } else {
                        "Connect and unlock the device with this fingerprint."
                    })
                    .style(theme::text::secondary),
                )
                .push(
                    button::secondary(None, "Check device")
                        .width(Length::Shrink)
                        .on_press_maybe(
                            (!busy && connected && slot.status != KeyStatus::Verified)
                                .then_some(message(ImportMessage::CheckDevice(index))),
                        ),
                )
            }
            KeySource::Elsewhere => card.push(
                caption(
                    "Another wallet or person signs with this key. Tenshu saves the PSBT \
                     for them.",
                )
                .style(theme::text::secondary),
            ),
        };
        content = content.push(
            coincube_ui::widget::Container::new(card)
                .padding(16)
                .width(Length::Fill)
                .style(theme::card::simple),
        );
    }
    if keys.multisig() && keys.provided() < keys.threshold {
        content = content.push(
            p2_regular(format!(
                "{} of {} keys needed to sign are provided.",
                keys.provided(),
                keys.threshold
            ))
            .style(theme::text::secondary),
        );
    }
    if let Some(error) = error {
        content = content.push(p1_regular(error).style(theme::text::error));
    }
    let actions = Row::new()
        .spacing(10)
        .align_y(Alignment::Center)
        .push(
            button::secondary(None, "Back")
                .width(Length::Shrink)
                .on_press_maybe((!busy).then_some(message(ImportMessage::Edit))),
        )
        .push(iced::widget::Space::new().width(Length::Fill))
        .push(
            button::primary(None, "Review wallet")
                .width(Length::Shrink)
                .on_press_maybe(
                    (!busy && keys.ready()).then_some(message(ImportMessage::KeysDone)),
                ),
        );
    (content.into(), actions.into())
}
