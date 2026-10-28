//! View renderer for [`crate::app::state::spark::settings::SparkSettings`].
//!
//! Renders the General sub-page:
//! - A "Stable Balance" toggle card (USD-pegging feature) with a
//!   clear on/off status line and a toggle button. Disabled while
//!   the bridge is unavailable or an `update_user_settings` RPC
//!   is in flight.
//! - A small "Bridge status" diagnostic card showing whether the
//!   Spark bridge subprocess is reachable (`get_info` round-trip
//!   successful on the last reload).
//!
//! The Lightning Address sub-page lives in [`lightning_address`] and
//! is rendered by `App::view` directly so it can pass a borrow of the
//! `ConnectCubePanel` into the form (the `State::view` trait signature
//! doesn't reach Connect state).

pub mod lightning_address;

use coincube_ui::{
    color,
    component::tenshu::toggle,
    widget::{Column, Element, Row},
};
use iced::widget::Space;
use iced::{Alignment, Length};

use crate::app::cache::Cache;
use crate::app::view::settings::page;
use crate::app::view::{Message, SparkSettingsMessage};
use crate::app::ConnectionStatus;

/// Coarse status the panel knows about. Only "Unavailable" needs
/// its own rendering branch; the other variants all render the
/// same content (Stable Balance card + bridge status card) and
/// differ only in what the bridge-status card says.
#[derive(Debug, Clone)]
pub enum SparkSettingsStatus {
    /// No Spark signer or the bridge subprocess failed to spawn.
    Unavailable,
    /// First `get_info` round-trip still in flight.
    Loading,
    /// Last `get_info` call failed.
    Error(String),
    /// Last `get_info` call succeeded — bridge is reachable.
    Connected,
}

pub struct SparkSettingsView {
    pub status: SparkSettingsStatus,
    /// Phase 6: Stable Balance on/off. `None` means the first
    /// `get_user_settings` RPC hasn't returned yet — the toggle
    /// renders as "Loading…" in that state.
    pub stable_balance_active: Option<bool>,
    /// Phase 6: `true` while a `set_stable_balance` RPC is in
    /// flight. Disables the toggle buttons so the user can't queue
    /// a second flip mid-rpc.
    pub stable_balance_saving: bool,
}

pub const STABLE_BALANCE_ID: &str = "tenshu.settings.spark.stable-balance";

impl SparkSettingsView {
    /// The Spark tab's Stable Balance group. The App adds the Lightning
    /// Address group beneath it, because that form needs `ConnectCubePanel`,
    /// which the `State::view` signature can't reach.
    pub fn render<'a>(self) -> Element<'a, Message> {
        if matches!(self.status, SparkSettingsStatus::Unavailable) {
            return page::group(
                "Spark isn't available",
                Some(
                    "Spark is not configured for this Cube, or the bridge subprocess failed to \
                     spawn. Configure a Spark signer on this Cube and restart the app to connect."
                        .into(),
                ),
                Space::new(),
            );
        }
        let bridge_reachable = matches!(self.status, SparkSettingsStatus::Connected);
        stable_balance_group(
            self.stable_balance_active,
            self.stable_balance_saving,
            bridge_reachable,
        )
    }

    /// The Connections tab: the Connect account, the Cube service session and
    /// the Spark bridge, then the bridge diagnostics.
    pub fn render_connections<'a>(self, cache: &'a Cache) -> Element<'a, Message> {
        let account = Row::new()
            .spacing(10)
            .align_y(Alignment::Center)
            .push(page::muted("Connect account"))
            .push(if cache.connect_authenticated {
                page::status(page::Status::Ok, "Signed in")
            } else {
                page::status(page::Status::Unknown, "Signed out")
            })
            .push(Space::new().width(Length::Fill))
            .push(page::link(
                "tenshu.settings.spark.manage-connect",
                if cache.connect_authenticated {
                    "Manage in Connect"
                } else {
                    "Sign in"
                },
                if cache.connect_authenticated {
                    Message::OpenConnectSwitchAccount
                } else {
                    Message::OpenConnectSignIn
                },
            ));
        let session = match (&cache.connect_stream_status, cache.connect_authenticated) {
            (_, false) => page::row("Cube service session")
                .accessory(page::status(page::Status::Unknown, "Not checked"))
                .description("Checked after sign-in."),
            // The realtime stream says nothing about registration; that comes
            // from the Cube's Connect id.
            (ConnectionStatus::Connected, true) => page::row("Cube service session")
                .accessory(page::status(page::Status::Ok, "Connected"))
                .description(if cache.current_cube_server_id.is_some() {
                    "Realtime connection open. This Cube is registered with Connect."
                } else {
                    "Realtime connection open. This Cube is not registered with Connect yet."
                }),
            (ConnectionStatus::DeviceRejected, true) => page::row("Cube service session")
                .accessory(page::status(page::Status::Bad, "Device not recognized"))
                .description(crate::app::DEVICE_REJECTED_HINT)
                .control(page::button(
                    "tenshu.settings.spark.reregister",
                    "Re-register…",
                    Some(Message::Menu(
                        crate::app::view::nav::shell::DEVICE_REJECTED_ROUTE,
                    )),
                )),
            (ConnectionStatus::Error(e), true) => page::row("Cube service session")
                .accessory(page::status(page::Status::Bad, "Reconnect required"))
                .description(e.clone()),
            (_, true) => page::row("Cube service session")
                .accessory(page::status(page::Status::Busy, "Connecting…"))
                .description("Re-authenticates this Cube only. Never rescans."),
        };
        let (_, _, headline, detail) = bridge_status_copy(&self.status);
        let kind = match self.status {
            SparkSettingsStatus::Connected => page::Status::Ok,
            SparkSettingsStatus::Loading => page::Status::Busy,
            SparkSettingsStatus::Error(_) | SparkSettingsStatus::Unavailable => page::Status::Bad,
        };
        let bridge = page::row("Spark bridge")
            .accessory(page::status(kind, headline))
            .description(detail);
        Column::new()
            .width(Length::Fill)
            .push(page::note(
                coincube_ui::component::tenshu::Tone::Muted,
                account,
                None,
            ))
            .push(page::group(
                "Health",
                None,
                page::rows([session.into(), bridge.into()]),
            ))
            .into()
    }
}

fn stable_balance_group<'a>(
    active: Option<bool>,
    saving: bool,
    spark_available: bool,
) -> Element<'a, Message> {
    let toggle_state = stable_balance_toggle_state(active, saving, spark_available);
    let (kind, word) = match (active, spark_available) {
        (_, false) => (page::Status::Bad, "Bridge unavailable"),
        (None, true) => (page::Status::Busy, "Loading…"),
        (Some(true), true) => (page::Status::Ok, "On"),
        (Some(false), true) => (page::Status::Off, "Off"),
    };
    let row = page::row("Stable Balance")
        .accessory(page::status(kind, word))
        .description(
            "Keeps your spendable balance pegged to USD as USDB. Turning it on converts sats \
             above a small threshold to USDB; turning it off converts USDB back to bitcoin. \
             Conversions can retry after a failure and the amount can differ slightly from the \
             estimate. No peg is guaranteed.",
        )
        .control(
            toggle(
                STABLE_BALANCE_ID,
                active.unwrap_or(false),
                Message::FocusAction,
            )
            .on_toggle_maybe(toggle_state.can_toggle.then_some(|on| {
                Message::SparkSettings(SparkSettingsMessage::StableBalanceToggled(on))
            })),
        )
        .top();
    page::group("Stable Balance", None, page::rows([row.into()]))
}

#[derive(Debug, PartialEq, Eq)]
struct StableBalanceToggleState {
    button_label: &'static str,
    next_state: bool,
    can_toggle: bool,
}

fn stable_balance_toggle_state(
    active: Option<bool>,
    saving: bool,
    spark_available: bool,
) -> StableBalanceToggleState {
    let can_toggle = spark_available && active.is_some() && !saving;
    let target = active.unwrap_or(false);
    let (button_label, next_state) = if target {
        ("Turn off", false)
    } else {
        ("Turn on", true)
    };
    StableBalanceToggleState {
        button_label,
        next_state,
        can_toggle,
    }
}

fn bridge_status_copy(
    status: &SparkSettingsStatus,
) -> (&'static str, iced::Color, &'static str, String) {
    match status {
        SparkSettingsStatus::Loading => (
            "●",
            color::GREY_3,
            "Checking bridge…",
            "Waiting for the first get_info round-trip.".to_string(),
        ),
        SparkSettingsStatus::Connected => (
            "✓",
            color::GREEN,
            "Connected",
            "The Spark bridge subprocess is reachable and \
             responding to JSON-RPC requests over stdio."
                .to_string(),
        ),
        SparkSettingsStatus::Error(err) => (
            "✗",
            color::RED,
            "Disconnected",
            format!(
                "The last get_info call failed. Restarting the cube \
                 re-spawns the bridge. Error: {}",
                err
            ),
        ),
        SparkSettingsStatus::Unavailable => (
            "✗",
            color::RED,
            "Unavailable",
            "No Spark signer configured, or the bridge subprocess \
             failed to spawn."
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_balance_toggle_state_only_enables_when_loaded_available_and_idle() {
        assert_eq!(
            stable_balance_toggle_state(Some(true), false, true),
            StableBalanceToggleState {
                button_label: "Turn off",
                next_state: false,
                can_toggle: true,
            }
        );
        assert_eq!(
            stable_balance_toggle_state(Some(false), false, true),
            StableBalanceToggleState {
                button_label: "Turn on",
                next_state: true,
                can_toggle: true,
            }
        );
        assert!(!stable_balance_toggle_state(None, false, true).can_toggle);
        assert!(!stable_balance_toggle_state(Some(true), true, true).can_toggle);
        assert!(!stable_balance_toggle_state(Some(true), false, false).can_toggle);
    }

    #[test]
    fn bridge_status_copy_names_loading_connected_error_and_unavailable() {
        let loading = bridge_status_copy(&SparkSettingsStatus::Loading);
        assert_eq!(loading.0, "●");
        assert_eq!(loading.1, color::GREY_3);
        assert_eq!(loading.2, "Checking bridge…");

        let connected = bridge_status_copy(&SparkSettingsStatus::Connected);
        assert_eq!(connected.0, "✓");
        assert_eq!(connected.1, color::GREEN);
        assert_eq!(connected.2, "Connected");

        let error = bridge_status_copy(&SparkSettingsStatus::Error("boom".to_string()));
        assert_eq!(error.0, "✗");
        assert_eq!(error.1, color::RED);
        assert_eq!(error.2, "Disconnected");
        assert!(error.3.contains("boom"));

        let unavailable = bridge_status_copy(&SparkSettingsStatus::Unavailable);
        assert_eq!(unavailable.0, "✗");
        assert_eq!(unavailable.1, color::RED);
        assert_eq!(unavailable.2, "Unavailable");
    }
}
