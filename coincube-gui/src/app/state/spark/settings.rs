//! Spark Settings: the Balances & conversion and Connections tabs.
//!
//! Balances & conversion holds:
//! - A "Stable Balance" toggle (Phase 6) — enables or disables the
//!   Spark SDK's USD-pegged balance feature via
//!   `update_user_settings(stable_balance_active_label)`.
//! - The Spark nickname (master plan §3 decision 6), which the App saves
//!   to this Cube's settings as [`CubeSettings::spark_alias`].
//!
//! Connections shows whether the last `get_info` round-trip to the
//! `coincube-spark-bridge` subprocess succeeded.
//!
//! The Lightning address tab is rendered at the App level
//! ([`crate::app::view::spark::settings::lightning_address`]) because
//! it needs a borrow of `ConnectCubePanel` that this panel can't
//! receive through the `State::view` trait signature.
//!
//! [`CubeSettings::spark_alias`]: crate::app::settings::CubeSettings::spark_alias

use std::sync::Arc;

use coincube_spark_protocol::GetUserSettingsOk;
use coincube_ui::widget::{Column, Element};
use iced::{Length, Task};

use crate::app::cache::Cache;
use crate::app::menu::Menu;
use crate::app::message::Message;
use crate::app::state::State;
use crate::app::view::spark::{SparkSettingsStatus, SparkSettingsView};
use crate::app::wallets::SparkBackend;
use crate::services::connect::client::backend::api::WALLET_ALIAS_MAXIMUM_LENGTH;

pub struct SparkSettings {
    backend: Option<Arc<SparkBackend>>,
    /// Coarse bridge-reachability state. Reflects whatever the most
    /// recent `get_info` call returned.
    status: SparkSettingsStatus,
    /// Phase 6: latest Stable Balance state read from the bridge.
    /// `None` until the first `get_user_settings` round-trip
    /// completes; the view renders the toggle in a "loading"
    /// state while it's `None`. Updated optimistically when the
    /// user flips the toggle, then reconciled on
    /// `StableBalanceSaved`.
    stable_balance_active: Option<bool>,
    /// Phase 6: `true` while a `set_stable_balance` RPC is in
    /// flight — the toggle is disabled in that window.
    stable_balance_saving: bool,
    /// A Spark nickname typed but not saved yet. `None` shows the saved one.
    nickname_input: Option<String>,
}

impl SparkSettings {
    pub fn new(backend: Option<Arc<SparkBackend>>) -> Self {
        let status = if backend.is_some() {
            SparkSettingsStatus::Loading
        } else {
            SparkSettingsStatus::Unavailable
        };
        Self {
            backend,
            status,
            stable_balance_active: None,
            stable_balance_saving: false,
            nickname_input: None,
        }
    }
}

/// Whether a typed Spark nickname is short enough to save: fewer than
/// [`WALLET_ALIAS_MAXIMUM_LENGTH`] characters once trimmed. The Vault alias
/// uses the same number (from the Connect API module) but counts bytes,
/// untrimmed; the Spark nickname stays on this device and counts what the
/// user sees.
pub fn nickname_fits(input: &str) -> bool {
    input.trim().chars().count() < WALLET_ALIAS_MAXIMUM_LENGTH
}

/// A typed Spark nickname as saved: trimmed, and `None` when blank, which
/// shows plain "Spark".
pub fn saved_nickname(input: &str) -> Option<String> {
    Some(input.trim())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

impl SparkSettings {
    /// A typed nickname that would change the saved one (`saved`), to save
    /// when the user leaves Balances & conversion without pressing Return.
    /// One too long to save stays in the field with its error instead.
    pub fn pending_nickname(&self, saved: Option<&str>) -> Option<String> {
        self.nickname_input
            .as_deref()
            .filter(|input| nickname_fits(input))
            .filter(|input| saved_nickname(input).as_deref() != saved)
            .map(str::to_owned)
    }
}

impl State for SparkSettings {
    fn view<'a>(
        &'a self,
        menu: &'a Menu,
        cache: &'a Cache,
    ) -> Element<'a, crate::app::view::Message> {
        let view = SparkSettingsView {
            status: self.status.clone(),
            stable_balance_active: self.stable_balance_active,
            stable_balance_saving: self.stable_balance_saving,
        };
        let content = if matches!(
            menu,
            Menu::Spark(crate::app::menu::SparkSubMenu::Settings(Some(
                crate::app::menu::SparkSettingsOption::Connections
            )))
        ) {
            view.render_connections(cache)
        } else {
            Column::new()
                .width(Length::Fill)
                .push(view.render())
                .push(crate::app::view::spark::settings::nickname_group(
                    self.nickname_input.as_deref(),
                    cache.wallet_names.spark.as_deref(),
                ))
                .into()
        };
        crate::app::view::dashboard(menu, cache, content)
    }

    fn reload(
        &mut self,
        _daemon: Option<Arc<dyn crate::daemon::Daemon + Sync + Send>>,
        _wallet: Option<Arc<crate::app::wallet::Wallet>>,
    ) -> Task<Message> {
        let Some(backend) = self.backend.clone() else {
            self.status = SparkSettingsStatus::Unavailable;
            return Task::none();
        };
        self.status = SparkSettingsStatus::Loading;
        // `get_info` is the liveness probe: a success means the bridge
        // subprocess is up and the SDK is past init. `get_user_settings`
        // is fetched in parallel so the Stable Balance toggle reflects
        // the real SDK state.
        let info_task = Task::perform(
            {
                let backend = backend.clone();
                async move { backend.get_info().await }
            },
            |result| match result {
                Ok(_) => Message::View(crate::app::view::Message::SparkSettings(
                    crate::app::view::SparkSettingsMessage::BridgeReachable,
                )),
                Err(e) => Message::View(crate::app::view::Message::SparkSettings(
                    crate::app::view::SparkSettingsMessage::BridgeError(e.to_string()),
                )),
            },
        );
        let settings_task = Task::perform(
            async move { backend.get_user_settings().await },
            |result: Result<GetUserSettingsOk, _>| match result {
                Ok(settings) => Message::View(crate::app::view::Message::SparkSettings(
                    crate::app::view::SparkSettingsMessage::UserSettingsLoaded(settings),
                )),
                Err(e) => {
                    tracing::warn!("get_user_settings failed: {}", e);
                    Message::View(crate::app::view::Message::SparkSettings(
                        crate::app::view::SparkSettingsMessage::UserSettingsLoaded(
                            GetUserSettingsOk {
                                stable_balance_active: false,
                                private_mode_enabled: false,
                            },
                        ),
                    ))
                }
            },
        );
        Task::batch(vec![info_task, settings_task])
    }

    fn update(
        &mut self,
        _daemon: Option<Arc<dyn crate::daemon::Daemon + Sync + Send>>,
        _cache: &Cache,
        message: Message,
    ) -> Task<Message> {
        if let Message::View(crate::app::view::Message::SparkSettings(msg)) = message {
            match msg {
                crate::app::view::SparkSettingsMessage::BridgeReachable => {
                    self.status = SparkSettingsStatus::Connected;
                }
                crate::app::view::SparkSettingsMessage::BridgeError(err) => {
                    self.status = SparkSettingsStatus::Error(err);
                }
                crate::app::view::SparkSettingsMessage::UserSettingsLoaded(settings) => {
                    self.stable_balance_active = Some(settings.stable_balance_active);
                }
                crate::app::view::SparkSettingsMessage::StableBalanceToggled(enabled) => {
                    let Some(backend) = self.backend.clone() else {
                        return Task::none();
                    };
                    // Optimistic update — snap the toggle immediately so
                    // the UI feels responsive. StableBalanceSaved
                    // reconciles if the RPC fails.
                    self.stable_balance_active = Some(enabled);
                    self.stable_balance_saving = true;
                    return Task::perform(
                        async move { backend.set_stable_balance(enabled).await },
                        move |result| match result {
                            Ok(()) => Message::View(crate::app::view::Message::SparkSettings(
                                crate::app::view::SparkSettingsMessage::StableBalanceSaved(Ok(
                                    enabled,
                                )),
                            )),
                            Err(e) => Message::View(crate::app::view::Message::SparkSettings(
                                crate::app::view::SparkSettingsMessage::StableBalanceSaved(Err(
                                    e.to_string()
                                )),
                            )),
                        },
                    );
                }
                crate::app::view::SparkSettingsMessage::StableBalanceSaved(result) => {
                    self.stable_balance_saving = false;
                    match result {
                        Ok(enabled) => {
                            self.stable_balance_active = Some(enabled);
                        }
                        Err(err) => {
                            tracing::warn!("set_stable_balance failed: {}", err);
                            if let Some(current) = self.stable_balance_active {
                                self.stable_balance_active = Some(!current);
                            }
                            self.status = SparkSettingsStatus::Error(err);
                        }
                    }
                }
                crate::app::view::SparkSettingsMessage::NicknameEdited(input) => {
                    self.nickname_input = Some(input);
                }
                // The App saves it.
                crate::app::view::SparkSettingsMessage::NicknameSubmitted(_) => {}
                // Keep a name typed while the save was in flight.
                crate::app::view::SparkSettingsMessage::NicknameSaved(saved) => {
                    if self
                        .nickname_input
                        .as_deref()
                        .is_none_or(|input| saved_nickname(input) == saved)
                    {
                        self.nickname_input = None;
                    }
                }
            }
        }
        Task::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::State;
    use crate::app::view::{Message as ViewMessage, SparkSettingsMessage};

    fn settings(stable_balance_active: bool, private_mode_enabled: bool) -> GetUserSettingsOk {
        GetUserSettingsOk {
            stable_balance_active,
            private_mode_enabled,
        }
    }

    fn update(panel: &mut SparkSettings, msg: SparkSettingsMessage) {
        let _ = State::update(
            panel,
            None,
            &Cache::default(),
            Message::View(ViewMessage::SparkSettings(msg)),
        );
    }

    #[test]
    fn new_panel_without_backend_is_unavailable() {
        let panel = SparkSettings::new(None);

        assert!(panel.backend.is_none());
        assert!(matches!(panel.status, SparkSettingsStatus::Unavailable));
        assert_eq!(panel.stable_balance_active, None);
        assert!(!panel.stable_balance_saving);
    }

    #[test]
    fn reload_without_backend_marks_unavailable() {
        let mut panel = SparkSettings::new(None);
        panel.status = SparkSettingsStatus::Connected;

        let _ = State::reload(&mut panel, None, None);

        assert!(matches!(panel.status, SparkSettingsStatus::Unavailable));
    }

    #[test]
    fn bridge_status_messages_update_status_card_state() {
        let mut panel = SparkSettings::new(None);

        update(&mut panel, SparkSettingsMessage::BridgeReachable);
        assert!(matches!(panel.status, SparkSettingsStatus::Connected));

        update(
            &mut panel,
            SparkSettingsMessage::BridgeError("bridge exited".to_string()),
        );
        assert!(matches!(
            panel.status,
            SparkSettingsStatus::Error(ref err) if err == "bridge exited"
        ));
    }

    #[test]
    fn user_settings_loaded_updates_stable_balance_flag() {
        let mut panel = SparkSettings::new(None);

        update(
            &mut panel,
            SparkSettingsMessage::UserSettingsLoaded(settings(true, true)),
        );
        assert_eq!(panel.stable_balance_active, Some(true));

        update(
            &mut panel,
            SparkSettingsMessage::UserSettingsLoaded(settings(false, false)),
        );
        assert_eq!(panel.stable_balance_active, Some(false));
    }

    #[test]
    fn stable_balance_toggle_without_backend_is_ignored() {
        let mut panel = SparkSettings::new(None);
        panel.stable_balance_active = Some(false);

        update(&mut panel, SparkSettingsMessage::StableBalanceToggled(true));

        assert_eq!(panel.stable_balance_active, Some(false));
        assert!(!panel.stable_balance_saving);
    }

    #[test]
    fn stable_balance_save_success_commits_state_and_clears_saving() {
        let mut panel = SparkSettings::new(None);
        panel.stable_balance_active = Some(false);
        panel.stable_balance_saving = true;

        update(
            &mut panel,
            SparkSettingsMessage::StableBalanceSaved(Ok(true)),
        );

        assert_eq!(panel.stable_balance_active, Some(true));
        assert!(!panel.stable_balance_saving);
    }

    #[test]
    fn nickname_is_trimmed_blank_is_plain_spark_and_long_is_refused() {
        assert_eq!(
            saved_nickname("  Daily spending \n"),
            Some("Daily spending".to_owned())
        );
        assert_eq!(saved_nickname("   "), None);
        assert!(nickname_fits(&"a".repeat(WALLET_ALIAS_MAXIMUM_LENGTH - 1)));
        assert!(!nickname_fits(&"a".repeat(WALLET_ALIAS_MAXIMUM_LENGTH)));
        // Characters, not bytes: 63 two-byte letters still fit.
        assert!(nickname_fits(&"é".repeat(WALLET_ALIAS_MAXIMUM_LENGTH - 1)));
    }

    #[test]
    fn a_typed_nickname_is_pending_only_while_it_would_change_the_saved_one() {
        let mut panel = SparkSettings::new(None);
        assert_eq!(panel.pending_nickname(None), None);

        update(
            &mut panel,
            SparkSettingsMessage::NicknameEdited(" Daily spending ".to_owned()),
        );
        assert_eq!(
            panel.pending_nickname(None).as_deref(),
            Some(" Daily spending ")
        );
        assert_eq!(panel.pending_nickname(Some("Daily spending")), None);

        // Clearing the field to go back to plain "Spark" is a change too.
        update(
            &mut panel,
            SparkSettingsMessage::NicknameEdited(String::new()),
        );
        assert_eq!(
            panel.pending_nickname(Some("Daily spending")).as_deref(),
            Some("")
        );
        assert_eq!(panel.pending_nickname(None), None);

        update(
            &mut panel,
            SparkSettingsMessage::NicknameEdited("a".repeat(WALLET_ALIAS_MAXIMUM_LENGTH)),
        );
        assert_eq!(panel.pending_nickname(None), None);
    }

    #[test]
    fn a_save_clears_the_field_unless_another_name_was_typed_meanwhile() {
        let mut panel = SparkSettings::new(None);
        update(
            &mut panel,
            SparkSettingsMessage::NicknameEdited("Daily".to_owned()),
        );
        update(
            &mut panel,
            SparkSettingsMessage::NicknameSaved(Some("Daily".to_owned())),
        );
        assert_eq!(panel.nickname_input, None);

        // Saved "Daily", but the user has typed on to "Daily spending".
        update(
            &mut panel,
            SparkSettingsMessage::NicknameEdited("Daily spending".to_owned()),
        );
        update(
            &mut panel,
            SparkSettingsMessage::NicknameSaved(Some("Daily".to_owned())),
        );
        assert_eq!(panel.nickname_input.as_deref(), Some("Daily spending"));

        update(
            &mut panel,
            SparkSettingsMessage::NicknameEdited("  ".to_owned()),
        );
        update(&mut panel, SparkSettingsMessage::NicknameSaved(None));
        assert_eq!(panel.nickname_input, None);
    }

    #[test]
    fn stable_balance_save_error_rolls_back_optimistic_state() {
        let mut panel = SparkSettings::new(None);
        panel.stable_balance_active = Some(true);
        panel.stable_balance_saving = true;

        update(
            &mut panel,
            SparkSettingsMessage::StableBalanceSaved(Err("sdk rejected toggle".to_string())),
        );

        assert_eq!(panel.stable_balance_active, Some(false));
        assert!(!panel.stable_balance_saving);
        assert!(matches!(
            panel.status,
            SparkSettingsStatus::Error(ref err) if err == "sdk rejected toggle"
        ));
    }
}
