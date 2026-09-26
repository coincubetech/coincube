//! View for the "Paired phones" / local LAN signer settings section.

use iced::widget::{
    qr_code::{self, QRCode},
    text_input, Column, Container, Row, Space,
};
use iced::{Alignment, Font, Length};

use coincube_ui::component::{badge, button, card, separation, text::*};
use coincube_ui::widget::Element;
use coincube_ui::{icon, theme};

use crate::app::cache;
use crate::app::menu::Menu;
use crate::app::state::settings::local_signing::{LocalSigningState, PairableKey, PairingFlow};
use crate::app::view::dashboard;
use crate::app::view::message::{LocalSigningMessage, Message, SettingsMessage};
use crate::phone_signer::errors::PairingError;

pub fn section<'a>(
    menu: &'a Menu,
    cache: &'a cache::Cache,
    state: &'a LocalSigningState,
) -> Element<'a, Message> {
    let mut col = Column::new()
        .spacing(20)
        .push(super::header(
            "Pair with Keychain",
            SettingsMessage::LocalSigningSection,
        ))
        .push(pairing_card(state))
        .push(paired_phones_card(state))
        .width(Length::Fill);

    if !matches!(state.flow, PairingFlow::Waiting { .. }) {
        if let Some(fp) = state.wallet_fingerprint {
            col = col.push(
                text(format!(
                    "Phones paired through this panel will sign for this vault \
                     (id {}). This identifier is derived from the vault \
                     descriptor and is distinct from any individual signer's \
                     master fingerprint.",
                    fp
                ))
                .style(theme::text::secondary),
            );
        }
    }

    dashboard(menu, cache, col)
}

fn pairing_card<'a>(state: &'a LocalSigningState) -> Element<'a, Message> {
    let header = Row::new()
        .push(badge::badge(icon::tooltip_icon()))
        .push(text("Pair a phone").bold())
        .padding(10)
        .spacing(20)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    let body: Element<'a, Message> = match &state.flow {
        PairingFlow::Idle => idle_body(state),
        PairingFlow::PhonePicker { discovered } => picker_body(discovered),
        PairingFlow::Waiting { phone, offer, qr } => waiting_body(phone, offer, qr.as_ref()),
        PairingFlow::Error(e) => error_body(e),
    };

    card::simple(
        Column::new()
            .push(header)
            .push(separation().width(Length::Fill))
            .push(Space::new().height(Length::Fixed(10.0)))
            .push(body),
    )
    .into()
}

fn idle_body<'a>(state: &'a LocalSigningState) -> Element<'a, Message> {
    let intro = text(
        "Pair a Keychain phone over your local network so it can \
         sign PSBTs directly, without going through the Connect \
         API. The phone must be on the same Wi-Fi.",
    );
    if state.keychain_keys_recorded && state.vault_keys.is_empty() {
        return Column::new()
            .padding(10)
            .spacing(8)
            .push(intro)
            .push(no_keychain_keys())
            .into();
    }

    let mut pair_btn = button::secondary(None, "Pair phone");
    if state.wallet_fingerprint.is_some() && state.selected_key.is_some() {
        pair_btn = pair_btn.on_press(Message::Settings(SettingsMessage::LocalSigning(
            LocalSigningMessage::StartPairing,
        )));
    }
    let prompt = if state.vault_keys.len() == 1 {
        "The Keychain key this phone will sign for:"
    } else {
        "Select the key held by this phone:"
    };
    let mut keys = Column::new().spacing(8).push(text(prompt));
    for key in &state.vault_keys {
        keys = keys.push(key_row(
            key,
            state.selected_key.as_ref() == Some(&key.xpub),
            state.show_key_details,
        ));
    }
    let details = button::link(
        None,
        if state.show_key_details {
            "Hide key details"
        } else {
            "Show key details"
        },
    )
    .on_press(Message::Settings(SettingsMessage::LocalSigning(
        LocalSigningMessage::ToggleKeyDetails,
    )));
    Column::new()
        .padding(10)
        .spacing(8)
        .push(intro)
        .push(keys)
        .push(details)
        .push(
            text("Key names can be changed in Vault → Settings → Wallet.")
                .style(theme::text::secondary),
        )
        .push(pair_btn)
        .into()
}

/// The Vault has no key from the Keychain app, so no phone holds a key it
/// could sign with. Says so instead of offering a pairing that must fail.
fn no_keychain_keys<'a>() -> Element<'a, Message> {
    Column::new()
        .spacing(4)
        .push(text("No Keychain keys to pair").bold())
        .push(
            text(
                "This Vault doesn't have a key from the COINCUBE Keychain \
                 app, so there's no phone that can sign for it. Keychain \
                 keys are added when you create a Vault.",
            )
            .style(theme::text::secondary),
        )
        .into()
}

/// A key's display name: the name the user gave it, else a generic label
/// with its fingerprint, never the raw xpub.
fn key_title(key: &PairableKey) -> String {
    match (&key.name, key.fingerprint) {
        (Some(name), _) => name.clone(),
        (None, Some(fp)) if key.keychain => format!("Keychain key {}", fp),
        (None, Some(fp)) => format!("Key {}", fp),
        (None, None) => "Unnamed key".to_string(),
    }
}

/// The line under a key's name: where it comes from and the fingerprint
/// the phone shows for it.
fn key_subtitle(key: &PairableKey) -> Option<String> {
    match (key.keychain, key.fingerprint) {
        (true, Some(fp)) => Some(format!("Keychain · {}", fp)),
        (true, None) => Some("Keychain".to_string()),
        (false, Some(fp)) => Some(fp.to_string()),
        (false, None) => None,
    }
}

fn key_row<'a>(key: &'a PairableKey, selected: bool, show_details: bool) -> Element<'a, Message> {
    let mut title = Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(text(key_title(key)).bold().width(Length::Fill));
    if selected {
        title = title.push(text("Selected").style(theme::text::success));
    }
    let mut content = Column::new().spacing(2).push(title);
    if let Some(subtitle) = key_subtitle(key) {
        content = content.push(text(subtitle).style(theme::text::secondary));
    }
    if show_details {
        content = content.push(
            text(&key.descriptor_key)
                .font(Font::MONOSPACE)
                .size(12)
                .style(theme::text::secondary)
                .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
        );
    }
    iced::widget::Button::new(content.width(Length::Fill))
        .width(Length::Fill)
        .style(theme::button::secondary)
        .on_press(Message::Settings(SettingsMessage::LocalSigning(
            LocalSigningMessage::SelectKey(key.xpub.clone()),
        )))
        .into()
}

fn waiting_body<'a>(
    phone: &'a crate::phone_signer::mdns::DiscoveredPhone,
    offer: &'a crate::phone_signer::pairing::PairingOffer,
    qr: Option<&'a qr_code::Data>,
) -> Element<'a, Message> {
    let remaining = crate::phone_signer::pairing::seconds_remaining(offer);
    let countdown = if remaining > 0 {
        format!("Waiting for {} — expires in {}s", phone.cert_fp8, remaining)
    } else {
        "Pairing offer expired.".to_string()
    };

    let mut body = Column::new()
        .padding(10)
        .spacing(10)
        .align_x(Alignment::Center)
        .push(text(countdown))
        .push(text(format!(
            "Scan this QR with the Keychain app on \
             phone {} ({}).",
            phone.cert_fp8, phone.addr,
        )));
    if let Some(qr) = qr {
        body = body.push(
            Container::new(QRCode::<coincube_ui::theme::Theme>::new(qr).cell_size(4)).padding(10),
        );
    }
    body = body.push(
        button::secondary(None, "Cancel").on_press(Message::Settings(
            SettingsMessage::LocalSigning(LocalSigningMessage::CancelPairing),
        )),
    );
    body.into()
}

fn picker_body<'a>(
    discovered: &'a [crate::phone_signer::mdns::DiscoveredPhone],
) -> Element<'a, Message> {
    if discovered.is_empty() {
        return Column::new()
            .padding(10)
            .spacing(8)
            .push(text("Looking for phones…").bold())
            .push(text(
                "No phones found on this Wi-Fi yet. Open the Keychain \
                 app on a phone on the same network and make sure it's \
                 unlocked, then wait a few seconds.",
            ))
            .push(
                button::secondary(None, "Cancel").on_press(Message::Settings(
                    SettingsMessage::LocalSigning(LocalSigningMessage::CancelPairing),
                )),
            )
            .into();
    }
    let mut col = Column::new()
        .padding(10)
        .spacing(8)
        .push(text("Pick a phone to pair with").bold())
        .push(text(
            "These are the Keychain phones currently advertising on \
             this Wi-Fi. The list refreshes every second.",
        ));
    for d in discovered {
        let fp8 = d.cert_fp8.clone();
        let row = Row::new()
            .padding([6, 0])
            .spacing(12)
            .align_y(Alignment::Center)
            .push(
                Column::new()
                    .push(text(format!("Phone {}", d.cert_fp8)).bold())
                    .push(text(format!("{}", d.addr)).style(theme::text::secondary))
                    .width(Length::FillPortion(3)),
            )
            .push(button::secondary(None, "Pair").on_press(Message::Settings(
                SettingsMessage::LocalSigning(LocalSigningMessage::PickPhone(fp8)),
            )));
        col = col.push(row);
    }
    col = col.push(separation().width(Length::Fill));
    col = col.push(
        button::secondary(None, "Cancel").on_press(Message::Settings(
            SettingsMessage::LocalSigning(LocalSigningMessage::CancelPairing),
        )),
    );
    col.into()
}

fn error_body<'a>(err: &'a PairingError) -> Element<'a, Message> {
    let (title, body) = match err {
        PairingError::OfferExpired => (
            "Offer expired",
            "The pairing offer ran out before any phone scanned it. \
             Generate a new offer and try again."
                .to_string(),
        ),
        PairingError::WalletFingerprintMismatch { expected, claimed } => (
            "Wrong wallet",
            format!(
                "The phone reports it can sign for wallet {} but \
                 this wallet expects {}. Confirm the phone is paired \
                 with the right vault.",
                claimed,
                expected
                    .iter()
                    .map(|fp| fp.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        PairingError::ReplayRefused => (
            "QR already used",
            "This pairing QR has already completed a pairing. Cancel \
             and start a fresh offer rather than re-using it."
                .to_string(),
        ),
        PairingError::PhoneVerificationFailed => (
            "Couldn't verify the phone",
            "The device that scanned the QR couldn't prove it's the \
             phone you're pairing. This can happen on an untrusted \
             network. Make sure both devices are on a network you \
             trust, then start a fresh pairing."
                .to_string(),
        ),
        PairingError::TransportKeyMissing => (
            "Keychain needs updating",
            "This Keychain didn't send an encryption key, so signing requests \
             to it couldn't be encrypted. Update the Keychain app on the phone, \
             then pair again."
                .to_string(),
        ),
        PairingError::NetworkError(s) => (
            "Network error",
            format!(
                "Could not finish the TLS handshake or stream the \
                 pairing envelope. Confirm both devices are on the \
                 same Wi-Fi and try again. ({})",
                s
            ),
        ),
        PairingError::InternalError(s) => ("Pairing failed", s.clone()),
    };

    let mut col = Column::new()
        .padding(10)
        .spacing(8)
        .push(text(title).bold().style(theme::text::error))
        .push(text(body).style(theme::text::error));
    if err.is_retriable() {
        col = col.push(
            button::secondary(None, "Try again").on_press(Message::Settings(
                SettingsMessage::LocalSigning(LocalSigningMessage::StartPairing),
            )),
        );
    }
    col.into()
}

fn paired_phones_card<'a>(state: &'a LocalSigningState) -> Element<'a, Message> {
    let header = Row::new()
        .push(badge::badge(icon::tooltip_icon()))
        .push(text("Paired phones").bold())
        .padding(10)
        .spacing(20)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    // Pairing is vault-scoped: only list phones paired with the
    // currently-loaded vault, so this list agrees with the pairing
    // card's "will sign for this vault" copy and the hw refresh loop's
    // scoping. A phone paired for another vault is managed from that
    // vault's panel.
    let phones_for_vault: Vec<&crate::phone_signer::pairing_store::PairedPhone> =
        match state.wallet_fingerprint {
            Some(vid) => state
                .phones
                .phones
                .iter()
                .filter(|p| p.vault_fingerprint == vid)
                .collect(),
            None => Vec::new(),
        };

    let body: Element<'a, Message> = if phones_for_vault.is_empty() {
        text(
            "No paired phones for this vault yet. Use 'Pair phone' \
             above to add one. Paired phones appear as a signer \
             whenever they're reachable on your Wi-Fi.",
        )
        .style(theme::text::secondary)
        .into()
    } else {
        let mut rows = Column::new().padding(10).spacing(10);
        for p in phones_for_vault {
            let fp8 = crate::phone_signer::identity::pin_hex8(&p.cert_pin);
            let draft = state.row_drafts.get(&fp8);
            let name_value = draft.map(|d| d.name.as_str()).unwrap_or(&p.name);
            let fallback_value = draft
                .map(|d| d.fallback.as_str())
                .unwrap_or_else(|| p.fallback_addr.as_deref().unwrap_or(""));
            let fp8_for_name = fp8.clone();
            let fp8_for_fb = fp8.clone();
            let fp8_for_save = fp8.clone();
            // Must be the same predicate signing and the hw refresh loop
            // apply. Matching only `descriptor_sha256` here would report
            // "Exact vault key paired" for a phone whose binding fails on
            // key id, vault id, or key membership — a healthy row the user
            // then can't sign with.
            let identity_status = if p
                .exact_signer_against(&state.descriptor_sha256, &state.vault_key_fingerprints)
                .is_ok()
            {
                "Exact vault key paired"
            } else {
                "Pair again and select the exact vault key held by this phone."
            };
            let row = Column::new()
                .push(text(identity_status))
                .padding([6, 0])
                .spacing(6)
                .push(
                    Row::new()
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(
                            text_input("Phone name", name_value)
                                .on_input(move |s| {
                                    Message::Settings(SettingsMessage::LocalSigning(
                                        LocalSigningMessage::DraftName(fp8_for_name.clone(), s),
                                    ))
                                })
                                .width(Length::FillPortion(3)),
                        )
                        .push(
                            Container::new(
                                text(fp8.clone())
                                    .size(P2_SIZE)
                                    .font(Font::MONOSPACE)
                                    .style(theme::text::secondary),
                            )
                            .padding([2, 6])
                            .width(Length::Shrink),
                        ),
                )
                .push(
                    Row::new()
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(
                            text_input(
                                "Fallback host:port (mDNS-blocked networks)",
                                fallback_value,
                            )
                            .on_input(move |s| {
                                Message::Settings(SettingsMessage::LocalSigning(
                                    LocalSigningMessage::DraftFallback(fp8_for_fb.clone(), s),
                                ))
                            })
                            .width(Length::FillPortion(3)),
                        )
                        .push(
                            button::secondary(None, "Save")
                                .on_press(Message::Settings(SettingsMessage::LocalSigning(
                                    LocalSigningMessage::SaveRow(fp8_for_save),
                                )))
                                .width(Length::Shrink),
                        )
                        .push(
                            button::secondary(None, "Remove")
                                .on_press(Message::Settings(SettingsMessage::LocalSigning(
                                    LocalSigningMessage::RemovePhone(fp8.clone()),
                                )))
                                .width(Length::Shrink),
                        ),
                );
            rows = rows.push(row);
            rows = rows.push(separation().width(Length::Fill));
        }
        rows.into()
    };

    card::simple(
        Column::new()
            .push(header)
            .push(separation().width(Length::Fill))
            .push(Space::new().height(Length::Fixed(10.0)))
            .push(body),
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::bip32::Fingerprint;
    use std::str::FromStr;

    fn key(name: Option<&str>, keychain: bool) -> PairableKey {
        PairableKey {
            xpub: "tpubD6NzVbkrYhZ4X".to_string(),
            descriptor_key: "[f108451f/48'/1'/0'/2']tpubD6NzVbkrYhZ4X".to_string(),
            fingerprint: Some(Fingerprint::from_str("f108451f").unwrap()),
            name: name.map(str::to_string),
            keychain,
        }
    }

    #[test]
    fn keys_are_labelled_by_name_and_never_by_xpub() {
        let named = key(Some("My iPhone"), true);
        assert_eq!(key_title(&named), "My iPhone");
        assert_eq!(key_subtitle(&named).as_deref(), Some("Keychain · f108451f"));

        let unnamed = key(None, true);
        assert_eq!(key_title(&unnamed), "Keychain key f108451f");

        let other = key(None, false);
        assert_eq!(key_title(&other), "Key f108451f");
        assert_eq!(key_subtitle(&other).as_deref(), Some("f108451f"));

        for k in [named, unnamed, other] {
            assert!(!key_title(&k).contains("tpub"));
            assert!(!key_subtitle(&k).unwrap_or_default().contains("tpub"));
        }
    }

    #[test]
    fn idle_body_renders_both_the_empty_state_and_a_key_list() {
        let mut state = LocalSigningState::default();
        state.keychain_keys_recorded = true;
        let _ = idle_body(&state);

        state.vault_keys = vec![key(Some("My iPhone"), true)];
        state.show_key_details = true;
        let _ = idle_body(&state);
    }
}
