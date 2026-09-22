use iced::widget::{image, Space};
use iced::{Alignment, Length, Task};

use coincube_ui::{
    component::{
        button,
        quote_display::{self, Quote, QuoteDisplayProps},
        text::{h3, p1_regular, text},
    },
    icon, theme,
    widget::{Column, Container, Element, Row},
};

use crate::app::settings::CubeSettings;
use crate::pin_input;
use crate::services::unlock::{self, PinOutcome};

pub struct PinEntry {
    cube: CubeSettings,
    fork_signer: std::sync::Arc<
        std::sync::Mutex<Option<std::sync::Arc<coincube_core::signer::MasterSigner>>>,
    >,
    /// Data root, needed to reach this Cube's seed file — the PIN is verified
    /// by decrypting it, not by checking a stored hash.
    datadir_root: std::path::PathBuf,
    pin_input: pin_input::PinInput,
    error: Option<String>,
    loading: bool,
    fork_generation: u64,
    fork_task: Option<iced::task::Handle>,
    // Store what to do after successful PIN entry
    pub on_success: PinEntrySuccess,
    /// This device's enrolled Connect duress account id, captured at
    /// construction so it can be carried explicitly through `DuressDetected`
    /// (Task A.1) rather than re-derived deep inside activation. `None` for a
    /// sovereign (no-Connect) enrollment.
    duress_account_id: Option<String>,
    loading_quote: Quote,
    loading_image_handle: image::Handle,
}

pub enum PinEntrySuccess {
    LoadApp {
        datadir: crate::dir::CoincubeDirectory,
        config: crate::app::Config,
        network: coincube_core::miniscript::bitcoin::Network,
        // Optional Vault wallet loading fields
        internal_bitcoind: Option<crate::node::bitcoind::Bitcoind>,
        backup: Option<crate::backup::Backup>,
        wallet_settings: Option<crate::app::settings::WalletSettings>,
        connect_client: Option<crate::services::coincube::CoincubeClient>,
    },
}

#[derive(Debug, Clone)]
pub enum Message {
    PinInput(pin_input::Message),
    Submit,
    Back,
    PinVerified,
    /// The submitted PIN matched this Cube's **duress** PIN. Bubbles up to the
    /// tab state machine, which delegates to the duress orchestrator (wipe Cube
    /// data + server POST) and locks into the cryptic "Duress Mode Activated"
    /// screen. The parent intercepts this; it is never handled inside
    /// `PinEntry::update`.
    ///
    /// Carries this device's enrolled Connect duress `account_id` (`None` for
    /// sovereign) so the orchestrator receives it explicitly — see Task A.1.
    DuressDetected {
        account_id: Option<String>,
    },
    /// The (blocking, ~831 ms) trial decryption finished off the UI thread.
    ///
    /// Carries only a classification, never the decrypted seed. Key material
    /// must not enter the message queue — iced clones messages, and every clone
    /// would be another copy of the mnemonic on the heap.
    Classified(Result<Verdict, String>),
    ForkClassified(u64, Result<Verdict, String>),
}

/// What [`Message::Classified`] reports back to the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Unlock,
    Duress,
    Wrong,
}

fn next_fork_generation() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

type ForkSignerSlot =
    std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<coincube_core::signer::MasterSigner>>>>;

fn retain_unlocked_signer(
    chain: crate::chain::ChainId,
    cube_id: &str,
    slot: &ForkSignerSlot,
    signer: coincube_core::signer::MasterSigner,
) -> Result<Verdict, String> {
    if chain.is_blake2b() {
        *slot
            .lock()
            .map_err(|_| "Unlock state unavailable".to_string())? =
            Some(std::sync::Arc::new(signer));
    } else {
        let secp = coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::signing_only();
        let fingerprint = signer.fingerprint(&secp);
        crate::app::session::store_unlocked_signer(cube_id, fingerprint, signer);
    }
    Ok(Verdict::Unlock)
}

impl PinEntry {
    pub fn new(
        cube: CubeSettings,
        datadir_root: std::path::PathBuf,
        on_success: PinEntrySuccess,
        duress_account_id: Option<String>,
    ) -> Self {
        let loading_quote = quote_display::random_quote("loading");
        let loading_image_handle = quote_display::image_handle_for_context("loading");
        Self {
            cube,
            fork_signer: Default::default(),
            datadir_root,
            pin_input: pin_input::PinInput::new(),
            error: None,
            loading: false,
            fork_generation: next_fork_generation(),
            fork_task: None,
            on_success,
            duress_account_id,
            loading_quote,
            loading_image_handle,
        }
    }

    pub fn cube(&self) -> &CubeSettings {
        &self.cube
    }

    pub fn pin(&self) -> zeroize::Zeroizing<String> {
        self.pin_input.value()
    }

    pub fn take_fork_signer(&self) -> Option<std::sync::Arc<coincube_core::signer::MasterSigner>> {
        self.fork_signer.lock().ok()?.take()
    }

    pub fn fail_open(&mut self, reason: String) {
        self.loading = false;
        self.error = Some(reason);
        self.pin_input.clear();
        self.fork_generation = next_fork_generation();
        self.fork_task.take();
        // A blocking decrypt can still finish after cancellation. Detach its
        // slot so it cannot repopulate this screen's current handoff.
        self.fork_signer = Default::default();
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let message = match message {
            Message::ForkClassified(generation, verdict) => {
                if generation != self.fork_generation {
                    return Task::none();
                }
                self.fork_task.take();
                Message::Classified(verdict)
            }
            msg => msg,
        };
        match message {
            Message::PinInput(pin_input::Message::Submit) => {
                // Enter key pressed in a PIN field — trigger submit
                self.update(Message::Submit)
            }
            Message::PinInput(msg) => {
                self.error = None;
                self.pin_input.update(msg).map(Message::PinInput)
            }
            Message::Submit => {
                if self.loading {
                    return Task::none();
                }

                if !self.pin_input.is_complete() {
                    self.error = Some("Please enter all 4 digits".to_string());
                    return Task::none();
                }

                // Escalating lockout on repeated wrong PINs. Not load-bearing
                // — an offline attacker skips the UI entirely — but a laptop
                // thief shouldn't get unlimited free guesses through the front
                // door. See `services::unlock::throttle`.
                let throttle = unlock::throttle::ThrottleState::load(&self.datadir_root);
                let remaining = throttle.remaining_lockout(&self.cube.id);
                if !remaining.is_zero() {
                    self.error = Some(unlock::throttle::lockout_message(remaining));
                    self.pin_input.clear();
                    return Task::none();
                }

                let pin = self.pin_input.value();

                // Classification costs ~831 ms on the happy path and ~1.7 s on
                // a wrong PIN with duress enrolled: the PIN is now checked by
                // decrypting the seed file, not against a 27 ms hash. That MUST
                // NOT run inline in `update()` — it would freeze the window for
                // the whole derivation. Put up the loading screen (which
                // already exists for the Breez load that follows) and do the
                // work on the blocking pool.
                self.loading = true;
                self.error = None;

                let cube = self.cube.clone();
                let root = self.datadir_root.clone();
                let fork_signer = self.fork_signer.clone();
                let PinEntrySuccess::LoadApp { connect_client, .. } = &self.on_success;
                let connect_client = connect_client.clone();
                let generation = self.fork_generation;
                let is_fork = cube.network.is_blake2b();
                let task = Task::perform(
                    async move {
                        if cube.network.is_blake2b() {
                            let client = connect_client.as_ref().ok_or_else(|| {
                                "Sign in to Connect before unlocking this Bitcoin Blake2b Cube"
                                    .to_string()
                            })?;
                            crate::chain::require_connect_feature(cube.network, client).await?;
                        }
                        tokio::task::spawn_blocking(move || {
                            let loc = unlock::CubeLocation::new(&root, &cube);
                            let result = if cube.network.is_blake2b() {
                                unlock::unlock_connect_fork(&loc, &pin)
                            } else {
                                unlock::unlock_blocking(&loc, &pin)
                            };
                            match result {
                                Ok(PinOutcome::Unlock(signer)) => retain_unlocked_signer(
                                    cube.network,
                                    &cube.id,
                                    &fork_signer,
                                    *signer,
                                ),
                                Ok(PinOutcome::Duress) => Ok(Verdict::Duress),
                                Ok(PinOutcome::Wrong) => Ok(Verdict::Wrong),
                                // Everything else — a missing seed, a missing
                                // keyring entry, an unreachable keychain — is an
                                // operational failure, not a wrong PIN (I7).
                                //
                                // `NoPinConfigured` in particular used to be
                                // reported as `Wrong`, which told the user their
                                // (correct) PIN was incorrect *and* charged them
                                // a throttle penalty for a Cube whose seed
                                // simply is not on this device. The input is
                                // still rejected — it is never treated as a
                                // successful unlock (PR 4 / I1) — but it is not
                                // a PIN attempt and must not be counted as one.
                                Err(e) => Err(e.to_string()),
                            }
                        })
                        .await
                        .unwrap_or_else(|e| Err(format!("PIN check failed to run: {e}")))
                    },
                    move |result| {
                        if is_fork {
                            Message::ForkClassified(generation, result)
                        } else {
                            Message::Classified(result)
                        }
                    },
                );
                if is_fork {
                    let (task, handle) = task.abortable();
                    self.fork_task = Some(handle.abort_on_drop());
                    task
                } else {
                    task
                }
            }
            Message::Classified(Ok(Verdict::Unlock)) => {
                unlock::throttle::ThrottleState::load(&self.datadir_root)
                    .record_success(&self.datadir_root, &self.cube.id);
                // Stay in `loading` — the Breez/Spark load runs next and the
                // screen must not flash back to the keypad in between.
                Task::done(Message::PinVerified)
            }
            Message::Classified(Ok(Verdict::Duress)) => {
                // Clear the counter too: it must not survive as evidence that
                // someone was guessing, and this Cube is about to be wiped.
                unlock::throttle::ThrottleState::load(&self.datadir_root)
                    .record_success(&self.datadir_root, &self.cube.id);
                // Clear the buffer and bubble up the enrolled account id so the
                // parent can drive the orchestrator. The neutral loading screen
                // stays up during the brief async activation gap: it is
                // identical to a normal unlock, so it reveals nothing to an
                // onlooker, and it blocks further input until we lock into the
                // cryptic screen.
                self.pin_input.clear();
                let account_id = self.duress_account_id.clone();
                Task::done(Message::DuressDetected { account_id })
            }
            Message::Classified(Ok(Verdict::Wrong)) => {
                self.loading = false;
                let penalty = unlock::throttle::ThrottleState::load(&self.datadir_root)
                    .record_failure(&self.datadir_root, &self.cube.id);
                self.error = Some(if penalty.is_zero() {
                    "Incorrect PIN. Please try again.".to_string()
                } else {
                    unlock::throttle::lockout_message(penalty)
                });
                self.pin_input.clear();
                Task::none()
            }
            Message::Classified(Err(e)) => {
                // Deliberately NOT "Incorrect PIN". This is a locked keychain, a
                // missing keyring entry, or unreadable files — telling a user
                // their PIN is wrong in that situation is how they conclude
                // their wallet is gone (invariant I7).
                self.loading = false;
                self.error = Some(e);
                self.pin_input.clear();
                Task::none()
            }
            // `DuressDetected` is intercepted by the parent (tab state machine);
            // if it ever reaches here it's a no-op.
            Message::Back
            | Message::PinVerified
            | Message::DuressDetected { .. }
            | Message::ForkClassified(_, _) => Task::none(),
        }
    }

    pub fn view(&self) -> Element<Message> {
        if self.loading {
            // Full-screen loading with Kage quote while BreezClient loads
            return Container::new(
                Column::new()
                    .width(Length::Fill)
                    .spacing(20)
                    .align_x(Alignment::Center)
                    .push(Space::new().height(Length::Fill))
                    .push(quote_display::display(&QuoteDisplayProps::new(
                        "loading",
                        &self.loading_quote,
                        &self.loading_image_handle,
                    )))
                    .push(crate::loading::loading_indicator(None))
                    .push(text("Loading your Cube...").style(theme::text::secondary))
                    .push(Space::new().height(Length::Fill)),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(50)
            .into();
        }

        let back_button = button::secondary(Some(icon::previous_icon()), "Back")
            .width(Length::Fixed(150.0))
            .on_press(Message::Back);

        let header = Row::new()
            .align_y(Alignment::Center)
            .push(Container::new(back_button).center_x(Length::FillPortion(2)))
            .push(Space::new().width(Length::FillPortion(8)))
            .push(Space::new().width(Length::FillPortion(2)));

        let title = h3(format!("Enter PIN for {}", self.cube.name));

        let mut content = Column::new()
            .spacing(30)
            .width(Length::Fill)
            .align_x(Alignment::Center)
            .push(title)
            .push(self.pin_input.view().map(Message::PinInput));

        if let Some(error) = &self.error {
            content = content.push(p1_regular(error).style(theme::text::error));
        }

        let can_submit = self.pin_input.is_complete();

        let submit_button = button::primary(None, "Submit")
            .width(Length::Fixed(200.0))
            .on_press_maybe(if can_submit {
                Some(Message::Submit)
            } else {
                None
            });

        content = content.push(submit_button);

        Container::new(
            Column::new()
                .width(Length::Fill)
                .push(Space::new().height(Length::Fixed(100.0)))
                .push(header)
                .push(Space::new().height(Length::Fixed(100.0)))
                .push(Container::new(content).center_x(Length::Fill)),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(20)
        .into()
    }
}

#[cfg(test)]
mod fork_handoff_tests {
    use super::*;
    fn entry(chain: crate::chain::ChainId) -> PinEntry {
        let cube = CubeSettings::new_with_raw_id("same-id".into(), "Synthetic".into(), chain);
        PinEntry::new(
            cube,
            Default::default(),
            PinEntrySuccess::LoadApp {
                datadir: crate::dir::CoincubeDirectory::new(Default::default()),
                config: crate::app::Config::new(false),
                network: chain.bitcoin_network(),
                internal_bitcoind: None,
                backup: None,
                wallet_settings: None,
                connect_client: None,
            },
            None,
        )
    }
    #[test]
    fn production_handoff_keeps_fork_signer_out_of_legacy_bitcoin_cache() {
        let _guard = crate::app::session::test_guard();
        crate::app::session::close();
        crate::app::session::open("same-id", zeroize::Zeroizing::new("1234".to_string()));
        let fork = entry(crate::chain::ChainId::BitcoinBlake2b);
        let signer = coincube_core::signer::MasterSigner::generate(
            coincube_core::miniscript::bitcoin::Network::Bitcoin,
        )
        .unwrap();
        let secp = coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::signing_only();
        let fp = signer.fingerprint(&secp);
        let twin = signer.try_clone().unwrap();
        retain_unlocked_signer(
            crate::chain::ChainId::BitcoinBlake2b,
            "same-id",
            &fork.fork_signer,
            signer,
        )
        .unwrap();
        assert!(crate::app::session::unlocked_signer("same-id", fp).is_none());
        retain_unlocked_signer(
            crate::chain::ChainId::Bitcoin,
            "same-id",
            &fork.fork_signer,
            twin,
        )
        .unwrap();
        assert!(crate::app::session::unlocked_signer("same-id", fp).is_some());
        assert!(fork.take_fork_signer().is_some());
        assert!(fork.take_fork_signer().is_none());
        crate::app::session::close();
    }

    #[tokio::test]
    async fn late_blocking_handoff_and_classification_cannot_revive_invalidated_entry() {
        use iced_runtime::futures::futures::StreamExt;
        let mut fork = entry(crate::chain::ChainId::BitcoinBlake2b);
        let slot = fork.fork_signer.clone();
        let generation = fork.fork_generation;
        let (send, receive) = tokio::sync::oneshot::channel();
        let completion = tokio::spawn(async move {
            receive.await.unwrap();
            let signer = coincube_core::signer::MasterSigner::generate(
                coincube_core::miniscript::bitcoin::Network::Bitcoin,
            )
            .unwrap();
            let verdict = retain_unlocked_signer(
                crate::chain::ChainId::BitcoinBlake2b,
                "same-id",
                &slot,
                signer,
            );
            Message::ForkClassified(generation, verdict)
        });
        fork.fail_open("Signed out".into());
        send.send(()).unwrap();
        let late = completion.await.unwrap();
        let task = fork.update(late);
        if let Some(stream) = iced_runtime::task::into_stream(task) {
            assert_eq!(stream.count().await, 0);
        }
        assert!(fork.take_fork_signer().is_none());
        assert_eq!(fork.error.as_deref(), Some("Signed out"));
        assert!(!fork.loading);
    }

    #[tokio::test]
    async fn actual_pending_fork_pin_task_is_aborted_on_invalidation() {
        use iced_runtime::futures::futures::StreamExt;
        let mut fork = entry(crate::chain::ChainId::BitcoinBlake2b);
        fork.datadir_root =
            std::env::temp_dir().join(format!("fork-pin-cancel-{}", uuid::Uuid::new_v4()));
        for (i, digit) in "2468".chars().enumerate() {
            let _ = fork.update(Message::PinInput(pin_input::Message::DigitChanged(
                i,
                digit.to_string(),
            )));
        }
        let pending = fork.update(Message::Submit);
        assert!(fork.fork_task.is_some());
        fork.fail_open("Signed out".into());
        if let Some(stream) = iced_runtime::task::into_stream(pending) {
            assert_eq!(stream.count().await, 0);
        }
        assert!(fork.take_fork_signer().is_none());
    }

    #[test]
    fn failed_or_invalidated_open_discards_pending_signer() {
        let mut fork = entry(crate::chain::ChainId::BitcoinBlake2b);
        let signer = coincube_core::signer::MasterSigner::generate(
            coincube_core::miniscript::bitcoin::Network::Bitcoin,
        )
        .unwrap();
        *fork.fork_signer.lock().unwrap() = Some(std::sync::Arc::new(signer));
        fork.fail_open("Synthetic invalidation".into());
        assert!(fork.take_fork_signer().is_none());
    }
}
