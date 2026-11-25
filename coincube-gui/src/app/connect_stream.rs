//! Connect stream health, panel routing and device registration renewal.
use super::*;

impl App {
    /// Register the current Connect device and bring its signing stream online.
    pub(super) fn ensure_connect_ready(&mut self) -> Task<Message> {
        // A Bitcoin Blake2b Cube still on the session it was opened with
        // brings its pipeline up from that session. Until that session is
        // saved, this is the retry: save it again (`save_served_session`).
        if self.runs_on_opening_session() {
            match &self.served_session {
                ServedSession::Saved { .. } => {}
                ServedSession::Saving => return Task::none(),
                ServedSession::NotSaved | ServedSession::Failed => {
                    self.served_session = ServedSession::NotSaved;
                    return self.save_served_session();
                }
            }
        }
        // On such a Cube every step below runs on its own session: the
        // access token is its client's, never the one `connect.json` holds
        // now (I3). `None` on every other Cube.
        let pinned_tokens = self.pinned_tokens();
        if self.cache.connect_served && pinned_tokens.is_none() {
            return Task::none();
        }
        // A retry once the session is saved keeps the retry state shown:
        // the bootstrap's own failures answer with a notice, not a message
        // this App could clear a "starting" state on. It reads ready once
        // the stream is up.
        // The user is signed in to Connect but the signing stream
        // isn't ready. Run the same device-registration + stream
        // bootstrap the in-app login flow does, sourcing tokens
        // from `connect.json`. The async block decides the next
        // message itself so we can give accurate feedback: some
        // networks (e.g. testnet4) have no `grpc_url` in their
        // ServiceConfig, meaning Connect/Keychain relay signing is
        // unavailable there no matter how often the user retries —
        // we say so and point them at local Wi-Fi pairing instead.
        let chain = self.cache.chain();
        let base_url = self.connect_base_url();
        let datadir = self.cache.datadir_path.clone();
        let expected_email = match &self.served_session {
            ServedSession::Saved { email, .. } if self.cache.connect_served => Some(email.clone()),
            _ => self
                .connect_email
                .clone()
                .or_else(|| self.cache.connect_email.clone()),
        };
        let shared_auth = if self.connect_email == expected_email {
            self.connect_auth.clone()
        } else {
            None
        };
        let cube_uuid = if self.cube_settings.id.is_empty() {
            None
        } else {
            Some(self.cube_settings.id.clone())
        };
        // The device/stream bootstrap below populates grpc_url /
        // tokens / device_id but NOT the cube's server-side id. When
        // that id is the missing Keychain prerequisite, register it
        // here too — otherwise "Sign with Connect" can never resolve
        // it. A live panel session registers through its authenticated
        // client; a restored connect.json (or remote) session whose
        // panel hasn't reached Dashboard has no panel client, so the
        // panel path would no-op — register directly from the restored
        // tokens (the same source the device bootstrap uses) and feed
        // the result back through the normal `CubeRegistered` path so
        // `current_cube_server_id` still populates.
        let cube_registration = if self.cache.current_cube_server_id.is_some() {
            Task::none()
        } else if self.panels.connect.account.is_authenticated() {
            self.panels.connect.ensure_cube_registered()
        } else if let Some(generation) = self.panels.connect.cube.begin_external_registration() {
            let datadir = datadir.clone();
            let net_str = settings::network_to_api_string(chain);
            let cube_name = self.cube_settings.name.clone();
            // Report this Cube's Vault presence so other devices can
            // evaluate the duress vault gate (PLAN-duress-vault-gate
            // PR 3). Monotonic upgrade-only: report `true` only when
            // this device holds the Vault, else omit (never clobber a
            // `true` reported by another device). Captured before the
            // async move.
            let cube_has_vault = self.cube_settings.vault_wallet_id.is_some().then_some(true);
            // Same for this device's Spark Stable Balance decision:
            // `None` is omitted, so it never clobbers another device's.
            let cube_spark_stable_balance = self.cube_settings.spark_stable_balance;
            let cube_uuid = cube_uuid.clone();
            let registration_email = expected_email.clone();
            let registration_base_url = base_url.clone();
            let registration_pin = pinned_tokens
                .as_ref()
                .map(|tokens| tokens.access_token.clone());
            Task::perform(
                async move {
                    use crate::services::coincube::{CoincubeClient, RegisterCubeRequest};
                    use crate::services::connect::client::cache::Account;
                    let uuid = cube_uuid.ok_or_else(|| "no cube uuid".to_string())?;
                    let email = registration_email.ok_or_else(|| {
                        "EnsureConnectReady: no active Connect email to register the cube"
                            .to_string()
                    })?;
                    let access_token = match registration_pin {
                        Some(pinned) => pinned,
                        None => {
                            Account::from_cache(&datadir.network_directory(chain), &email)
                                .ok()
                                .flatten()
                                .ok_or_else(|| {
                                    "EnsureConnectReady: no cached Connect session to \
                                 register the cube"
                                        .to_string()
                                })?
                                .tokens
                                .access_token
                        }
                    };
                    let mut client = CoincubeClient::with_base_url(registration_base_url);
                    client.set_token(&access_token);
                    client
                        .register_cube(RegisterCubeRequest {
                            uuid,
                            name: cube_name,
                            network: net_str,
                            has_vault: cube_has_vault,
                            spark_stable_balance: cube_spark_stable_balance,
                        }) // upgrade-only Option<bool>
                        .await
                        .map_err(|e| {
                            crate::app::state::connect::cube::CubeRegistrationError::from(&e)
                        })
                },
                move |r| {
                    Message::View(view::Message::ConnectCube(
                        view::ConnectCubeMessage::CubeRegistered {
                            generation,
                            result: r,
                        },
                    ))
                },
            )
        } else {
            // A registration is already in flight — claimed either by
            // this branch on an earlier press or by the panel. Skip the
            // duplicate; the bootstrap below still runs, which is the
            // part a second press is actually asking for.
            Task::none()
        };
        let carry_unsaved_id = pinned_tokens.is_some();
        let bootstrap = Task::perform(
            async move {
                use crate::services::connect::client::cache::Account;
                use crate::services::connect::grpc::bootstrap::ensure_device_registered_best_effort;

                let info_toast = |msg: &str| {
                    Message::View(view::Message::ShowToast(log::Level::Info, msg.to_string()))
                };

                let network_dir = datadir.network_directory(chain);
                let Some(expected_email) = expected_email else {
                    tracing::warn!(
                        "EnsureConnectReady: no active Connect email to match connect.json"
                    );
                    return info_toast(
                        "Couldn't find a Connect session. Sign in to Connect, or pair \
                         a phone over Wi-Fi to sign locally.",
                    );
                };
                // A Connect-served Cube's tokens are its own, whatever
                // `connect.json` holds now; every other Cube reads the file.
                let (email, cached_tokens) = match pinned_tokens {
                    Some(pinned) => (expected_email, pinned),
                    None => {
                        let Some(account) = Account::from_cache(&network_dir, &expected_email)
                            .ok()
                            .flatten()
                        else {
                            tracing::warn!(
                                "EnsureConnectReady: no cached Connect account for active email"
                            );
                            return info_toast(
                                "Couldn't find this Connect session. Sign in to Connect, or \
                                 pair a phone over Wi-Fi to sign locally.",
                            );
                        };
                        (account.email, account.tokens)
                    }
                };
                let pinned = carry_unsaved_id;
                let tokens_arc =
                    connect_registration_auth(shared_auth, cached_tokens, &network_dir, &email)
                        .await;

                let Some(grpc_url) =
                    crate::services::connect::client::resolve_connect_grpc_url_for(&base_url).await
                else {
                    // No gRPC endpoint for this network — Connect
                    // relay signing can't work here. Don't pretend
                    // a retry will help.
                    tracing::info!("EnsureConnectReady: no Connect gRPC URL available for {chain}");
                    return info_toast(
                        "Mobile signing over Connect isn't available on this network. \
                         Pair a phone over Wi-Fi to sign locally instead.",
                    );
                };

                // Same helper as every other registration path — see
                // the note on the in-app login path above.
                let device_name = crate::utils::device::signer_device_label(chain);
                // An id that could not be saved (the shared file moved on)
                // still serves a Connect-served Cube pinned to its session;
                // every other Cube needs it saved, as before.
                let Some(registered) = ensure_device_registered_best_effort(
                    &grpc_url,
                    tokens_arc.clone(),
                    &network_dir,
                    &email,
                    device_name,
                    env!("CARGO_PKG_VERSION").to_string(),
                    std::env::consts::OS.to_string(),
                )
                .await
                .filter(|registered| registered.persisted || pinned) else {
                    return info_toast(
                        "Couldn't register this device with Connect. Try again, or pair a \
                         phone over Wi-Fi to sign locally.",
                    );
                };

                // gRPC is available and the device is registered —
                // bring the stream up so the cache fields populate,
                // then the user can retry Sign via Keychain.
                Message::TriggerConnectStreamReady {
                    chain,
                    datadir,
                    tokens: tokens_arc,
                    email,
                    cube_uuid,
                    device_id: (!registered.persisted).then_some(registered.device_id),
                }
            },
            |m| m,
        );
        Task::batch([cube_registration, bootstrap])
    }

    /// Top-level handler for `Message::ConnectStream`. PR A logs and
    /// persists the latest event seq; PR B's session-routing logic is
    /// folded in here when the per-modal dispatch lands.
    pub(super) fn handle_connect_stream(
        &mut self,
        event: crate::services::connect::grpc::ConnectStreamMessage,
    ) -> Task<Message> {
        use crate::services::connect::grpc::ConnectStreamMessage as M;
        match event {
            M::Connected => {
                log::info!("[CONNECT GRPC] Stream connected");
                self.connect_renewal = Renewal::default();
                self.set_connect_status(ConnectionStatus::Connected)
            }
            M::Disconnected(reason) => {
                log::warn!("[CONNECT GRPC] Stream disconnected: {}", reason);
                // The stream reports a failure before announcing its retry;
                // keep that failure until a connection succeeds. A drop from
                // a live stream reads as Reconnecting…, the first attempt
                // as Connecting….
                match self.cache.connect_stream_status {
                    ConnectionStatus::Connected => {
                        self.set_connect_status(ConnectionStatus::Reconnecting)
                    }
                    ConnectionStatus::Inactive => {
                        self.set_connect_status(ConnectionStatus::Connecting)
                    }
                    _ => Task::none(),
                }
            }
            M::Trouble(trouble) => {
                use crate::services::connect::grpc::trouble::Trouble;
                // A renewal in progress or one that needs the user outranks
                // the stream's own trouble.
                if matches!(
                    self.cache.connect_stream_status,
                    ConnectionStatus::Renewing | ConnectionStatus::RenewFailed
                ) {
                    return Task::none();
                }
                self.set_connect_status(match trouble {
                    Trouble::Offline => ConnectionStatus::Offline,
                    Trouble::Unreachable => ConnectionStatus::Reconnecting,
                })
            }
            M::Refused(refusal) => {
                use crate::services::connect::grpc::trouble::Refusal;
                match refusal {
                    // Connect has no record of this id: renew it (C1).
                    Refusal::DeviceUnknown => self.renew_connect_device(false),
                    // Removed from the account: never renewed (C3). Signing
                    // out drops the stream.
                    Refusal::DeviceRevoked => {
                        log::warn!("[CONNECT] This computer was removed from the Connect account");
                        Task::batch([
                            Task::done(Message::ConnectSignedOutBy(SignedOutBy::ComputerRemoved)),
                            Task::done(Message::View(view::Message::ConnectAccount(
                                view::ConnectAccountMessage::LogOut,
                            ))),
                        ])
                    }
                    // A server without reasons: renewing could undo a
                    // removal, so the user decides (Try again).
                    Refusal::Unexplained => self.set_connect_status(ConnectionStatus::RenewFailed),
                }
            }
            // A Connect-served Bitcoin Blake2b Cube cannot take another
            // session while open (I3), so an ended one ends its Connect
            // services for good instead of retrying for ever.
            M::SessionEnded(err) if self.cache.connect_served => self.end_served_session(err),
            // Elsewhere an ended session is handled like an in-stream error:
            // the stream retries with backoff and reads the shared token
            // again on each attempt.
            M::Error(err) | M::SessionEnded(err) => {
                log::warn!("[CONNECT GRPC] Stream error: {}", err);
                if matches!(
                    self.cache.connect_stream_status,
                    ConnectionStatus::Renewing | ConnectionStatus::RenewFailed
                ) {
                    return Task::none();
                }
                self.set_connect_status(ConnectionStatus::Reconnecting)
            }
            M::SessionEvent(session_event) => self.handle_connect_session_event(session_event),
            M::DuressActivated { unlock_at, source } => {
                self.handle_connect_duress_activated(unlock_at, source)
            }
            M::DuressCleared => self.handle_connect_duress_cleared(),
            M::DuressDisabled { account_id } => {
                // Issue 2: duress disabled account-wide on another device. Disarm
                // THIS device locally (clear Cube PIN hashes + local state +
                // queue) — no UI lock, no wipe. `reconcile_duress_disarm` only
                // touches a matching Connect enrollment, so a sovereign local
                // enrollment (or an unrelated account) is left untouched. Refresh
                // the Duress panel afterwards if the user is sitting in it.
                log::info!("[CONNECT GRPC] Duress disabled remotely");
                let datadir = self.datadir.clone();
                let gen = self.panels.connect.account.session_generation();
                Task::perform(
                    async move { reconcile_duress_disarm(datadir, account_id).await },
                    move |res| {
                        Message::View(view::Message::ConnectAccount(
                            view::ConnectAccountMessage::Duress(
                                view::DuressMessage::DisarmComplete(res, gen),
                            ),
                        ))
                    },
                )
            }
        }
    }

    fn handle_connect_session_event(
        &self,
        session_event: crate::services::connect::grpc::connect_v1::SessionEvent,
    ) -> Task<Message> {
        log::info!(
            "[CONNECT GRPC] SessionEvent seq={} type={:?} session={}",
            session_event.event_seq,
            session_event.event_type,
            session_event.session_id,
        );
        // Persist the latest seq so a restart resumes from the
        // right cursor. Best-effort — log and continue on error.
        let seq = session_event.event_seq;
        let persist_task = if let Some(email) = self.connect_email.clone() {
            let network_dir = self.datadir.network_directory(self.cache.chain());
            Task::perform(
                async move {
                    if let Err(e) =
                        crate::services::connect::client::cache::set_last_seen_event_seq_for_email(
                            &network_dir,
                            &email,
                            seq,
                        )
                        .await
                    {
                        log::warn!(
                            "[CONNECT GRPC] Failed to persist last_seen_event_seq={}: {}",
                            seq,
                            e,
                        );
                    }
                },
                |_| Message::CacheUpdated,
            )
        } else {
            Task::none()
        };
        // Fan the event out via Message::KeychainSign(StreamEvent).
        // It travels through the standard update path and is
        // delegated to the active PSBT modal (if any) by
        // `PsbtState`'s catchall arm — modals that don't
        // recognise the session_id are no-ops.
        let dispatch_task = Task::done(Message::KeychainSign(
            crate::app::state::vault::keychain_sign::KeychainSignMessage::StreamEvent(
                session_event,
            ),
        ));
        Task::batch([persist_task, dispatch_task])
    }

    fn handle_connect_duress_activated(
        &self,
        unlock_at: Option<chrono::DateTime<chrono::Utc>>,
        source: String,
    ) -> Task<Message> {
        // Phase 7b: remote duress activation. Persist active state to
        // DuressLocalState (at the data-dir root) WITHOUT wiping —
        // remote activation can be accidental, so local Cube data is
        // left intact. The UI lock is sequenced AFTER the persist (it's
        // the task's completion message, not a parallel batch) so the
        // cryptic screen never appears before `active` is durable; if
        // the app exits right after seeing the lock, the relaunch
        // reconcile still routes back into duress.
        log::warn!(
            "[CONNECT GRPC] Duress activated remotely (source={})",
            source
        );
        let root = self.datadir.path().to_path_buf();
        Task::perform(
            async move {
                // load() already maps a missing file to Ok(default); a
                // real parse/IO error must NOT be papered over with a
                // default that save() then writes back, clobbering valid
                // state (enrolled / account_id / encrypted code). Skip
                // the persist on a real read error — the UI still locks
                // below, and the DuressLockRemote backstop / session-check
                // re-sync handle durability.
                let mut st = match crate::services::duress::DuressLocalState::load(&root) {
                    Ok(st) => st,
                    Err(e) => {
                        log::warn!(
                            "[CONNECT GRPC] reading duress state failed; \
                             not overwriting: {e}"
                        );
                        return;
                    }
                };
                st.active = true;
                st.unlock_at = unlock_at;
                // Retry: a failed persist would let a relaunch reconcile
                // (which keys off st.active) drop back to the normal Home
                // flow with Cube data intact while the server is still in
                // duress. The DuressLockRemote handler re-persists as a
                // final backstop tied to the UI lock.
                for attempt in 1..=3 {
                    match st.save(&root) {
                        Ok(()) => break,
                        Err(e) => log::warn!(
                            "[CONNECT GRPC] persist remote duress state \
                             attempt {attempt}/3 failed: {e}"
                        ),
                    }
                }
            },
            |_| Message::View(view::Message::DuressLockRemote),
        )
    }

    fn handle_connect_duress_cleared(&self) -> Task<Message> {
        log::info!("[CONNECT GRPC] Duress cleared remotely");
        let root = self.datadir.path().to_path_buf();
        Task::perform(
            async move {
                // As above: skip on a real read error so a transient
                // failure can't clobber valid state with a default. The
                // cryptic screen's own server poll re-syncs the cleared
                // state on the next check.
                let mut st = match crate::services::duress::DuressLocalState::load(&root) {
                    Ok(st) => st,
                    Err(e) => {
                        log::warn!(
                            "[CONNECT GRPC] reading duress state failed; \
                             not overwriting: {e}"
                        );
                        return;
                    }
                };
                st.active = false;
                st.unlock_at = None;
                if let Err(e) = st.save(&root) {
                    log::warn!("[CONNECT GRPC] Failed to clear remote duress state: {e}");
                }
            },
            |_| Message::CacheUpdated,
        )
    }

    /// The Connect control, its panel and renewal; everything else routes on.
    pub(super) fn update_connect_chrome(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::View(view::Message::Settings(
                view::SettingsMessage::ReregisterConnectDevice,
            )) => self.renew_connect_device(true),
            Message::View(view::Message::Settings(
                view::SettingsMessage::ConnectDeviceReregistered(result),
            )) => self.settle_connect_renewal(result),
            Message::ConnectLightTick => {
                self.cache.connect_light_dim = !self.cache.connect_light_dim;
                Task::none()
            }
            Message::ConnectRenewalRetry => {
                if self.cache.connect_stream_status == ConnectionStatus::Renewing {
                    self.renew_connect_device(false)
                } else {
                    Task::none()
                }
            }
            Message::ConnectSignedOutBy(reason) => {
                self.cache.connect_signed_out_by = Some(reason);
                self.cache.connect_panel_open = false;
                Task::none()
            }
            Message::View(view::Message::ToggleConnectPanel) => {
                self.cache.connect_panel_open = !self.cache.connect_panel_open;
                self.cache.cube_switcher_open = false;
                self.cache.cube_lock_confirmation = false;
                if self.cache.connect_panel_open {
                    iced::widget::operation::focus(view::nav::shell::connect_panel_first_focus(
                        &self.cache,
                    ))
                } else {
                    iced::widget::operation::focus(view::nav::shell::CONNECTION_ID)
                }
            }
            Message::View(view::Message::CloseConnectPanel) => {
                if !self.cache.connect_panel_open {
                    return Task::none();
                }
                self.cache.connect_panel_open = false;
                iced::widget::operation::focus(view::nav::shell::CONNECTION_ID)
            }
            Message::View(view::Message::DismissConnectSignOut) => {
                self.cache.connect_signed_out_by = None;
                self.cache.connect_panel_open = false;
                iced::widget::operation::focus(view::nav::shell::CONNECTION_ID)
            }
            message => self.route(message),
        }
    }

    /// Sets the live-updates status and tells an open Keychain signing modal.
    pub(super) fn set_connect_status(&mut self, status: ConnectionStatus) -> Task<Message> {
        if self.cache.connect_stream_status == status {
            return Task::none();
        }
        self.cache.connect_stream_status = status;
        stream_health_dispatch(status)
    }

    /// Registers this computer again (C1). Automatic attempts stop after
    /// [`RENEWAL_ATTEMPTS`]; Try again (`manual`) always makes one more.
    pub(super) fn renew_connect_device(&mut self, manual: bool) -> Task<Message> {
        if self.connect_renewal.in_flight {
            return Task::none();
        }
        if manual {
            self.connect_renewal.attempts = RENEWAL_ATTEMPTS - 1;
        } else if self.connect_renewal.attempts >= RENEWAL_ATTEMPTS {
            return self.set_connect_status(ConnectionStatus::RenewFailed);
        }
        let Some(task) = crate::app::state::settings::connections::renewal_task(&self.cache) else {
            log::warn!("[CONNECT] Can't renew this computer's registration: Connect isn't set up");
            return self.set_connect_status(ConnectionStatus::RenewFailed);
        };
        self.connect_renewal.attempts += 1;
        self.connect_renewal.in_flight = true;
        log::info!(
            "[CONNECT] Renewing this computer's registration (attempt {}{})",
            self.connect_renewal.attempts,
            if manual { ", Try again" } else { "" }
        );
        Task::batch([self.set_connect_status(ConnectionStatus::Renewing), task])
    }

    /// A renewal settled. Success swaps the id into the stream; a failure
    /// retries after [`RENEWAL_RETRY`] until the attempts run out.
    pub(super) fn settle_connect_renewal(
        &mut self,
        result: Result<view::ReregisteredDevice, String>,
    ) -> Task<Message> {
        self.connect_renewal.in_flight = false;
        match result {
            Ok(registered) => {
                // Logged for support instead of shown (C6).
                log::info!(
                    "[CONNECT] Renewed this computer's registration: {} -> {}",
                    registered.replaces,
                    registered.device_id
                );
                self.adopt_reregistered_device(&registered)
                    .unwrap_or_else(Task::none)
            }
            Err(e) => {
                log::warn!(
                    "[CONNECT] Renewing this computer's registration failed: {}",
                    e
                );
                if self.connect_renewal.attempts >= RENEWAL_ATTEMPTS {
                    self.set_connect_status(ConnectionStatus::RenewFailed)
                } else {
                    Task::perform(tokio::time::sleep(RENEWAL_RETRY), |_| {
                        Message::ConnectRenewalRetry
                    })
                }
            }
        }
    }

    /// A successful renewal hands back the device id. Swap it into the stream
    /// config: the subscription is keyed on `device_id`, so a stream stopped
    /// by a refusal (or one still using the stale id) is replaced by one that
    /// says hello with the new id.
    /// The swap only applies to the stream the request was started from: a
    /// result landing after sign-out (which clears the stream config), for
    /// another account, or after a newer sign-in already moved the stream to
    /// a different id is ignored, so it can't bring the device card back or
    /// replace a newer registration.
    /// An unchanged id restarts the stream too (through the subscription's
    /// restart count): the running one may be sleeping on a refusal
    /// re-check for minutes after Connect accepted the registration.
    pub(super) fn adopt_reregistered_device(
        &mut self,
        registered: &view::ReregisteredDevice,
    ) -> Option<Task<Message>> {
        if self.connect_email.as_deref() != Some(registered.email.as_str()) {
            return None;
        }
        let cfg = self.connect_stream_config.as_mut()?;
        if cfg.device_id != registered.replaces {
            return None;
        }
        if cfg.device_id == registered.device_id {
            self.connect_stream_restart = self.connect_stream_restart.wrapping_add(1);
        } else {
            cfg.device_id = registered.device_id.clone();
            self.cache.connect_device_id = Some(registered.device_id.clone());
        }
        // The new subscription reports Connected or a refusal; until then
        // the light keeps pulsing amber.
        Some(self.set_connect_status(ConnectionStatus::Renewing))
    }
}

impl App {
    /// The Connect API this Cube's signer pipeline talks to: the base URL of
    /// the Cube's own client. On a Bitcoin Cube that is the panel's client,
    /// built on the app's configured API. A Bitcoin Blake2b Cube that was
    /// opened with a session uses the endpoint that session was admitted on.
    pub(super) fn connect_base_url(&self) -> String {
        self.fork_connect_client
            .as_ref()
            .map(|client| client.base_url.clone())
            .unwrap_or_else(|| self.panels.connect.account.client.base_url.clone())
    }

    pub(super) fn connect_stream_ready(
        &mut self,
        cfg: Option<(
            crate::services::connect::grpc::stream::ConnectStreamConfig,
            String,
        )>,
    ) -> Task<Message> {
        match cfg {
            Some((mut cfg, email)) => {
                if self.connect_email.as_deref() != Some(email.as_str())
                    || !self
                        .connect_auth
                        .as_ref()
                        .is_some_and(|auth| Arc::ptr_eq(auth, &cfg.tokens))
                {
                    return Task::none();
                }
                let current = match cfg.tokens.try_read() {
                    Ok(current) => current,
                    Err(_) => {
                        let tokens = cfg.tokens.clone();
                        let retry_cfg = cfg.clone();
                        return Task::perform(
                            async move {
                                drop(tokens.read().await);
                                Message::ConnectStreamReady(Some((retry_cfg, email)))
                            },
                            |message| message,
                        );
                    }
                };
                let network_dir = self
                    .cache
                    .datadir_path
                    .network_directory(self.cache.chain());
                if self.cache.connect_served {
                    // Pinned to the Cube's own session (I3). The shared file
                    // may hold a newer token for this account; its entry still
                    // gives the device id and the event cursor, which belong
                    // to the account and this computer, not to a token.
                    let pinned =
                        self.is_pinned_session(&cfg.base_url, &current.access_token, &email);
                    drop(current);
                    if !pinned {
                        return Task::none();
                    }
                    let entry = crate::services::connect::client::cache::Account::from_cache(
                        &network_dir,
                        &email,
                    )
                    .ok()
                    .flatten();
                    // About may have completed re-registration while the
                    // async vault lookup ran: a saved id wins. Otherwise the
                    // id registration returned without saving it stays.
                    if let Some(saved) = entry
                        .as_ref()
                        .and_then(|entry| entry.device_id.clone())
                        .filter(|id| !id.trim().is_empty())
                    {
                        cfg.device_id = saved;
                    }
                    if cfg.device_id.trim().is_empty() {
                        return self.set_signer_pipeline_start(cache::SignerPipelineStart::Failed);
                    }
                    cfg.last_seen_seq = cfg.last_seen_seq.max(
                        entry
                            .and_then(|entry| entry.last_seen_event_seq)
                            .unwrap_or(0),
                    );
                } else {
                    let cached_account =
                        crate::services::connect::client::cache::ConnectCache::from_file(
                            &network_dir,
                        )
                        .ok()
                        .and_then(|cache| cache.active_account().cloned())
                        .filter(|account| {
                            account.email == email
                                && account.tokens.access_token == current.access_token
                                && account.tokens.refresh_token == current.refresh_token
                        });
                    drop(current);
                    let Some(account) = cached_account else {
                        return Task::none();
                    };
                    let Some(device_id) = account.device_id.filter(|id| !id.trim().is_empty())
                    else {
                        return Task::none();
                    };
                    // About may have completed re-registration while the
                    // async vault lookup ran. Install its current id, not
                    // the one captured before that await.
                    cfg.device_id = device_id;
                    cfg.last_seen_seq = cfg
                        .last_seen_seq
                        .max(account.last_seen_event_seq.unwrap_or(0));
                }
                tracing::info!(
                    "Connect stream ready (device_id={}, last_seen_seq={})",
                    cfg.device_id,
                    cfg.last_seen_seq,
                );
                // Mirror into Cache so deep panels (the open
                // PSBT modal in particular) can spin up a
                // GrpcSessionClient on demand.
                self.cache.connect_grpc_url = Some(cfg.grpc_url.clone());
                self.cache.connect_base_url = Some(cfg.base_url.clone());
                self.cache.connect_tokens = Some(cfg.tokens.clone());
                self.cache.connect_device_id = Some(cfg.device_id.clone());
                self.cache.connect_email = Some(email);
                self.connect_stream_config = Some(cfg);
                // Subscription will mount on the next render
                // tick — show `Connecting` until the first
                // `ConnectStreamMessage::Connected` lands.
                self.cache.connect_stream_status = ConnectionStatus::Connecting;
                return self.set_signer_pipeline_start(cache::SignerPipelineStart::Idle);
            }
            None => {
                tracing::debug!("Connect stream not started: missing grpc_url or device_id",);
                // A Connect-served Cube whose session is saved says so,
                // with a retry, rather than leaving the signer list waiting.
                if self.runs_on_opening_session()
                    && matches!(self.served_session, ServedSession::Saved { .. })
                    && self.connect_stream_config.is_none()
                {
                    return self.set_signer_pipeline_start(cache::SignerPipelineStart::Failed);
                }
            }
        }
        Task::none()
    }

    /// Moves a Connect-served Cube's signer pipeline start-up along and tells
    /// the current panel, so an open signer list follows it at once (it reads
    /// the state on the next message it gets). Nothing is sent when the state
    /// does not change, so on every other Cube this stays silent.
    fn set_signer_pipeline_start(&mut self, start: cache::SignerPipelineStart) -> Task<Message> {
        if self.cache.signer_pipeline_start == start {
            return Task::none();
        }
        self.cache.signer_pipeline_start = start;
        stream_health_dispatch(self.cache.connect_stream_status)
    }

    pub(super) fn trigger_connect_stream_ready(
        &mut self,
        chain: crate::chain::ChainId,
        datadir: CoincubeDirectory,
        tokens: Arc<
            tokio::sync::RwLock<crate::services::connect::client::auth::AccessTokenResponse>,
        >,
        email: String,
        cube_uuid: Option<String>,
        device_id: Option<String>,
    ) -> Task<Message> {
        if self.in_app_refresh_commit.is_some() {
            self.deferred_connect_stream_triggers
                .push(Message::TriggerConnectStreamReady {
                    chain,
                    datadir,
                    tokens,
                    email,
                    cube_uuid,
                    device_id,
                });
            return Task::none();
        }
        let current = match tokens.try_read() {
            Ok(current) => current,
            Err(_) => {
                let wait_tokens = tokens.clone();
                let retry_tokens = tokens.clone();
                return Task::perform(
                    async move {
                        drop(wait_tokens.read().await);
                        Message::TriggerConnectStreamReady {
                            chain,
                            datadir,
                            tokens: retry_tokens,
                            email,
                            cube_uuid,
                            device_id,
                        }
                    },
                    |message| message,
                );
            }
        };
        // A Connect-served Cube takes only its own session, whatever the
        // shared `connect.json` holds now (I3); every other Cube only the
        // session the file holds.
        let owns_session = if self.cache.connect_served {
            self.is_pinned_session(&self.connect_base_url(), &current.access_token, &email)
        } else {
            let network_dir = datadir.network_directory(chain);
            crate::services::connect::client::cache::ConnectCache::from_file(&network_dir)
                .ok()
                .and_then(|cache| cache.active_account().cloned())
                .is_some_and(|account| {
                    account.email == email
                        && account.tokens.access_token == current.access_token
                        && account.tokens.refresh_token == current.refresh_token
                })
        };
        drop(current);
        if !owns_session {
            return Task::none();
        }
        self.connect_auth = Some(tokens.clone());
        self.connect_email = Some(email.clone());
        self.cache.connect_tokens = Some(tokens.clone());
        self.cache.connect_email = Some(email.clone());
        self.cache.has_connect_session = true;
        connect_stream_ready_task(
            chain,
            self.connect_base_url(),
            datadir,
            tokens,
            email,
            cube_uuid,
            device_id.filter(|_| self.cache.connect_served),
        )
    }

    /// Whether this Bitcoin Blake2b Cube brings its signer pipeline up from
    /// the session it was opened with, saving it at open: always when it is
    /// Connect-served, and for a local Cube while it still holds that
    /// session (`fork_connect_client`, PR 5). Only a Connect-served Cube is
    /// also pinned to it; a local one then runs the Bitcoin path, and once
    /// that session ends its panel signs in as a Bitcoin Cube's does.
    fn runs_on_opening_session(&self) -> bool {
        self.cache.chain().is_blake2b()
            && (self.cache.connect_served || self.fork_connect_client.is_some())
    }

    /// The session a Connect-served Bitcoin Blake2b Cube's signer pipeline
    /// is pinned to (I3): the Cube's own client, for the account its session
    /// was saved for at open. `None` on every other Cube, and before the
    /// session is saved.
    fn pinned_connect_session(&self) -> Option<crate::app::connect_session::ConnectSession> {
        if !self.cache.connect_served {
            return None;
        }
        let ServedSession::Saved { email, .. } = &self.served_session else {
            return None;
        };
        Some(crate::app::connect_session::ConnectSession {
            client: self.fork_connect_client.clone()?,
            account: email.clone(),
        })
    }

    /// Whether a pipeline entry's endpoint, bearer and account are the
    /// session this Cube is pinned to, compared by the one comparison every
    /// Connect session check uses (`connect_session::same_session`). False
    /// whenever nothing is pinned.
    pub(super) fn is_pinned_session(
        &self,
        base_url: &str,
        access_token: &str,
        email: &str,
    ) -> bool {
        let Some(pinned) = self.pinned_connect_session() else {
            return false;
        };
        let mut client = pinned.client.clone();
        client.base_url = base_url.to_string();
        client.set_token(access_token);
        crate::app::connect_session::same_session(
            &pinned,
            &crate::app::connect_session::ConnectSession {
                client,
                account: email.to_string(),
            },
        )
    }

    /// The tokens a Connect-served Cube's pipeline runs on: its client's
    /// access token and the refresh token saved with it at open. `None` on
    /// every other Cube, and before the session is saved.
    fn pinned_tokens(&self) -> Option<crate::services::connect::client::auth::AccessTokenResponse> {
        let pinned = self.pinned_connect_session()?;
        let ServedSession::Saved { refresh_token, .. } = &self.served_session else {
            return None;
        };
        let access_token = pinned.client.token().filter(|t| !t.trim().is_empty())?;
        Some(
            crate::services::connect::client::auth::AccessTokenResponse {
                access_token: access_token.to_string(),
                refresh_token: refresh_token.clone(),
                expires_at: chrono::Utc::now().timestamp() + 30 * 24 * 60 * 60,
            },
        )
    }

    /// Save a Connect-served Bitcoin Blake2b Cube's session to its chain
    /// folder, once its admitted panel has reached its dashboard and the
    /// account email is known (Desktop plan PR 3). A Bitcoin Cube gets its
    /// `connect.json` from an in-Cube sign-in; this Cube has none, and its
    /// admitted panel must not be sent the login messages (it answers an
    /// accepted commit by revoking its client, which closes the Cube). So the
    /// App writes the session with the guarded writer a keyring restore uses
    /// and then runs the login completion itself. Does nothing on any other
    /// Cube, or once the session is saving or saved.
    pub(super) fn save_served_session(&mut self) -> Task<Message> {
        if !self.runs_on_opening_session() || self.served_session != ServedSession::NotSaved {
            return Task::none();
        }
        let Some(client) = self.fork_connect_client.as_ref() else {
            return Task::none();
        };
        let account = &self.panels.connect.account;
        let Some(token) = client.token().filter(|t| !t.trim().is_empty()) else {
            return Task::none();
        };
        let Some(email) = account
            .user
            .as_ref()
            .map(|user| user.email.clone())
            .filter(|email| !email.is_empty())
        else {
            return Task::none();
        };
        // The panel must still hold the Cube's own client: a panel that lost
        // it is being torn down, and nothing is written for it.
        if !account.is_authenticated()
            || account.client.token() != Some(token)
            || account.client.base_url != client.base_url
        {
            return Task::none();
        }
        let saved_refresh = account.saved_refresh_token_for(token);
        // A local Cube is not pinned and later Cubes of the account restore
        // this file, so it writes only a whole session, as a Bitcoin sign-in
        // commit does. Without the saved sign-in's refresh token it writes
        // nothing and signs in through the Bitcoin path once its opening
        // session ends.
        if !self.cache.connect_served && saved_refresh.is_none() {
            return Task::none();
        }
        let tokens = crate::services::connect::client::auth::AccessTokenResponse {
            access_token: token.to_string(),
            // The saved sign-in's refresh token only when it is this same
            // session. A Connect-served Cube never refreshes, and the gRPC
            // interceptor and the stream send the access token only, so an
            // empty one is never sent anywhere.
            refresh_token: saved_refresh.unwrap_or_default(),
            expires_at: chrono::Utc::now().timestamp() + 30 * 24 * 60 * 60,
        };
        let epoch = account.auth_epoch();
        // The App's own data directory: the save never depends on a cache
        // field being bound to it.
        let network_dir = self.datadir.network_directory(self.cache.chain());
        self.served_session = ServedSession::Saving;
        let starting = self.set_signer_pipeline_start(cache::SignerPipelineStart::Starting);
        let save = Task::perform(
            async move {
                use crate::services::connect::client::cache::{
                    persist_restored_tokens, ConnectCache, ConnectCacheError,
                };
                let saved = async {
                    tokio::fs::create_dir_all(network_dir.path())
                        .await
                        .map_err(|e| ConnectCacheError::WritingFile(e.to_string()))?;
                    // A fresh snapshot: the guard refuses only a change made
                    // between this read and the locked write, or a file it
                    // cannot parse. It replaces a different token already
                    // stored for the account and makes the account active,
                    // without clearing a live re-registration claim or its
                    // device id (a new login would).
                    let snapshot = match ConnectCache::from_file(&network_dir) {
                        Ok(snapshot) => snapshot,
                        Err(ConnectCacheError::NotFound) => ConnectCache::default(),
                        Err(e) => return Err(e),
                    };
                    persist_restored_tokens(&network_dir, &email, &tokens, &snapshot).await
                }
                .await;
                if let Err(e) = &saved {
                    tracing::warn!("Couldn't save this Cube's Connect session: {e}");
                }
                Message::ConnectServedSessionSaved {
                    email,
                    tokens,
                    epoch,
                    saved: saved.is_ok(),
                }
            },
            |message| message,
        );
        Task::batch([starting, save])
    }

    /// The save started by [`Self::save_served_session`] settled. Saved, the
    /// Cube runs the login completion a Bitcoin Cube runs after its in-Cube
    /// sign-in: it registers the signer device and brings the stream up.
    /// Refused, the signer list says Mobile signing could not start and
    /// offers to try again; the Cube is otherwise unaffected.
    pub(super) fn connect_served_session_saved(
        &mut self,
        email: String,
        tokens: crate::services::connect::client::auth::AccessTokenResponse,
        epoch: u64,
        saved: bool,
    ) -> Task<Message> {
        // A teardown since the write started leaves nothing to continue.
        if self.served_session != ServedSession::Saving
            || self.fork_connect_client.as_ref().and_then(|c| c.token())
                != Some(tokens.access_token.as_str())
        {
            return Task::none();
        }
        if !saved {
            self.served_session = ServedSession::Failed;
            return self.set_signer_pipeline_start(cache::SignerPipelineStart::Failed);
        }
        self.served_session = ServedSession::Saved {
            email: email.clone(),
            refresh_token: tokens.refresh_token.clone(),
        };
        self.connect_login_completed(tokens.access_token, tokens.refresh_token, email, epoch)
    }

    /// A Connect-served Cube's session ended on the server. Its backend is
    /// bound to that session (I3), so it is not retried: the fork teardown
    /// runs, and the Cube asks to be reopened. A Cube promoted to its managed
    /// node loses its Connect services only and keeps its node. Pending
    /// signing requests are cancelled first, best effort: the ended session
    /// may refuse the cancel too, and the server lets them expire.
    fn end_served_session(&mut self, reason: String) -> Task<Message> {
        log::warn!("[CONNECT GRPC] This Cube's Connect session ended: {reason}");
        let cancel = match (self.daemon.clone(), self.panels.current_mut()) {
            (daemon, Some(panel)) => panel.update(
                daemon,
                &self.cache,
                Message::View(view::Message::Spend(
                    view::SpendTxMessage::CancelKeychainSign,
                )),
            ),
            _ => Task::none(),
        };
        if self.requires_connect() {
            self.invalidate_fork_session();
        } else {
            // It keeps the reopen notice (`revoke_local_connect_services`).
            self.revoke_local_connect_services();
        }
        cancel
    }
}

#[cfg(test)]
mod chain_scope_tests {
    use crate::app::settings::network_to_api_string;
    use crate::chain::ChainId;
    use crate::dir::CoincubeDirectory;

    /// The signer pipeline resolves its Connect folder and the network name
    /// it sends from the Cube's `ChainId`. On every chain that pre-dates the
    /// fork that is exactly what the `bitcoin::Network` it used to carry
    /// gave, so existing installs keep their folder, their `connect.json`
    /// and their registration.
    #[test]
    fn a_bitcoin_family_cube_keeps_its_connect_folder_and_network_name() {
        let root = CoincubeDirectory::new(std::path::PathBuf::from("/coincube"));
        for chain in ChainId::ALL.iter().copied().filter(|c| !c.is_blake2b()) {
            let network = chain.bitcoin_network();
            assert_eq!(
                root.network_directory(chain).path(),
                root.network_directory(network).path(),
                "{chain:?}"
            );
            assert_eq!(
                network_to_api_string(chain),
                network_to_api_string(network),
                "{chain:?}"
            );
        }
    }

    /// A `bitcoin::Network` cannot tell Bitcoin Blake2b from Bitcoin: carried
    /// through the pipeline it would point a Bitcoin Blake2b Cube at the
    /// Bitcoin folder and register it as `mainnet`.
    #[test]
    fn a_bitcoin_blake2b_cube_gets_its_own_connect_folder_and_network_name() {
        let root = CoincubeDirectory::new(std::path::PathBuf::from("/coincube"));
        for (chain, dir, name) in [
            (
                ChainId::BitcoinBlake2b,
                "bitcoin-blake2b",
                "bitcoin-blake2b",
            ),
            (
                ChainId::BitcoinBlake2bTestnet4,
                "bitcoin-blake2b-testnet4",
                "bitcoin-blake2b-testnet4",
            ),
        ] {
            assert_eq!(
                root.network_directory(chain).path(),
                root.path().join(dir),
                "{chain:?}"
            );
            assert_eq!(network_to_api_string(chain), name, "{chain:?}");

            let lossy = chain.bitcoin_network();
            assert_ne!(
                root.network_directory(lossy).path(),
                root.network_directory(chain).path()
            );
            assert_ne!(network_to_api_string(lossy), name);
        }
    }

    /// A tripwire for three files, not a proof: the pipeline's own files must
    /// not go back to the lossy network. It matches the accessor written on
    /// one line, so a reformatted or aliased read slips past it, and it does
    /// not cover `app/mod.rs` or `gui/tab.rs`, which have legitimate uses of
    /// the lossy network (the duress check in `gui/tab.rs` among them, left
    /// on it deliberately so a Bitcoin sign-in keeps confirming a clear).
    /// `a_fork_cube_snapshots_the_connect_cache_of_its_own_chain_folder` in
    /// `app/mod.rs` covers a call site there.
    #[test]
    fn the_signer_pipeline_never_reads_the_lossy_cache_network() {
        for (file, source) in [
            ("app/connect_stream.rs", include_str!("connect_stream.rs")),
            ("app/connect_auth.rs", include_str!("connect_auth.rs")),
            (
                "app/state/settings/connections.rs",
                include_str!("state/settings/connections.rs"),
            ),
        ] {
            // Built at run time so this test's own text is not a match.
            let lossy = ["cache", "network"].join(".");
            for (number, line) in source.lines().enumerate() {
                assert!(
                    !line.contains(&lossy),
                    "{file}:{} reads the lossy network: {}",
                    number + 1,
                    line.trim()
                );
            }
        }
    }
}
