//! Public Bitcoin wallet source input shared by Split and wallet import.
//!
//! This component owns only the source choice, descriptor text and public
//! hardware-account reader. Callers own discovery, persistence and spending.

use coincube_ui::theme::tenshu::button::ActionKind;
use coincube_ui::{
    component::{tenshu::ActionButton, text::*},
    theme,
    widget::{Column, Element, Row},
};
use iced::advanced::widget::Id;
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
    Focus(Id),
    SourceSelected(Source),
    Hardware(HardwareMessage),
    ExternalEdited(String),
    InternalEdited(String),
}

fn focus(id: Id) -> Message {
    Message::Focus(id)
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

    #[cfg(test)]
    pub(crate) fn hardware_action_id(&self, name: impl AsRef<str>) -> Id {
        self.hardware.action_id(name)
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
            descriptor_help: "Use public Bitcoin mainnet descriptors with signer origins for hardware and PSBT signing. For a combined descriptor ending in /<0;1>/*, use the receive field and leave change empty.",
            hardware_help: "Read one standard Bitcoin mainnet singlesig account from a connected hardware wallet (BIP84, BIP49 or BIP44). The device stays responsible for signing; its seed is never copied into this Cube.",
        }
    }
}

pub fn view<'a>(form: &'a WalletSourceForm, copy: &SourceFormCopy) -> Element<'a, Message> {
    let source_button = |label, source| {
        let kind = if form.source == source {
            ActionKind::Primary
        } else {
            ActionKind::Secondary
        };
        let id = match source {
            Source::Descriptor => "tenshu.source.descriptor",
            Source::Hardware => "tenshu.source.hardware",
        };
        ActionButton::new(id, label, kind, focus).on_press(Message::SourceSelected(source))
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
                    .id("tenshu.source.receive")
                    .on_input(Message::ExternalEdited)
                    .padding(10)
                    .width(Length::Fill),
            )
            .push(p1_bold(copy.internal_label))
            .push(
                text_input("wpkh([fingerprint/path]xpub.../1/*)", &form.internal)
                    .id("tenshu.source.change")
                    .on_input(Message::InternalEdited)
                    .padding(10)
                    .width(Length::Fill),
            )
            .push(caption(copy.descriptor_help).style(theme::text::secondary))
            .into()
    }
}
