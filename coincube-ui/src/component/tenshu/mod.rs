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
pub mod copy_field;
pub mod disclosure;
pub mod field;
mod scope;
pub mod segmented;
pub mod select;
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
pub use copy_field::{copy_field, CopyField};
pub use disclosure::{disclosure, Disclosure};
pub use field::{search_field, text_field, FieldSize, TextField};
pub use segmented::{
    choice_chips, nav_list, radio_list, segment, segmented, tabs, Segment, SegmentSize, Segmented,
};
pub use select::{select, Select};
pub use setting_row::{setting_row, SettingRow};
pub use surface::{card, panel};
pub use text::{body, caption, label, mono, section, title};
pub use toggle::{toggle, Toggle, ToggleForm};

pub use crate::theme::tenshu::button::ActionKind;
pub use crate::theme::tenshu::Tone;
