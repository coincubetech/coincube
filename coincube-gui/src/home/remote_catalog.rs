//! Connect's remote-only Cube catalog and its request ownership.

use super::{
    catalog_display_name, derive_phone_recovery, launcher_catalog, locally_stored_cubes, settings,
    CoincubeError, CubeResponse, GhostImport, Home, Message, RemoteCube, Task,
};

impl Home {
    /// The signed-in Connect account, when its user record is loaded.
    pub(super) fn connect_account_id(&self) -> Option<u32> {
        self.connect_account
            .authenticated_client()
            .and(self.connect_account.user.as_ref())
            .map(|user| user.id)
    }

    /// A ghost import goes ahead only while the account that listed the
    /// ghost is signed in and still lists it, on the same network.
    pub(super) fn ghost_still_registered(&self, ghost: &GhostImport) -> Result<(), String> {
        let listed = self.remote_cubes.iter().any(|cube| {
            cube.uuid == ghost.uuid && launcher_catalog::ghost_chain(cube) == Some(ghost.network)
        });
        if listed && ghost.account == self.connect_account_id() {
            Ok(())
        } else {
            Err(coincube_ui::localized_format!(
                "Connect no longer lists {} for this account. Cancel, then pick it again from the Launcher.",
                "Connect ya no muestra {} en esta cuenta. Cancela y vuelve a elegirlo desde el Launcher.",
                catalog_display_name(&ghost.name)
            ))
        }
    }

    /// Fetch the full server cube list and keep the ones with no local
    /// counterpart — the "restore from Connect" rows on the Cubes page. Both
    /// the API call and the local settings reads run off the UI thread.
    /// `Task::none()` when signed out; the list is Connect-only.
    ///
    /// Must be re-run whenever the *local* set changes, not just at sign-in.
    /// `Message::Checked` can only prune this list (drop rows that became
    /// local); a Cube deleted locally while its Connect copy was kept becomes
    /// remote-only at that instant, and nothing but a re-fetch can discover it.
    /// Without this it stayed invisible — and so unrestorable — until the next
    /// sign-in.
    ///
    /// Claims a fresh [`remote_cubes_request`](Self::remote_cubes_request)
    /// generation and carries it through to `RemoteCubesLoaded`, so a result
    /// that lands after a newer fetch (or after logout) is discarded rather
    /// than applied. Takes `&mut self` for that reason.
    pub(super) fn fetch_remote_cubes(&mut self) -> Task<Message> {
        // Bump unconditionally, before the signed-out bail: a fetch starting
        // while signed out still means "anything older is superseded", and
        // leaving the generation untouched here would let an in-flight result
        // from a prior session land afterwards.
        let request_id = self.invalidate_remote_cubes();
        let Some(rc_client) = self.connect_account.authenticated_client() else {
            return Task::none();
        };
        let datadir = self.datadir_path.clone();
        Task::perform(
            async move {
                let server_cubes = rc_client.list_cubes().await.map_err(|e| e.to_string())?;

                // Collect local cube UUIDs across all networks. Each listed
                // Cube's mark variation rides back with the result; it is
                // written to disk only once the result is known current.
                let marks = server_mark_variations(&server_cubes);
                let mut local_uuids = std::collections::HashSet::new();
                for cube in locally_stored_cubes(&datadir) {
                    local_uuids.insert(cube.id);
                }

                // Every listed Cube over the plan's limit, local or not.
                let connect_off: Vec<String> = server_cubes
                    .iter()
                    .filter(|sc| sc.connect_features_disabled)
                    .map(|sc| sc.uuid.clone())
                    .collect();

                // Keep only server cubes with no local counterpart
                let remote_source: Vec<CubeResponse> = server_cubes
                    .into_iter()
                    .filter(|sc| !local_uuids.contains(&sc.uuid))
                    .collect();

                // Determine restorability per cube. `has_recovery_kit`
                // comes free from `list_cubes()`, but both the seed half
                // (what a password restore needs) and the owner-keychain
                // "phone" envelope summary live on `/recovery-kit/status`,
                // so probe it for *every* remote cube — a phone-only cube
                // has `has_recovery_kit == false` yet is still recoverable
                // via its envelope, so gating the probe on the password
                // kit would hide it. Probes run concurrently (`join_all`)
                // so latency is one round-trip, not N. A flaky probe must
                // not blank the whole list — on any error we log and treat
                // every method as absent (the row still shows, just
                // without a restore icon). `join_all` preserves input
                // order.
                //
                // Duress note: phone availability is read from the
                // `ownerSelf` *status* summary (presence/tier, no
                // ciphertext); the duress gate fires later, at the actual
                // recovery attempt (`OwnerKeychainRestoreStep` →
                // `fetch_owner_recovery_envelope`), which returns neutral
                // "unavailable, try later" copy on a 423 (invariant I3).
                let client_ref = &rc_client;
                let remote_only: Vec<RemoteCube> = iced::futures::future::join_all(
                    remote_source.into_iter().map(|sc| async move {
                        let (has_encrypted_seed, phone_recoverable, phone_full_cube) =
                            match client_ref.get_recovery_kit_status(sc.id).await {
                                Ok(status) => {
                                    let (phone_recoverable, phone_full_cube) =
                                        derive_phone_recovery(status.owner_self.as_ref());
                                    (
                                        status.has_encrypted_seed,
                                        phone_recoverable,
                                        phone_full_cube,
                                    )
                                }
                                // No kit/status yet → nothing recoverable.
                                Err(CoincubeError::NotFound) => (false, false, false),
                                Err(e) => {
                                    log::warn!(
                                        "[LAUNCHER] Recovery Kit status probe \
                                         failed for \"{}\": {}",
                                        sc.name,
                                        e
                                    );
                                    (false, false, false)
                                }
                            };
                        RemoteCube {
                            id: sc.id,
                            uuid: sc.uuid,
                            name: sc.name,
                            network: sc.network,
                            has_recovery_kit: sc.has_recovery_kit,
                            has_encrypted_seed,
                            phone_recoverable,
                            phone_full_cube,
                            encryption_pubkey: sc.encryption_pubkey,
                            has_vault: sc.has_vault,
                            mark_variation: sc.mark_variation.unwrap_or_default(),
                        }
                    }),
                )
                .await;

                Ok((remote_only, connect_off, marks))
            },
            move |result: Result<RemoteListing, String>| {
                let (result, connect_off, marks) = match result {
                    Ok((remote_only, connect_off, marks)) => (Ok(remote_only), connect_off, marks),
                    Err(e) => (Err(e), Vec::new(), Default::default()),
                };
                Message::RemoteCubesLoaded {
                    request_id,
                    result,
                    connect_off,
                    marks,
                }
            },
        )
    }

    /// Supersede every in-flight remote-cube fetch and return the new
    /// generation. Callers that are *starting* a fetch keep the returned id;
    /// callers that only need to discard (logout) ignore it.
    ///
    /// Logout must do this. It clears `remote_cubes`, but a fetch issued for
    /// the previous account is still running and its `RemoteCubesLoaded` would
    /// otherwise repopulate the list — putting the prior account's Cubes back
    /// on screen after sign-out. The neighbouring `recover_vault` reset and
    /// `recovery_method_modal` drop guard the same hazard for their surfaces.
    pub(super) fn invalidate_remote_cubes(&mut self) -> u64 {
        self.remote_cubes_request = self.remote_cubes_request.wrapping_add(1);
        self.remote_cubes_request
    }

    pub(super) fn apply_remote_cubes(
        &mut self,
        request_id: u64,
        result: Result<Vec<RemoteCube>, String>,
        connect_off: Vec<String>,
        marks: MarkVariations,
    ) -> Task<Message> {
        // Superseded by a newer fetch, or invalidated by logout. Drop
        // it: applying it would either roll the list back to an older
        // server view or restore the previous account's Cubes.
        if request_id != self.remote_cubes_request {
            log::debug!(
                "[LAUNCHER] Discarding stale remote-cube fetch #{} (current #{})",
                request_id,
                self.remote_cubes_request
            );
            return Task::none();
        }
        match result {
            Ok(remote_only) => {
                self.remote_cubes = remote_only;
                self.connect_off_cubes = connect_off.into_iter().collect();
                // Connect's number is the record: show it now, and write
                // any differing one into the Cube's settings file, with the
                // fact that Connect has a row for every listed local Cube.
                // Only a current result gets here, so a late, older listing
                // can never write an older number.
                self.adopt_mark_variations(&marks);
                return persist_mark_variations(self.datadir_path.clone(), marks);
            }
            Err(e) => {
                log::warn!("[LAUNCHER] Failed to fetch remote cubes: {e}");
            }
        }
        Task::none()
    }
}

/// Every listed Cube by UUID, with its ring-mark variation when the response
/// carried one. An API that predates the field sends none, and the local
/// numbers stand; the Cube is still listed, so Connect has a row for it.
pub(crate) type MarkVariations = std::collections::HashMap<String, Option<u32>>;

type RemoteListing = (Vec<RemoteCube>, Vec<String>, MarkVariations);

/// Write each listed number into the local Cube's settings file where it
/// differs, and record that Connect has a row for each listed local Cube
/// (`CubeSettings::connect_registered`), off the UI thread. A Cube already
/// recorded with a matching number is not rewritten.
fn persist_mark_variations(
    datadir: crate::dir::CoincubeDirectory,
    marks: MarkVariations,
) -> Task<Message> {
    if marks.is_empty() {
        return Task::none();
    }
    Task::perform(
        async move {
            for cube in locally_stored_cubes(&datadir) {
                let Some(&variation) = marks.get(&cube.id) else {
                    continue;
                };
                let new_mark = variation.filter(|v| *v != cube.mark_variation);
                if cube.connect_registered && new_mark.is_none() {
                    continue;
                }
                let nd = datadir.network_directory(cube.network);
                if let Err(e) = settings::record_connect_row(&nd, &cube.id, new_mark).await {
                    log::warn!(
                        "[LAUNCHER] Failed to record Connect's mark for cube {}: {}",
                        cube.id,
                        e
                    );
                }
            }
        },
        |()| (),
    )
    .discard()
}

fn server_mark_variations(server_cubes: &[CubeResponse]) -> MarkVariations {
    server_cubes
        .iter()
        .map(|sc| (sc.uuid.clone(), sc.mark_variation))
        .collect()
}

impl Home {
    /// Take the server's ring-mark variations into the Cubes on screen: the
    /// current network's list and every other network's catalog.
    pub(super) fn adopt_mark_variations(&mut self, marks: &MarkVariations) {
        if marks.is_empty() {
            return;
        }
        let adopt = |cube: &mut crate::app::settings::CubeSettings| {
            if let Some(&listed) = marks.get(&cube.id) {
                cube.connect_registered = true;
                if let Some(variation) = listed {
                    cube.mark_variation = variation;
                }
            }
        };
        if let super::State::Cubes { cubes, .. } = &mut self.state {
            cubes.iter_mut().for_each(adopt);
        }
        for (_, cubes) in &mut self.network_catalog {
            cubes.iter_mut().for_each(adopt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use httpmock::{Method::GET, MockServer};
    use iced_runtime::futures::futures::StreamExt;
    use serde_json::json;

    fn home(dir: &std::path::Path, url: String) -> Home {
        let datadir = CoincubeDirectory::new(dir.to_path_buf());
        for chain in ChainId::ALL {
            let network_dir = datadir.network_directory(chain);
            std::fs::create_dir_all(network_dir.path()).unwrap();
            let cubes = if chain == ChainId::BitcoinBlake2b {
                vec![CubeSettings::new_with_raw_id(
                    "local".into(),
                    "Local".into(),
                    chain,
                )]
            } else {
                vec![]
            };
            std::fs::write(
                network_dir.path().join("settings.json"),
                serde_json::to_vec(&settings::Settings {
                    cubes,
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        }
        let mut home = Home::new(datadir, Some(Network::Bitcoin)).0;
        home.connect_account.client = CoincubeClient::for_test(url);
        home.connect_account.user = Some(crate::services::coincube::User {
            id: 7,
            email: "owner@example.invalid".into(),
            email_verified: Some(true),
            display_name: None,
        });
        home
    }

    fn cube(id: u64, uuid: &str) -> serde_json::Value {
        json!({"id": id, "uuid": uuid, "name": uuid, "network":"mainnet", "status":"active"})
    }

    async fn outputs(task: Task<Message>) -> Vec<Message> {
        let Some(stream) = iced_runtime::task::into_stream(task) else {
            return vec![];
        };
        stream
            .filter_map(|action| async move {
                match action {
                    iced_runtime::Action::Output(message) => Some(message),
                    _ => None,
                }
            })
            .collect()
            .await
    }

    fn cached() -> RemoteCube {
        RemoteCube {
            mark_variation: 0,
            id: 9,
            uuid: "cached".into(),
            name: "Cached".into(),
            network: "mainnet".into(),
            has_recovery_kit: false,
            has_encrypted_seed: false,
            phone_recoverable: false,
            phone_full_cube: false,
            encryption_pubkey: None,
            has_vault: None,
        }
    }

    #[tokio::test]
    async fn fetch_filters_all_local_chains_and_keeps_phone_only_and_failed_probe_rows() {
        let server = MockServer::start();
        let list = server.mock(|when, then| {
            when.method(GET).path("/api/v1/connect/cubes");
            then.status(200).json_body(json!({"data": [cube(1,"local"), cube(2,"phone"), cube(3,"missing"), cube(4,"offline")]}));
        });
        let local = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/connect/cubes/1/recovery-kit/status");
            then.status(500);
        });
        let phone = server.mock(|when, then| {
            when.method(GET).path("/api/v1/connect/cubes/2/recovery-kit/status");
            then.status(200).json_body(json!({"data": {"hasRecoveryKit": false, "hasEncryptedSeed": false,
                "hasEncryptedWalletDescriptor": false, "encryptionScheme":"aes-256-gcm",
                "ownerSelf": {"hasRecipient":true,"tier":"full","envelopeKinds":["seed","descriptor"]}}}));
        });
        let missing = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/connect/cubes/3/recovery-kit/status");
            then.status(404);
        });
        let offline = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/connect/cubes/4/recovery-kit/status");
            then.status(500);
        });
        let dir = tempfile::tempdir().unwrap();
        let mut home = home(dir.path(), server.base_url());
        for message in outputs(home.fetch_remote_cubes()).await {
            drop(home.update(message));
        }
        list.assert();
        local.assert_hits(0);
        phone.assert();
        missing.assert();
        offline.assert();
        assert_eq!(
            home.remote_cubes
                .iter()
                .map(|cube| cube.uuid.as_str())
                .collect::<Vec<_>>(),
            ["phone", "missing", "offline"]
        );
        assert!(!home.remote_cubes[0].has_recovery_kit);
        assert!(home.remote_cubes[0].phone_recoverable && home.remote_cubes[0].phone_full_cube);
        for cube in &home.remote_cubes[1..] {
            assert!(!cube.has_encrypted_seed && !cube.phone_recoverable && !cube.phone_full_cube);
        }
    }

    fn saved_local_mark(dir: &std::path::Path) -> u32 {
        let datadir = CoincubeDirectory::new(dir.to_path_buf());
        settings::Settings::from_file(&datadir.network_directory(ChainId::BitcoinBlake2b))
            .unwrap()
            .cubes
            .into_iter()
            .find(|cube| cube.id == "local")
            .unwrap()
            .mark_variation
    }

    /// Connect's mark variation is the record: listing the account's Cubes
    /// writes a differing number into the local Cube's settings, and a Cube
    /// on another computer carries its own.
    #[tokio::test]
    async fn listing_adopts_connects_mark_variation() {
        let server = MockServer::start();
        let mut local = cube(1, "local");
        local["markVariation"] = json!(4);
        local["markVersion"] = json!(1);
        let mut ghost = cube(2, "ghost");
        ghost["markVariation"] = json!(9);
        server.mock(|when, then| {
            when.method(GET).path("/api/v1/connect/cubes");
            then.status(200).json_body(json!({"data": [local, ghost]}));
        });
        server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/connect/cubes/2/recovery-kit/status");
            then.status(404);
        });
        let dir = tempfile::tempdir().unwrap();
        let mut home = home(dir.path(), server.base_url());
        assert_eq!(saved_local_mark(dir.path()), 0);
        for message in outputs(home.fetch_remote_cubes()).await {
            let persist = home.update(message);
            assert!(outputs(persist).await.is_empty());
        }
        assert_eq!(saved_local_mark(dir.path()), 4);
        assert_eq!(home.remote_cubes.len(), 1);
        assert_eq!(home.remote_cubes[0].mark_variation, 9);
        assert_eq!(home.remote_cubes[0].mark_seed(), None, "not a UUID");
    }

    /// An API that predates the field sends no number: the local one stands.
    #[tokio::test]
    async fn listing_without_a_mark_variation_keeps_the_local_one() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/api/v1/connect/cubes");
            then.status(200)
                .json_body(json!({"data": [cube(1, "local")]}));
        });
        let dir = tempfile::tempdir().unwrap();
        let datadir = CoincubeDirectory::new(dir.path().to_path_buf());
        let mut home = home(dir.path(), server.base_url());
        settings::set_cube_mark_variation(
            &datadir.network_directory(ChainId::BitcoinBlake2b),
            "local",
            3,
        )
        .await
        .unwrap();
        for message in outputs(home.fetch_remote_cubes()).await {
            let persist = home.update(message);
            assert!(outputs(persist).await.is_empty());
        }
        assert_eq!(saved_local_mark(dir.path()), 3);
    }

    /// Every local Cube a current Connect listing shows is recorded as
    /// registered (`CubeSettings::connect_registered`), with or without a
    /// mark number, which heals Cubes whose `remote_synced` a rename reset.
    #[tokio::test]
    async fn listing_records_each_listed_local_cube_as_registered() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/api/v1/connect/cubes");
            then.status(200)
                .json_body(json!({"data": [cube(1, "local")]}));
        });
        let dir = tempfile::tempdir().unwrap();
        let datadir = CoincubeDirectory::new(dir.path().to_path_buf());
        let saved = || {
            settings::Settings::from_file(&datadir.network_directory(ChainId::BitcoinBlake2b))
                .unwrap()
                .cubes
                .into_iter()
                .find(|cube| cube.id == "local")
                .unwrap()
        };
        let mut home = home(dir.path(), server.base_url());
        assert!(!saved().is_connect_registered());
        for message in outputs(home.fetch_remote_cubes()).await {
            let persist = home.update(message);
            assert!(outputs(persist).await.is_empty());
        }
        assert!(saved().connect_registered);
        assert_eq!(saved().mark_variation, 0);
    }

    /// The Cubes on screen take the listed numbers at once.
    #[test]
    fn adopting_marks_updates_the_cubes_on_screen() {
        let dir = tempfile::tempdir().unwrap();
        let mut home = home(dir.path(), "http://127.0.0.1:9".into());
        let chain = ChainId::Testnet4;
        home.network_catalog = vec![(
            chain,
            vec![CubeSettings::new_with_raw_id(
                "other".into(),
                "Other".into(),
                chain,
            )],
        )];
        home.state = State::Cubes {
            cubes: vec![CubeSettings::new_with_raw_id(
                "here".into(),
                "Here".into(),
                ChainId::Bitcoin,
            )],
            create_cube: false,
            source: ChainId::Bitcoin,
        };
        let marks = super::MarkVariations::from([
            ("other".to_owned(), Some(6)),
            ("here".to_owned(), Some(2)),
        ]);
        home.adopt_mark_variations(&marks);
        assert_eq!(home.network_catalog[0].1[0].mark_variation, 6);
        let State::Cubes { cubes, .. } = &home.state else {
            unreachable!()
        };
        assert_eq!(cubes[0].mark_variation, 2);
    }

    /// A listing that a newer fetch (or a logout) has superseded writes
    /// nothing, so a late, older number can never land on disk.
    #[tokio::test]
    async fn a_superseded_listing_writes_no_mark() {
        let server = MockServer::start();
        let mut local = cube(1, "local");
        local["markVariation"] = json!(5);
        server.mock(|when, then| {
            when.method(GET).path("/api/v1/connect/cubes");
            then.status(200).json_body(json!({"data": [local]}));
        });
        let dir = tempfile::tempdir().unwrap();
        let mut home = home(dir.path(), server.base_url());
        let stale = outputs(home.fetch_remote_cubes()).await;
        home.invalidate_remote_cubes();
        for message in stale {
            let persist = home.update(message);
            assert!(outputs(persist).await.is_empty());
        }
        assert_eq!(saved_local_mark(dir.path()), 0);
    }

    #[tokio::test]
    async fn signed_out_fetch_supersedes_a_real_inflight_response() {
        let server = MockServer::start();
        let list = server.mock(|when, then| {
            when.method(GET).path("/api/v1/connect/cubes");
            then.status(200)
                .json_body(json!({"data":[cube(3,"old-account")]}));
        });
        let status = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/connect/cubes/3/recovery-kit/status");
            then.status(404);
        });
        let dir = tempfile::tempdir().unwrap();
        let mut home = home(dir.path(), server.base_url());
        let prior = home.fetch_remote_cubes();
        let generation = home.remote_cubes_request;
        home.connect_account.user = None;
        assert!(outputs(home.fetch_remote_cubes()).await.is_empty());
        assert_ne!(generation, home.remote_cubes_request);
        for message in outputs(prior).await {
            drop(home.update(message));
        }
        list.assert();
        status.assert();
        assert!(home.remote_cubes.is_empty());
    }

    #[tokio::test]
    async fn failed_list_request_preserves_the_previous_catalog() {
        let server = MockServer::start();
        let list = server.mock(|when, then| {
            when.method(GET).path("/api/v1/connect/cubes");
            then.status(503);
        });
        let dir = tempfile::tempdir().unwrap();
        let mut home = home(dir.path(), server.base_url());
        home.remote_cubes = vec![cached()];
        for message in outputs(home.fetch_remote_cubes()).await {
            drop(home.update(message));
        }
        list.assert();
        assert_eq!(home.remote_cubes.len(), 1);
        assert_eq!(home.remote_cubes[0].uuid, "cached");
    }
}
