//! Cube › Settings › Connections: the Connect account, this device's
//! registration and realtime stream, and the Re-register action.

use std::sync::Arc;

use coincube_ui::widget::Element;
use iced::Task;

use crate::app::cache::Cache;
use crate::app::menu::Menu;
use crate::app::message::Message;
use crate::app::state::State;
use crate::app::view;
use crate::app::wallet::Wallet;
use crate::daemon::Daemon;

/// How the last Re-register ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReregisterOutcome {
    /// Connect accepted the registration at local time `at` ("HH:MM").
    /// `RegisterDevice` is idempotent: it returns the same id unless Connect
    /// had revoked the old one, so `changed` says which happened.
    Done {
        at: String,
        device_id: String,
        changed: bool,
    },
    /// The request failed; the old registration stays.
    Failed(String),
}

impl ReregisterOutcome {
    /// The line shown under the Re-register row.
    pub fn line(&self) -> String {
        match self {
            Self::Done {
                at,
                device_id,
                changed: false,
            } => format!("Re-registered at {at}. Same device ID ({device_id})."),
            Self::Done {
                at,
                device_id,
                changed: true,
            } => format!("Re-registered at {at}. New device ID: {device_id}."),
            Self::Failed(e) => format!("Re-registration failed: {e}"),
        }
    }
}

#[derive(Default)]
pub struct ConnectionsSettingsState {
    /// The most recent Re-register result, `None` before any attempt (or
    /// when the result belonged to a sign-in that has since changed).
    pub reregister_status: Option<ReregisterOutcome>,
    /// True while a re-register RPC is in flight. Disables the button
    /// to prevent double-clicks.
    pub reregistering: bool,
}

impl State for ConnectionsSettingsState {
    fn view<'a>(&'a self, menu: &'a Menu, cache: &'a Cache) -> Element<'a, view::Message> {
        crate::app::view::settings::connections::connections_section(menu, cache, self)
    }

    fn update(
        &mut self,
        _daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        cache: &Cache,
        message: Message,
    ) -> Task<Message> {
        match message {
            Message::View(view::Message::Settings(
                view::SettingsMessage::ReregisterConnectDevice,
            )) => {
                if self.reregistering {
                    return Task::none();
                }
                // Read the bits we need to spawn the RPC. Without
                // grpc_url and tokens there's nothing we can do —
                // surface a clear error rather than silently swallow
                // the click.
                let (Some(grpc_url), Some(tokens), Some(email), Some(replaces)) = (
                    cache.connect_grpc_url.clone(),
                    cache.connect_tokens.clone(),
                    cache.connect_email.clone(),
                    cache.connect_device_id.clone(),
                ) else {
                    self.reregister_status = Some(ReregisterOutcome::Failed(
                        "Connect isn't ready yet. Wait a moment, then try again.".to_string(),
                    ));
                    return Task::none();
                };
                let network_dir = cache.datadir_path.network_directory(cache.chain());
                self.reregistering = true;
                self.reregister_status = None;
                let app_version = env!("CARGO_PKG_VERSION").to_string();
                let os_version = std::env::consts::OS.to_string();
                // Match the device label used at first launch (see
                // `services/connect/login.rs::device_name_for_this_host`) so
                // re-register produces a consistent name. Both now call the
                // same helper rather than each keeping their own copy.
                let device_name = crate::utils::device::device_label();
                return Task::perform(
                    async move {
                        let claim = match crate::services::connect::client::cache::begin_device_reregistration(
                            &network_dir, &email, &replaces,
                        ).await {
                            Ok(Some(claim)) => claim,
                            Ok(None) => return Err("Connect registration changed. Refresh this page and try again.".to_string()),
                            Err(e) => {
                                tracing::warn!("Re-register: failed to claim cached registration: {}", e);
                                return Err("Couldn't reset this device's registration. \
                                    Check that Tenshu can write to its data folder, then try again."
                                    .to_string());
                            }
                        };
                        let result =
                            crate::services::connect::grpc::bootstrap::register_device_for_claim(
                                crate::services::connect::grpc::bootstrap::DeviceRegistration {
                                    grpc_url: &grpc_url,
                                    tokens,
                                    network_dir: &network_dir,
                                    email: &email,
                                    device_name,
                                    app_version,
                                    os_version,
                                },
                                &claim,
                            )
                            .await
                            .map(|device_id| view::ReregisteredDevice {
                                email: email.clone(),
                                replaces: replaces.clone(),
                                device_id,
                            })
                            .map_err(|e| e.to_string());
                        // Release this request's claim on failure. The old id
                        // remained cached; a newer login keeps its own id.
                        if result.is_err() {
                            if let Err(e) =
                                crate::services::connect::client::cache::finish_device_reregistration(
                                    &network_dir,
                                    &email,
                                    &claim,
                                    Some(&replaces),
                                )
                                .await
                            {
                                tracing::warn!(
                                    "Re-register: failed to release registration claim: {}",
                                    e,
                                );
                            }
                        }
                        result
                    },
                    |r| {
                        Message::View(view::Message::Settings(
                            view::SettingsMessage::ConnectDeviceReregistered(r),
                        ))
                    },
                );
            }
            Message::View(view::Message::Settings(
                view::SettingsMessage::ConnectDeviceReregistered(res),
            )) => {
                self.reregistering = false;
                let at = chrono::Local::now().format("%H:%M").to_string();
                self.reregister_status = settled(cache, res, at);
            }
            _ => {}
        }

        Task::none()
    }

    fn reload(
        &mut self,
        _daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        _wallet: Option<Arc<Wallet>>,
    ) -> Task<Message> {
        Task::none()
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
}

/// The outcome to show for a settled Re-register. A success only shows while
/// the cache already carries the returned id for the same account (the App
/// adopts it first); one for another sign-in shows nothing.
fn settled(
    cache: &Cache,
    res: Result<view::ReregisteredDevice, String>,
    at: String,
) -> Option<ReregisterOutcome> {
    match res {
        Ok(registered)
            if cache.connect_email.as_deref() == Some(registered.email.as_str())
                && cache.connect_device_id.as_deref() == Some(registered.device_id.as_str()) =>
        {
            Some(ReregisterOutcome::Done {
                changed: registered.device_id != registered.replaces,
                device_id: registered.device_id,
                at,
            })
        }
        Ok(_) => None,
        Err(e) => Some(ReregisterOutcome::Failed(e)),
    }
}

impl From<ConnectionsSettingsState> for Box<dyn State> {
    fn from(s: ConnectionsSettingsState) -> Box<dyn State> {
        Box::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered(replaces: &str, device_id: &str) -> view::ReregisteredDevice {
        view::ReregisteredDevice {
            email: "robert@example.invalid".into(),
            replaces: replaces.into(),
            device_id: device_id.into(),
        }
    }

    fn cache(device_id: &str) -> Cache {
        Cache {
            connect_email: Some("robert@example.invalid".into()),
            connect_device_id: Some(device_id.into()),
            ..Cache::default()
        }
    }

    #[test]
    fn an_unchanged_id_says_same_device_id() {
        let outcome = settled(&cache("160"), Ok(registered("160", "160")), "14:02".into());
        assert_eq!(
            outcome.as_ref().map(ReregisterOutcome::line).as_deref(),
            Some("Re-registered at 14:02. Same device ID (160).")
        );
    }

    #[test]
    fn a_revoked_id_says_new_device_id() {
        // The App has adopted the new id into the cache by now.
        let outcome = settled(&cache("161"), Ok(registered("160", "161")), "09:30".into());
        assert_eq!(
            outcome.as_ref().map(ReregisterOutcome::line).as_deref(),
            Some("Re-registered at 09:30. New device ID: 161.")
        );
    }

    #[test]
    fn a_result_for_another_sign_in_shows_nothing_and_errors_show() {
        assert_eq!(
            settled(&cache("170"), Ok(registered("160", "161")), "09:30".into()),
            None
        );
        assert_eq!(
            settled(&cache("160"), Err("offline".into()), "09:30".into())
                .map(|o| o.line())
                .as_deref(),
            Some("Re-registration failed: offline")
        );
    }
}
