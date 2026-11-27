//! Regenerate and Back on Cube Settings › Display, end to end through the
//! App (Cube avatars CD3): the Display row's buttons emit the intent, the App
//! changes the Cube's mark number through Connect when the Cube is registered
//! and signed in, on this device when it never registered, and not at all
//! when it is registered but signed out or belongs to another account.
//!
//! Every Connect call goes to a local `httpmock` server with a catch-all on
//! `/avatar`, so a request to the retired avatar routes fails the test.

use super::*;
use crate::app::cache::{CubeMarkAccess, CubeMarkChange};
use crate::app::view::settings::general::{
    avatar_row, MARK_BACK_ID, MARK_FAILED_LINE, MARK_REGENERATE_ID, MARK_SIGN_IN_LINE,
    MARK_UNCONFIRMED_LINE,
};
use crate::app::view::shared::landing::capture;
use crate::services::coincube::CoincubeClient;
use httpmock::{Method as MockMethod, Mock, MockServer};
use iced::advanced::widget::Id;
use serde_json::json;

const MARK_CUBE: &str = "9fb93534-081c-4253-b1e2-73a87984a8ab";
const SERVER_ID: u64 = 42;
const OUTER: (u32, u32) = (1024, 768);

/// An open Cube with a UUID id, saved in its network's settings file, never
/// registered and signed out.
fn mark_app(root: &std::path::Path) -> App {
    let mut app = super::shell_tests::bitcoin_app_with_id(root, MARK_CUBE);
    app.cache.cube_mark_seed = app.cube_settings.mark_seed();
    app.panels.connect.set_cube_has_server_record(false);
    let network_dir = app.datadir.network_directory(app.cube_settings.network);
    let cube = app.cube_settings.clone();
    run(settings::update_settings_file(
        &network_dir,
        move |mut s| {
            s.cubes.push(cube);
            Some(s)
        },
    ))
    .unwrap();
    app.sync_panel_derived_cache_fields();
    app
}

fn run<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn outputs(task: Task<Message>) -> Vec<Message> {
    use iced_runtime::futures::futures::StreamExt;
    let Some(stream) = iced_runtime::task::into_stream(task) else {
        return Vec::new();
    };
    run(stream
        .filter_map(|action| async move {
            match action {
                iced_runtime::Action::Output(message) => Some(message),
                _ => None,
            }
        })
        .collect())
}

/// Feed `message` to the App and every message its tasks produce, until
/// nothing is left: the set call's result, then the settings write.
fn settle(app: &mut App, message: Message) {
    let mut pending = vec![message];
    while let Some(message) = pending.pop() {
        pending.extend(outputs(app.update(message)));
    }
}

fn saved_mark_variation(app: &App) -> u32 {
    let network_dir = app.datadir.network_directory(app.cube_settings.network);
    settings::Settings::from_file(&network_dir)
        .unwrap()
        .cubes
        .into_iter()
        .find(|cube| cube.id == MARK_CUBE)
        .unwrap()
        .mark_variation
}

/// Sign the App's Connect panel in against `server`, with the Cube
/// registered as `SERVER_ID`.
fn sign_in(app: &mut App, server: &MockServer) {
    let mut client = CoincubeClient::for_test(server.base_url());
    client.set_token("fixture-token");
    app.panels.connect.cube.client = Some(client);
    app.panels.connect.cube.server_cube_id = Some(SERVER_ID);
    app.sync_panel_derived_cache_fields();
}

/// Fails the test on any request to a retired avatar route.
fn avatar_tripwire(server: &MockServer) -> Mock<'_> {
    server.mock(|when, then| {
        when.path_contains("/avatar");
        then.status(410);
    })
}

/// The set call for `variation`, answered with the server's `kept` number.
fn set_mark(server: &MockServer, variation: u32, kept: u32) -> Mock<'_> {
    server.mock(|when, then| {
        when.method(MockMethod::PUT)
            .path(format!("/api/v1/connect/cubes/{}/mark", SERVER_ID))
            .header("authorization", "Bearer fixture-token")
            .json_body(json!({ "markVariation": variation }));
        then.status(200).json_body(json!({
            "success": true,
            "data": { "markVariation": kept, "markVersion": 1 }
        }));
    })
}

/// What pressing the Display row's `id` button emits, by pointer or by
/// keyboard (Enter on the focused button), as App messages. A disabled
/// button takes no focus, so it can be neither tabbed to nor pressed.
fn press(app: &App, id: &'static str, pointer: bool) -> Vec<Message> {
    let focusable = capture::inspect(
        "cube-mark-press",
        avatar_row(&app.cache),
        OUTER,
        "dark",
        coincube_ui::theme::Theme::dark(),
    )
    .has_focus(Id::new(id));
    if !focusable {
        return Vec::new();
    }
    capture::activate(avatar_row(&app.cache), Id::new(id), OUTER, pointer)
        .into_iter()
        .filter(|message| matches!(message, view::Message::CubeMark(_)))
        .map(Message::View)
        .collect()
}

fn temp_root(scenario: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("cube-mark-{}-{}", scenario, uuid::Uuid::new_v4()))
}

/// Signed in to a registered Cube: Regenerate (by pointer) and Back (by
/// keyboard) send the set call with current ± 1, and the local number, seed
/// and settings file follow the server's answer, not the request.
#[test]
fn signed_in_regenerate_and_back_set_the_number_through_connect() {
    let _guard = crate::app::session::test_guard();
    let root = temp_root("connect");
    let server = MockServer::start();
    let tripwire = avatar_tripwire(&server);
    let mut app = mark_app(&root);
    sign_in(&mut app, &server);
    assert_eq!(app.cache.cube_mark_access, CubeMarkAccess::Connect);

    let regenerate = set_mark(&server, 1, 1);
    let pressed = press(&app, MARK_REGENERATE_ID, true);
    assert_eq!(pressed.len(), 1);
    for message in pressed {
        settle(&mut app, message);
    }
    regenerate.assert();
    assert_eq!(app.cube_settings.mark_variation, 1);
    assert_eq!(app.cache.cube_mark_variation, 1);
    assert_eq!(app.panels.connect.cube.mark_variation, 1);
    assert_eq!(
        app.cache.cube_mark_seed.as_deref(),
        Some("9fb93534-081c-4253-b1e2-73a87984a8ab:1")
    );
    assert_eq!(saved_mark_variation(&app), 1);
    assert_eq!(app.cache.cube_mark_change, CubeMarkChange::Idle);

    // The server is the record: it answered 7 to a request for 2.
    let raced = set_mark(&server, 2, 7);
    settle(
        &mut app,
        Message::View(view::Message::CubeMark(view::CubeMarkIntent::Regenerate)),
    );
    raced.assert();
    assert_eq!(app.cube_settings.mark_variation, 7);
    assert_eq!(saved_mark_variation(&app), 7);

    let back = set_mark(&server, 6, 6);
    let pressed = press(&app, MARK_BACK_ID, false);
    assert_eq!(pressed.len(), 1);
    for message in pressed {
        settle(&mut app, message);
    }
    back.assert();
    assert_eq!(app.cube_settings.mark_variation, 6);
    assert_eq!(saved_mark_variation(&app), 6);

    tripwire.assert_hits(0);
    let _ = std::fs::remove_dir_all(&root);
}

/// While the set call is in flight both buttons are disabled and a second
/// press sends nothing.
#[test]
fn both_buttons_are_disabled_while_the_set_call_is_in_flight() {
    let _guard = crate::app::session::test_guard();
    let root = temp_root("in-flight");
    let server = MockServer::start();
    let tripwire = avatar_tripwire(&server);
    let mut app = mark_app(&root);
    sign_in(&mut app, &server);
    app.cube_settings.mark_variation = 3;
    app.cache.cube_mark_variation = 3;

    let first = set_mark(&server, 4, 4);
    let in_flight = app.update(Message::View(view::Message::CubeMark(
        view::CubeMarkIntent::Regenerate,
    )));
    assert!(matches!(
        app.cache.cube_mark_change,
        CubeMarkChange::Saving { .. }
    ));
    assert!(press(&app, MARK_REGENERATE_ID, true).is_empty());
    assert!(press(&app, MARK_BACK_ID, false).is_empty());
    // A press that arrives anyway issues no second request.
    assert!(outputs(app.update(Message::View(view::Message::CubeMark(
        view::CubeMarkIntent::Back
    ))))
    .is_empty());
    for message in outputs(in_flight) {
        settle(&mut app, message);
    }
    first.assert_hits(1);
    assert_eq!(app.cube_settings.mark_variation, 4);
    assert_eq!(app.cache.cube_mark_change, CubeMarkChange::Idle);
    tripwire.assert_hits(0);
    let _ = std::fs::remove_dir_all(&root);
}

/// A failed set call (here Connect's duress refusal) leaves the number, the
/// seed and the file as they were, and the row shows one line saying so.
#[test]
fn a_failed_set_call_changes_nothing_and_says_so() {
    let _guard = crate::app::session::test_guard();
    let root = temp_root("failed");
    let server = MockServer::start();
    let tripwire = avatar_tripwire(&server);
    let mut app = mark_app(&root);
    sign_in(&mut app, &server);
    let seed_before = app.cache.cube_mark_seed.clone();

    for status in [423, 500] {
        let mut refused = server.mock(|when, then| {
            when.method(MockMethod::PUT)
                .path(format!("/api/v1/connect/cubes/{}/mark", SERVER_ID));
            then.status(status).json_body(json!({
                "success": false,
                "error": { "code": "DURESS_ACTIVE", "message": "locked" }
            }));
        });
        settle(
            &mut app,
            Message::View(view::Message::CubeMark(view::CubeMarkIntent::Regenerate)),
        );
        refused.assert();
        refused.delete();
        assert_eq!(app.cube_settings.mark_variation, 0, "status {}", status);
        assert_eq!(app.cache.cube_mark_seed, seed_before, "status {}", status);
        assert_eq!(saved_mark_variation(&app), 0, "status {}", status);
        assert!(matches!(
            app.cache.cube_mark_change,
            CubeMarkChange::Failed { .. }
        ));
        let shown = capture::inspect(
            "cube-mark-failed",
            avatar_row(&app.cache),
            OUTER,
            "dark",
            coincube_ui::theme::Theme::dark(),
        );
        assert!(shown.contains(MARK_FAILED_LINE), "status {}", status);
    }
    tripwire.assert_hits(0);
    let _ = std::fs::remove_dir_all(&root);
}

/// A result from a Connect session that has since ended is dropped: the
/// number stays, and the buttons are released.
#[test]
fn a_set_result_from_an_ended_session_is_dropped() {
    let _guard = crate::app::session::test_guard();
    let root = temp_root("stale");
    let server = MockServer::start();
    let mut app = mark_app(&root);
    sign_in(&mut app, &server);
    let stale = app.panels.connect.cube.session_generation();
    app.cache.cube_mark_change = CubeMarkChange::Saving { generation: stale };
    app.panels.connect.cube.clear_client();
    settle(
        &mut app,
        Message::CubeMarkSet {
            generation: stale,
            result: Ok(9),
        },
    );
    assert_eq!(app.cube_settings.mark_variation, 0);
    assert_eq!(saved_mark_variation(&app), 0);
    assert_eq!(app.cache.cube_mark_change, CubeMarkChange::Idle);
    let _ = std::fs::remove_dir_all(&root);
}

/// A Cube that never registered changes its number on this device, with no
/// request, and its first registration carries the number (API CA1).
#[test]
fn a_never_registered_cube_changes_locally_and_registers_with_its_number() {
    let _guard = crate::app::session::test_guard();
    let root = temp_root("local");
    let server = MockServer::start();
    let tripwire = avatar_tripwire(&server);
    let anything = server.mock(|when, then| {
        when.path_contains("/api/v1/connect/cubes/");
        then.status(500);
    });
    let mut app = mark_app(&root);
    assert_eq!(app.cache.cube_mark_access, CubeMarkAccess::Local);

    for pointer in [true, false] {
        for message in press(&app, MARK_REGENERATE_ID, pointer) {
            settle(&mut app, message);
        }
    }
    assert_eq!(app.cube_settings.mark_variation, 2);
    assert_eq!(app.panels.connect.cube.mark_variation, 2);
    assert_eq!(saved_mark_variation(&app), 2);
    for message in press(&app, MARK_BACK_ID, false) {
        settle(&mut app, message);
    }
    assert_eq!(app.cube_settings.mark_variation, 1);
    assert_eq!(saved_mark_variation(&app), 1);
    for message in press(&app, MARK_REGENERATE_ID, true) {
        settle(&mut app, message);
    }
    assert_eq!(app.cube_settings.mark_variation, 2);
    anything.assert_hits(0);

    // Signing in registers the Cube; the request carries the local number.
    let register = server.mock(|when, then| {
        when.method(MockMethod::POST)
            .path("/api/v1/connect/cubes")
            .json_body_partial(r#"{ "markVariation": 2 }"#);
        then.status(503);
    });
    let mut client = CoincubeClient::for_test(server.base_url());
    client.set_token("fixture-token");
    app.panels.connect.cube.client = Some(client);
    let _ = outputs(app.panels.connect.cube.register_cube());
    register.assert();
    tripwire.assert_hits(0);
    let _ = std::fs::remove_dir_all(&root);
}

/// A registered Cube with no Connect session: both buttons are disabled
/// with the sign-in line, nothing changes, and after sign-in the server's
/// number is what the row draws.
#[test]
fn a_registered_cube_signed_out_waits_for_the_servers_number() {
    let _guard = crate::app::session::test_guard();
    let root = temp_root("signed-out");
    let mut app = mark_app(&root);
    app.cube_settings.remote_synced = true;
    app.panels.connect.set_cube_has_server_record(true);
    app.cube_settings.mark_variation = 1;
    app.cache.cube_mark_variation = 1;
    app.cache.cube_mark_seed = app.cube_settings.mark_seed();
    app.sync_panel_derived_cache_fields();
    assert_eq!(app.cache.cube_mark_access, CubeMarkAccess::SignedOut);

    let shown = capture::inspect(
        "cube-mark-signed-out",
        avatar_row(&app.cache),
        OUTER,
        "dark",
        coincube_ui::theme::Theme::dark(),
    );
    assert!(shown.contains(MARK_SIGN_IN_LINE));
    for id in [MARK_REGENERATE_ID, MARK_BACK_ID] {
        for pointer in [true, false] {
            assert!(press(&app, id, pointer).is_empty(), "{}", id);
        }
    }
    // Even an intent that arrives does nothing.
    settle(
        &mut app,
        Message::View(view::Message::CubeMark(view::CubeMarkIntent::Regenerate)),
    );
    assert_eq!(app.cube_settings.mark_variation, 1);

    // Signing in: the registration response carries the server's number.
    settle(&mut app, Message::CubeMarkFromConnect(Some(5)));
    assert_eq!(app.cube_settings.mark_variation, 5);
    assert_eq!(
        app.cache.cube_mark_seed.as_deref(),
        Some("9fb93534-081c-4253-b1e2-73a87984a8ab:5")
    );
    assert_eq!(saved_mark_variation(&app), 5);
    let server = MockServer::start();
    sign_in(&mut app, &server);
    assert_eq!(app.cache.cube_mark_access, CubeMarkAccess::Connect);
    let shown = capture::inspect(
        "cube-mark-signed-in",
        avatar_row(&app.cache),
        OUTER,
        "dark",
        coincube_ui::theme::Theme::dark(),
    );
    assert!(!shown.contains(MARK_SIGN_IN_LINE));
    assert!(shown.has_focus(Id::new(MARK_BACK_ID)));
    let _ = std::fs::remove_dir_all(&root);
}

/// Another Connect account's Cube shows the mark with no buttons, whether
/// Connect refused its registration or its recorded owner is someone else,
/// and an intent changes nothing. Back is absent at 0 for the owner.
#[test]
fn only_the_owner_gets_the_buttons() {
    let _guard = crate::app::session::test_guard();
    let root = temp_root("owner");
    let server = MockServer::start();
    let tripwire = avatar_tripwire(&server);
    let mut app = mark_app(&root);
    sign_in(&mut app, &server);
    app.cache.connect_email = Some("me@example.invalid".into());

    let shown = capture::inspect(
        "cube-mark-owner-zero",
        avatar_row(&app.cache),
        OUTER,
        "dark",
        coincube_ui::theme::Theme::dark(),
    );
    assert!(shown.has_focus(Id::new(MARK_REGENERATE_ID)));
    assert!(!shown.has_focus(Id::new(MARK_BACK_ID)), "no Back at 0");

    app.cache.cube_connect_owner = Some(settings::ConnectOwner {
        user_id: 9,
        email: "someone@example.invalid".into(),
    });
    app.sync_panel_derived_cache_fields();
    assert_eq!(app.cache.cube_mark_access, CubeMarkAccess::NotOwner);

    app.cache.cube_connect_owner = Some(settings::ConnectOwner {
        user_id: 7,
        email: "ME@example.invalid".into(),
    });
    app.sync_panel_derived_cache_fields();
    assert_eq!(app.cache.cube_mark_access, CubeMarkAccess::Connect);

    app.panels.connect.cube.owned_by_other_account = true;
    app.cube_settings.mark_variation = 2;
    app.cache.cube_mark_variation = 2;
    app.sync_panel_derived_cache_fields();
    assert_eq!(app.cache.cube_mark_access, CubeMarkAccess::NotOwner);
    let shown = capture::inspect(
        "cube-mark-not-owner",
        avatar_row(&app.cache),
        OUTER,
        "dark",
        coincube_ui::theme::Theme::dark(),
    );
    assert!(!shown.has_focus(Id::new(MARK_REGENERATE_ID)));
    assert!(!shown.has_focus(Id::new(MARK_BACK_ID)));
    settle(
        &mut app,
        Message::View(view::Message::CubeMark(view::CubeMarkIntent::Regenerate)),
    );
    assert_eq!(app.cube_settings.mark_variation, 2);
    tripwire.assert_hits(0);
    let _ = std::fs::remove_dir_all(&root);
}

/// A registration in flight holds the buttons, so a local change cannot race
/// the number that request already carries.
#[test]
fn a_registration_in_flight_holds_the_buttons() {
    let _guard = crate::app::session::test_guard();
    let root = temp_root("registering");
    let mut app = mark_app(&root);
    assert!(app
        .panels
        .connect
        .cube
        .begin_external_registration()
        .is_some());
    app.sync_panel_derived_cache_fields();
    assert_eq!(app.cache.cube_mark_access, CubeMarkAccess::Waiting);
    settle(
        &mut app,
        Message::View(view::Message::CubeMark(view::CubeMarkIntent::Regenerate)),
    );
    assert_eq!(app.cube_settings.mark_variation, 0);
    let _ = std::fs::remove_dir_all(&root);
}

/// The saved Cube, read back from its network's settings file.
fn saved_cube(root: &std::path::Path) -> settings::CubeSettings {
    let datadir = CoincubeDirectory::new(root.to_path_buf());
    settings::Settings::from_file(&datadir.network_directory(crate::chain::ChainId::Bitcoin))
        .unwrap()
        .cubes
        .into_iter()
        .find(|cube| cube.id == MARK_CUBE)
        .unwrap()
}

/// Save a Cube with the fixture's id, as Cube creation does.
fn save_cube(root: &std::path::Path) {
    let datadir = CoincubeDirectory::new(root.to_path_buf());
    let network_dir = datadir.network_directory(crate::chain::ChainId::Bitcoin);
    let cube = settings::CubeSettings::new_with_raw_id(
        MARK_CUBE.into(),
        "Fixture".into(),
        crate::chain::ChainId::Bitcoin,
    );
    run(settings::update_settings_file(
        &network_dir,
        move |mut s| {
            s.cubes.push(cube);
            Some(s)
        },
    ))
    .unwrap();
}

/// Open the saved Cube the way the loader does: its settings read from the
/// file and handed to `App::new`.
fn open_saved(root: &std::path::Path) -> App {
    let (mut app, _) = super::shell_tests::bitcoin_app_with_settings(root, saved_cube(root));
    app.sync_panel_derived_cache_fields();
    app
}

/// A registered Cube renamed while signed out (which resets
/// `remote_synced`) and reopened is still registered: Regenerate stays off
/// with the sign-in line rather than offering a local change the next
/// sign-in would overwrite.
#[test]
fn a_registered_cube_renamed_signed_out_reopens_signed_out() {
    let _guard = crate::app::session::test_guard();
    let root = temp_root("rename");
    save_cube(&root);
    let network_dir =
        CoincubeDirectory::new(root.clone()).network_directory(crate::chain::ChainId::Bitcoin);
    run(settings::mark_cube_synced(&network_dir, MARK_CUBE, None)).unwrap();

    let mut app = open_saved(&root);
    assert_eq!(app.cache.cube_mark_access, CubeMarkAccess::SignedOut);
    settle(
        &mut app,
        Message::View(view::Message::Settings(
            view::SettingsMessage::CubeNameSubmitted("Renamed".into()),
        )),
    );
    let renamed = saved_cube(&root);
    assert_eq!(renamed.name, "Renamed");
    assert!(!renamed.remote_synced, "a local rename waits to sync");
    assert!(renamed.connect_registered);

    let reopened = open_saved(&root);
    assert_eq!(reopened.cache.cube_mark_access, CubeMarkAccess::SignedOut);
    let shown = capture::inspect(
        "cube-mark-renamed",
        avatar_row(&reopened.cache),
        OUTER,
        "dark",
        coincube_ui::theme::Theme::dark(),
    );
    assert!(shown.contains(MARK_SIGN_IN_LINE));
    assert!(!shown.has_focus(Id::new(MARK_REGENERATE_ID)));
    let _ = std::fs::remove_dir_all(&root);
}

/// A Cube registered before `connect_registered` existed has only
/// `remote_synced` on disk. Renaming it while signed out must keep it
/// registered, so a reopened Cube still waits for sign-in.
#[test]
fn a_legacy_registered_cube_renamed_signed_out_stays_registered() {
    let _guard = crate::app::session::test_guard();
    let root = temp_root("legacy-rename");
    save_cube(&root);
    let network_dir =
        CoincubeDirectory::new(root.clone()).network_directory(crate::chain::ChainId::Bitcoin);
    run(settings::update_settings_file(&network_dir, |mut s| {
        for cube in s.cubes.iter_mut() {
            cube.remote_synced = true;
        }
        Some(s)
    }))
    .unwrap();
    let legacy = saved_cube(&root);
    assert!(legacy.remote_synced && !legacy.connect_registered);

    let mut app = open_saved(&root);
    assert_eq!(app.cache.cube_mark_access, CubeMarkAccess::SignedOut);
    settle(
        &mut app,
        Message::View(view::Message::Settings(
            view::SettingsMessage::CubeNameSubmitted("Renamed".into()),
        )),
    );
    let renamed = saved_cube(&root);
    assert!(!renamed.remote_synced, "a local rename waits to sync");
    assert!(renamed.connect_registered, "the Connect row is remembered");
    let reopened = open_saved(&root);
    assert_eq!(reopened.cache.cube_mark_access, CubeMarkAccess::SignedOut);
    let _ = std::fs::remove_dir_all(&root);
}

/// A never-registered Cube opened signed out, then registered by the App's
/// Connect panel after sign-in, stays registered after sign-out, and a
/// fresh App opened from the file knows it too.
#[test]
fn an_in_app_registration_survives_sign_out_and_reopening() {
    let _guard = crate::app::session::test_guard();
    let root = temp_root("in-app-registration");
    save_cube(&root);
    let mut app = open_saved(&root);
    assert_eq!(app.cache.cube_mark_access, CubeMarkAccess::Local);

    let server = MockServer::start();
    let tripwire = avatar_tripwire(&server);
    let register = server.mock(|when, then| {
        when.method(MockMethod::POST).path("/api/v1/connect/cubes");
        then.status(200).json_body(json!({
            "success": true,
            "data": {
                "id": SERVER_ID,
                "uuid": MARK_CUBE,
                "name": "Fixture",
                "network": "mainnet",
                "lightningAddress": null,
                "status": "active"
            }
        }));
    });
    let mut client = CoincubeClient::for_test(server.base_url());
    client.set_token("fixture-token");
    app.panels.connect.cube.client = Some(client);
    for message in outputs(app.panels.connect.cube.register_cube()) {
        settle(&mut app, message);
    }
    register.assert();
    assert_eq!(app.panels.connect.cube.server_cube_id, Some(SERVER_ID));
    assert!(saved_cube(&root).connect_registered);
    assert!(!saved_cube(&root).remote_synced);

    app.panels.connect.cube.clear_client();
    app.sync_panel_derived_cache_fields();
    assert_eq!(app.cache.cube_mark_access, CubeMarkAccess::SignedOut);

    let reopened = open_saved(&root);
    assert_eq!(reopened.cache.cube_mark_access, CubeMarkAccess::SignedOut);
    tripwire.assert_hits(0);
    let _ = std::fs::remove_dir_all(&root);
}

/// Signed in to a registered Cube whose registration failed this session:
/// both buttons are off with one line, which does not show while a
/// registration is in flight.
#[test]
fn a_failed_registration_says_connect_has_not_confirmed_the_cube() {
    let _guard = crate::app::session::test_guard();
    let root = temp_root("unconfirmed");
    let server = MockServer::start();
    let mut app = mark_app(&root);
    app.panels.connect.set_cube_has_server_record(true);
    let mut client = CoincubeClient::for_test(server.base_url());
    client.set_token("fixture-token");
    app.panels.connect.cube.client = Some(client);

    let generation = app.panels.connect.cube.session_generation();
    assert!(app
        .panels
        .connect
        .cube
        .begin_external_registration()
        .is_some());
    app.sync_panel_derived_cache_fields();
    assert_eq!(app.cache.cube_mark_access, CubeMarkAccess::Waiting);
    let in_flight = capture::inspect(
        "cube-mark-registering",
        avatar_row(&app.cache),
        OUTER,
        "dark",
        coincube_ui::theme::Theme::dark(),
    );
    assert!(!in_flight.contains(MARK_UNCONFIRMED_LINE));

    settle(
        &mut app,
        Message::View(view::Message::ConnectCube(
            view::ConnectCubeMessage::CubeRegistered {
                generation,
                result: Err("Out of Cube slots".to_string().into()),
            },
        )),
    );
    assert_eq!(app.cache.cube_mark_access, CubeMarkAccess::Unconfirmed);
    let shown = capture::inspect(
        "cube-mark-unconfirmed",
        avatar_row(&app.cache),
        OUTER,
        "dark",
        coincube_ui::theme::Theme::dark(),
    );
    assert!(shown.contains(MARK_UNCONFIRMED_LINE));
    assert!(!shown.has_focus(Id::new(MARK_REGENERATE_ID)));
    let _ = std::fs::remove_dir_all(&root);
}

/// A save still in flight when its Connect session ends must not hold the
/// buttons in the next session, and its late answer changes nothing.
#[test]
fn a_save_in_flight_does_not_hold_the_next_session() {
    let _guard = crate::app::session::test_guard();
    let root = temp_root("saving-session");
    let server = MockServer::start();
    let mut app = mark_app(&root);
    sign_in(&mut app, &server);
    let old = app.panels.connect.cube.session_generation();
    app.cache.cube_mark_change = CubeMarkChange::Saving { generation: old };
    app.sync_panel_derived_cache_fields();
    assert_eq!(
        app.cache.cube_mark_change,
        CubeMarkChange::Saving { generation: old }
    );
    // Sign out and back in while the old call is still out.
    app.panels.connect.cube.clear_client();
    sign_in(&mut app, &server);
    assert_eq!(app.cache.cube_mark_change, CubeMarkChange::Idle);
    // The old call's late answer changes nothing in the new session.
    let current = app.cube_settings.mark_variation;
    drop(app.update(Message::CubeMarkSet {
        generation: old,
        result: Ok(current + 5),
    }));
    assert_eq!(app.cache.cube_mark_change, CubeMarkChange::Idle);
    assert_eq!(app.cube_settings.mark_variation, current);
    let _ = std::fs::remove_dir_all(&root);
}

/// A failure line belongs to the Connect session that saw it: signing out
/// (or into another account) clears it.
#[test]
fn a_failure_line_does_not_outlive_its_session() {
    let _guard = crate::app::session::test_guard();
    let root = temp_root("failure-session");
    let server = MockServer::start();
    let mut app = mark_app(&root);
    sign_in(&mut app, &server);
    let generation = app.panels.connect.cube.session_generation();
    app.cache.cube_mark_change = CubeMarkChange::Failed { generation };
    app.sync_panel_derived_cache_fields();
    assert_eq!(
        app.cache.cube_mark_change,
        CubeMarkChange::Failed { generation }
    );
    app.panels.connect.cube.clear_client();
    sign_in(&mut app, &server);
    assert_eq!(app.cache.cube_mark_change, CubeMarkChange::Idle);
    let shown = capture::inspect(
        "cube-mark-new-session",
        avatar_row(&app.cache),
        OUTER,
        "dark",
        coincube_ui::theme::Theme::dark(),
    );
    assert!(!shown.contains(MARK_FAILED_LINE));
    let _ = std::fs::remove_dir_all(&root);
}
