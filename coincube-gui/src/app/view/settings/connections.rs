//! Cube › Settings › Connections: the Connect account, this device, and
//! Troubleshooting with Re-register. The Vault and Spark Connections tabs
//! show their own health and link here with [`cube_connections_row`].

use iced::Length;

use coincube_ui::widget::{Column, Element};

use crate::app::cache::Cache;
use crate::app::menu::{CubeSettingsOption, CubeSubMenu, Menu};
use crate::app::state::settings::connections::{ConnectionsSettingsState, ReregisterOutcome};
use crate::app::view::dashboard;
use crate::app::view::message::{Message, SettingsMessage};
use crate::app::view::settings::page;
use crate::app::ConnectionStatus;

pub const MANAGE_CONNECT_ID: &str = "tenshu.settings.cube-connections.connect";
pub const COPY_DEVICE_ID: &str = "tenshu.settings.cube-connections.copy-device";
pub const REREGISTER_ID: &str = "tenshu.settings.cube-connections.reregister";

/// Where the Vault and Spark Connections tabs send the reader.
pub const CUBE_CONNECTIONS_ROUTE: Menu =
    Menu::Cube(CubeSubMenu::Settings(CubeSettingsOption::Connections));

pub const REREGISTER_COPY: &str = "Use if this Cube stops receiving updates from Connect, for \
     example after it was revoked. It renews this device's registration only; wallets and keys \
     are not touched. The device ID changes only if Connect had revoked it.";

pub fn connections_section<'a>(
    menu: &'a Menu,
    cache: &'a Cache,
    state: &'a ConnectionsSettingsState,
) -> Element<'a, Message> {
    dashboard(
        menu,
        cache,
        connections_content(cache, state.reregistering, state.reregister_status.as_ref()),
    )
}

/// The realtime stream's status and the line under it.
fn stream_status(cache: &Cache) -> (page::Status, &'static str, String) {
    if !cache.connect_authenticated {
        return (
            page::Status::Unknown,
            "Not checked",
            "Checked after sign-in.".to_owned(),
        );
    }
    match &cache.connect_stream_status {
        ConnectionStatus::Connected => (
            page::Status::Ok,
            "Connected",
            if cache.current_cube_server_id.is_some() {
                "Updates from Connect arrive as they happen.".to_owned()
            } else {
                "Updates from Connect arrive as they happen. This Cube is not registered with \
                 Connect yet."
                    .to_owned()
            },
        ),
        ConnectionStatus::DeviceRejected => (
            page::Status::Bad,
            "Device not recognized",
            crate::app::DEVICE_REJECTED_HINT.to_owned(),
        ),
        ConnectionStatus::Error(e) => (page::Status::Bad, "Reconnect required", e.clone()),
        ConnectionStatus::Connecting | ConnectionStatus::Inactive => (
            page::Status::Busy,
            "Connecting…",
            "Updates from Connect arrive once the stream is open.".to_owned(),
        ),
    }
}

pub fn connections_content<'a>(
    cache: &'a Cache,
    reregistering: bool,
    outcome: Option<&ReregisterOutcome>,
) -> Element<'a, Message> {
    let signed_in = cache.connect_authenticated;
    let mut account = page::row("Account")
        .accessory(if signed_in {
            page::status(page::Status::Ok, "Signed in")
        } else {
            page::status(page::Status::Unknown, "Signed out")
        })
        .control(page::link(
            MANAGE_CONNECT_ID,
            if signed_in {
                "Manage in Connect"
            } else {
                "Sign in"
            },
            if signed_in {
                Message::OpenConnectSwitchAccount
            } else {
                Message::OpenConnectSignIn
            },
        ));
    if let Some(email) = cache.connect_email.as_deref().filter(|_| signed_in) {
        account = account.line(page::muted(email));
    }

    let mut device: Vec<Element<'a, Message>> = Vec::new();
    match cache.connect_device_id.as_deref() {
        Some(id) => device.push(
            page::row("Device ID")
                .line(page::muted(id).font(coincube_ui::font::MONO_REGULAR))
                .control(page::button(
                    COPY_DEVICE_ID,
                    "Copy",
                    Some(Message::Clipboard(id.to_owned())),
                ))
                .into(),
        ),
        None => device.push(
            page::row("Device ID")
                .description("Assigned when this device registers with Connect after sign-in.")
                .into(),
        ),
    }
    let (kind, word, detail) = stream_status(cache);
    device.push(
        page::row("Realtime stream")
            .accessory(page::status(kind, word))
            .description(detail)
            .into(),
    );

    let mut content = Column::new()
        .width(Length::Fill)
        .push(page::group(
            "Connect account",
            None,
            page::rows([account.into()]),
        ))
        .push(page::group("This device", None, page::rows(device)));

    // Re-register needs a registration to renew.
    if cache.connect_device_id.is_some() {
        let mut reregister = page::row("Re-register this device")
            .description(REREGISTER_COPY)
            .control(page::button(
                REREGISTER_ID,
                if reregistering {
                    "Re-registering…"
                } else {
                    "Re-register"
                },
                (!reregistering)
                    .then_some(Message::Settings(SettingsMessage::ReregisterConnectDevice)),
            ))
            .top();
        if let Some(outcome) = outcome {
            let line = outcome.line();
            reregister = reregister.line(match outcome {
                ReregisterOutcome::Done { .. } => page::status(page::Status::Ok, line),
                ReregisterOutcome::Failed(_) => page::status(page::Status::Bad, line),
            });
        }
        content = content.push(page::group(
            "Troubleshooting",
            None,
            page::rows([reregister.into()]),
        ));
    }
    content.into()
}

/// The one row the Vault and Spark Connections tabs keep for the Cube's
/// Connect session: its status and a way to Cube › Connections.
pub fn cube_connections_row<'a>(cache: &Cache, id: &'static str) -> Element<'a, Message> {
    {
        let (kind, word) = if !cache.connect_authenticated {
            (page::Status::Unknown, "Signed out")
        } else {
            match &cache.connect_stream_status {
                ConnectionStatus::Connected => (page::Status::Ok, "Connected"),
                ConnectionStatus::DeviceRejected => (page::Status::Bad, "Device not recognized"),
                ConnectionStatus::Error(_) => (page::Status::Bad, "Reconnect required"),
                ConnectionStatus::Connecting | ConnectionStatus::Inactive => {
                    (page::Status::Busy, "Connecting…")
                }
            }
        };
        page::row("Connect account and this device")
            .accessory(page::status(kind, word))
            .description("Shared by every wallet in this Cube.")
            .control(page::button(
                id,
                "Cube connections",
                Some(Message::Menu(CUBE_CONNECTIONS_ROUTE)),
            ))
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::view::shared::landing::capture;

    fn signed_in() -> Cache {
        Cache {
            connect_authenticated: true,
            connect_email: Some("robert@coincube.io".into()),
            connect_device_id: Some("160".into()),
            connect_stream_status: ConnectionStatus::Connected,
            current_cube_server_id: Some(1),
            ..Cache::default()
        }
    }

    fn inspect(content: Element<'_, Message>) -> capture::Inspection {
        capture::inspect(
            "settings-cube-connections",
            content,
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
        )
    }

    #[test]
    fn connections_shows_account_device_and_reregister() {
        let cache = signed_in();
        let probe = inspect(connections_content(&cache, false, None));
        for label in [
            "Connect account",
            "robert@coincube.io",
            "Signed in",
            "Manage in Connect",
            "This device",
            "Device ID",
            "160",
            "Realtime stream",
            "Updates from Connect arrive as they happen.",
            "Troubleshooting",
            "Re-register this device",
            "wallets and keys are not touched",
        ] {
            assert!(probe.contains(label), "{}", label);
        }
        let copied = capture::activate(
            connections_content(&cache, false, None),
            iced::advanced::widget::Id::new(COPY_DEVICE_ID),
            (1440, 900),
            true,
        );
        assert!(copied
            .iter()
            .any(|m| matches!(m, Message::Clipboard(id) if id == "160")));
        let pressed = capture::activate(
            connections_content(&cache, false, None),
            iced::advanced::widget::Id::new(REREGISTER_ID),
            (1440, 900),
            true,
        );
        assert!(pressed.iter().any(|m| matches!(
            m,
            Message::Settings(SettingsMessage::ReregisterConnectDevice)
        )));
    }

    #[test]
    fn the_result_line_says_whether_the_device_id_changed() {
        let cache = signed_in();
        let same = ReregisterOutcome::Done {
            at: "14:02".into(),
            device_id: "160".into(),
            changed: false,
        };
        assert!(inspect(connections_content(&cache, false, Some(&same)))
            .contains("Re-registered at 14:02. Same device ID (160)."));
        let new = ReregisterOutcome::Done {
            at: "14:02".into(),
            device_id: "161".into(),
            changed: true,
        };
        let probe = inspect(connections_content(&cache, false, Some(&new)));
        assert!(probe.contains("Re-registered at 14:02. New device ID: 161."));
        assert!(!probe.contains("Same device ID"));
    }

    #[test]
    fn signed_out_offers_sign_in_and_no_reregister() {
        let cache = Cache::default();
        let probe = inspect(connections_content(&cache, false, None));
        assert!(probe.contains("Signed out") && probe.contains("Sign in"));
        assert!(!probe.contains("Re-register"));
    }

    #[test]
    fn a_rejected_device_says_so_on_the_stream_and_the_link_row() {
        let cache = Cache {
            connect_stream_status: ConnectionStatus::DeviceRejected,
            ..signed_in()
        };
        assert!(inspect(connections_content(&cache, false, None)).contains("Device not recognized"));
        let probe = inspect(cube_connections_row(&cache, "test.link"));
        assert!(probe.contains("Device not recognized"));
        assert!(probe.contains("Shared by every wallet in this Cube."));
        let sent = capture::activate(
            cube_connections_row(&cache, "test.link"),
            iced::advanced::widget::Id::new("test.link"),
            (1440, 900),
            true,
        );
        assert!(sent
            .iter()
            .any(|m| matches!(m, Message::Menu(menu) if *menu == CUBE_CONNECTIONS_ROUTE)));
    }
}
