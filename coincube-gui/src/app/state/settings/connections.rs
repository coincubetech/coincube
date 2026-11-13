//! Cube › Settings › Connections: the Connect account and this computer's
//! live updates. The page has no state of its own; renewing this computer's
//! registration belongs to the App ([`renewal_task`]), which runs it on its
//! own when Connect stops recognizing the computer and again on Try again.

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

#[derive(Default)]
pub struct ConnectionsSettingsState {
    /// Details for support (the device ID) is open under the notice that
    /// automatic renewal failed.
    pub support_open: bool,
}

impl State for ConnectionsSettingsState {
    fn view<'a>(&'a self, menu: &'a Menu, cache: &'a Cache) -> Element<'a, view::Message> {
        crate::app::view::settings::connections::connections_section(menu, cache, self)
    }

    fn update(
        &mut self,
        _daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        _cache: &Cache,
        message: Message,
    ) -> Task<Message> {
        if let Message::View(view::Message::Settings(
            view::SettingsMessage::ToggleConnectSupportDetails,
        )) = message
        {
            self.support_open = !self.support_open;
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

impl From<ConnectionsSettingsState> for Box<dyn State> {
    fn from(s: ConnectionsSettingsState) -> Box<dyn State> {
        Box::new(s)
    }
}

/// Registers this computer again with its stored transport key and settles
/// as `SettingsMessage::ConnectDeviceReregistered`. `RegisterDevice` is
/// idempotent: it returns the same id unless Connect no longer had it.
///
/// `None` when Connect isn't set up enough to try (no gRPC URL, sign-in or
/// cached registration); the caller treats that as a failed attempt.
pub fn renewal_task(cache: &Cache) -> Option<Task<Message>> {
    let (Some(grpc_url), Some(tokens), Some(email), Some(replaces)) = (
        cache.connect_grpc_url.clone(),
        cache.connect_tokens.clone(),
        cache.connect_email.clone(),
        cache.connect_device_id.clone(),
    ) else {
        return None;
    };
    let network_dir = cache.datadir_path.network_directory(cache.chain());
    let app_version = env!("CARGO_PKG_VERSION").to_string();
    let os_version = std::env::consts::OS.to_string();
    // The label used at first launch, so a renewal keeps the same name.
    let device_name = crate::utils::device::device_label();
    Some(Task::perform(
        async move {
            let claim = match crate::services::connect::client::cache::begin_device_reregistration(
                &network_dir,
                &email,
                &replaces,
            )
            .await
            {
                Ok(Some(claim)) => claim,
                Ok(None) => return Err("Connect registration changed".to_string()),
                Err(e) => {
                    tracing::warn!("Renewal: failed to claim cached registration: {}", e);
                    return Err("couldn't reset the cached registration".to_string());
                }
            };
            let result = crate::services::connect::grpc::bootstrap::register_device_for_claim(
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
            // Release this request's claim on failure. The old id remained
            // cached; a newer login keeps its own id.
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
                    tracing::warn!("Renewal: failed to release registration claim: {}", e);
                }
            }
            result
        },
        |r| {
            Message::View(view::Message::Settings(
                view::SettingsMessage::ConnectDeviceReregistered(r),
            ))
        },
    ))
}
