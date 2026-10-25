//! Tenshu v113 components for the redesigned presenters.
//!
//! These are presentation contracts. Callers supply typed values, stable IDs,
//! the owner's focus intent and existing intents; the components hold no
//! wallet, balance, privacy or route state. Classic components elsewhere in
//! `component` are unchanged for screens that have not been ported.

pub mod action;
pub mod asset_row;
pub mod balance;
pub mod chip;
pub mod field;
mod scope;
pub mod segmented;
pub mod setting_row;
pub mod surface;
pub mod text;
pub mod toggle;

#[cfg(test)]
mod gallery;

pub use action::{chip_action, eye_action, transfer_action, ActionButton};
pub use asset_row::asset_row;
pub use balance::{balance, AmountVisibility, BalanceTier, BalanceValue, Figure, MASK};
pub use chip::{status_badge, status_chip, StatusBadge, StatusChip};
pub use field::{search_field, text_field, FieldSize, TextField};
pub use segmented::{segment, segmented, Segment, SegmentSize, Segmented};
pub use setting_row::{setting_row, SettingRow};
pub use surface::{card, panel};
pub use text::{body, caption, label, mono, section, title};
pub use toggle::{toggle, Toggle, ToggleForm};

pub use crate::theme::tenshu::button::ActionKind;
pub use crate::theme::tenshu::Tone;
