//! Public Bitcoin wallet source input shared by Split and wallet import.
//!
//! This component owns only the source choice, descriptor text and public
//! hardware-account reader. Callers own discovery, persistence and spending.

use coincube_ui::{
    component::{button, text::*},
    theme,
    widget::{Column, Element, Row},
};
use iced::{widget::text_input, Length, Subscription, Task};

use crate::{
    dir::CoincubeDirectory,
    split_hardware::{self, HardwareAccount, HardwareMessage, HardwareSource},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Descriptor,
    Hardware,
}

#[derive(Debug, Clone)]
pub enum Message {
    SourceSelected(Source),
    Hardware(HardwareMessage),
    ExternalEdited(String),
    InternalEdited(String),
}

pub struct WalletSourceForm {
    source: Source,
    external: String,
    internal: String,
    hardware: HardwareSource,
}

impl Default for WalletSourceForm {
    fn default() -> Self {
        Self {
            source: Source::Descriptor,
            external: String::new(),
            internal: String::new(),
            hardware: HardwareSource::default(),
        }
    }
}

impl WalletSourceForm {
    pub fn source(&self) -> Source {
        self.source
    }

    pub fn external(&self) -> &str {
        &self.external
    }

    pub fn internal(&self) -> &str {
        &self.internal
    }

    pub fn hardware_account(&self) -> Option<&HardwareAccount> {
        self.hardware.account()
    }

    pub fn clear_transient(&mut self) {
        self.hardware.clear();
    }

    pub fn set_hardware_root(&mut self, datadir: CoincubeDirectory) {
        self.hardware.set_root(datadir);
    }

    pub fn polls_hardware(&self) -> bool {
        self.source == Source::Hardware
    }

    pub fn subscription(&self) -> Subscription<Message> {
        if self.polls_hardware() {
            self.hardware.subscription().map(Message::Hardware)
        } else {
            Subscription::none()
        }
    }

    /// Returns whether a source change invalidates discovery evidence.
    pub fn select_source(&mut self, source: Source) -> bool {
        if source == self.source {
            return false;
        }
        self.hardware.clear();
        self.source = source;
        true
    }

    /// Descriptor edits invalidate discovery even if the value is unchanged:
    /// callers use the edit event to revoke any in-flight result.
    pub fn edit_external(&mut self, value: String) {
        self.hardware.clear();
        self.external = value;
    }

    pub fn edit_internal(&mut self, value: String) {
        self.hardware.clear();
        self.internal = value;
    }

    pub fn update_hardware(&mut self, message: HardwareMessage) -> (Task<Message>, bool) {
        let (task, invalidated) = self.hardware.update(message);
        (task.map(Message::Hardware), invalidated)
    }

    #[cfg(test)]
    pub(crate) fn hardware_mut(&mut self) -> &mut HardwareSource {
        &mut self.hardware
    }
}

/// Flow-specific copy around the same controls. Split's text is kept verbatim
/// while Import can require change information and explain its own limits.
pub struct SourceFormCopy {
    pub internal_label: &'static str,
    pub descriptor_help: &'static str,
    pub hardware_help: &'static str,
}

impl SourceFormCopy {
    pub const fn split() -> Self {
        Self {
            internal_label: "Internal / change descriptor (optional)",
            descriptor_help: "Supported for discovery: pkh, sh(wpkh), wpkh, wsh(multi/sortedmulti), and tr key-path (scan only: tr has no signing route, so it cannot be split). Only pre-fork coins can be split. Only public mainnet descriptors are accepted. The bounded scan uses a gap of 20 and checks at most 100 addresses per branch.",
            hardware_help: "Hardware accounts use standard singlesig paths (BIP84, BIP49, BIP44) on Bitcoin mainnet. Only pre-fork coins can be split. The bounded scan uses a gap of 20 and checks at most 100 addresses per branch.",
        }
    }

    pub const fn import() -> Self {
        Self {
            internal_label: "Internal / change descriptor",
            descriptor_help: "Import public Bitcoin mainnet descriptors for the receive and change branches. A combined descriptor with /<0;1>/* can be entered in the first field. Include every signer origin for hardware and PSBT signing. Only public keys are accepted; this Cube's recovery seed is separate from the wallet's signing keys.",
            hardware_help: "Read one standard Bitcoin mainnet singlesig account from a connected hardware wallet (BIP84, BIP49 or BIP44). The device stays responsible for signing; its seed is never copied into this Cube.",
        }
    }
}

pub fn view<'a>(form: &'a WalletSourceForm, copy: &SourceFormCopy) -> Element<'a, Message> {
    let source_button = |label, source| {
        let button = if form.source == source {
            button::primary(None, label)
        } else {
            button::secondary(None, label)
        };
        button.on_press(Message::SourceSelected(source))
    };
    let content = Column::new()
        .spacing(12)
        .push(p1_bold("Source wallet"))
        .push(
            Row::new()
                .spacing(10)
                .push(source_button("Public descriptor", Source::Descriptor))
                .push(source_button("Hardware wallet", Source::Hardware)),
        );
    if form.source == Source::Hardware {
        content
            .push(split_hardware::view(&form.hardware).map(Message::Hardware))
            .push(caption(copy.hardware_help).style(theme::text::secondary))
            .into()
    } else {
        content
            .push(p1_bold("External / receive descriptor"))
            .push(
                text_input("wpkh([fingerprint/path]xpub.../0/*)", &form.external)
                    .on_input(Message::ExternalEdited)
                    .padding(10)
                    .width(Length::Fill),
            )
            .push(p1_bold(copy.internal_label))
            .push(
                text_input("wpkh([fingerprint/path]xpub.../1/*)", &form.internal)
                    .on_input(Message::InternalEdited)
                    .padding(10)
                    .width(Length::Fill),
            )
            .push(caption(copy.descriptor_help).style(theme::text::secondary))
            .into()
    }
}
