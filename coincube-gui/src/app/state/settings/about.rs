use std::sync::Arc;

use coincube_ui::widget::Element;
use iced::Task;

use crate::app::cache::Cache;
use crate::app::menu::Menu;
use crate::app::message::Message;
use crate::app::state::State;
use crate::app::view;
use crate::app::wallet::Wallet;
use crate::daemon::{Daemon, DaemonBackend};

#[derive(Default)]
pub struct AboutSettingsState {
    daemon_version: Option<String>,
    /// Status of the most recent "Re-register this device" click.
    /// Drives the inline banner under the Connect device card. `None`
    /// means "no recent attempt"; `Ok(_)` shows a green confirmation
    /// with the new device id; `Err(_)` shows the API error so the
    /// user can decide whether to retry.
    pub reregister_status: Option<Result<String, String>>,
    /// True while a re-register RPC is in flight. Disables the button
    /// to prevent double-clicks.
    pub reregistering: bool,
}

impl State for AboutSettingsState {
    fn view<'a>(&'a self, menu: &'a Menu, cache: &'a Cache) -> Element<'a, view::Message> {
        crate::app::view::settings::about::about_section(
            menu,
            cache,
            self.daemon_version.as_ref(),
            self,
        )
    }

    fn update(
        &mut self,
        daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        cache: &Cache,
        message: Message,
    ) -> Task<Message> {
        match message {
            Message::Info(res) => match res {
                Ok(info) => {
                    if let Some(daemon) = daemon {
                        if daemon.backend() == DaemonBackend::RemoteBackend {
                            self.daemon_version = None;
                        } else {
                            self.daemon_version = Some(info.version)
                        }
                    } else {
                        self.daemon_version = None;
                    }
                }
                Err(e) => {
                    return Task::done(Message::View(view::Message::ShowError(
                        crate::user_error::report(&e),
                    )));
                }
            },
            Message::View(view::Message::Settings(
                view::SettingsMessage::ReregisterConnectDevice,
            )) => {
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
                    self.reregister_status = Some(Err(
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
                self.reregister_status = match res {
                    Ok(registered)
                        if cache.connect_email.as_deref() == Some(registered.email.as_str())
                            && cache.connect_device_id.as_deref()
                                == Some(registered.device_id.as_str()) =>
                    {
                        Some(Ok(registered.device_id))
                    }
                    Ok(_) => None,
                    Err(e) => Some(Err(e)),
                };
            }
            _ => {}
        }

        Task::none()
    }

    fn reload(
        &mut self,
        daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        _wallet: Option<Arc<Wallet>>,
    ) -> Task<Message> {
        if let Some(daemon) = daemon {
            Task::perform(
                async move { daemon.get_info().await.map_err(|e| e.into()) },
                Message::Info,
            )
        } else {
            Task::none()
        }
    }
}

impl From<AboutSettingsState> for Box<dyn State> {
    fn from(s: AboutSettingsState) -> Box<dyn State> {
        Box::new(s)
    }
}
