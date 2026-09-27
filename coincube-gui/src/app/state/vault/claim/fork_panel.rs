//! Fork Claim screen. The coordinator owns authorization and the journal;
//! this screen supplies explicit consent and keeps asynchronous results bound
//! to the session which started them.
use super::super::psbt::{claim_signing_dispatch, PsbtState};
use super::{describe, fork_load::Loaded, SigningOnlyDaemon, SESSION_ENDED};
use crate::{
    app::{cache::Cache, menu::Menu, message::Message, state::State, view, wallet::Wallet},
    daemon::{
        model::{Coin, SpendTx},
        Daemon,
    },
    dir::CoincubeDirectory,
    services::{
        claim_coordinator::{
            fork::{Coordinator, Preparation, SigningCheck},
            Outcome, Review, Revoker,
        },
        claim_observation::TransactionObservation,
        claim_workflow::{Context, Status},
    },
};
use coincube_core::{miniscript::bitcoin::secp256k1, psbt_unified::UnifiedPsbt};
use coincube_ui::{
    component::{button, card, text::*},
    widget::{ColumnExt, Element},
};
use iced::{widget::Column, Subscription, Task};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

static NEXT_PANEL: AtomicU64 = AtomicU64::new(1);

/// Only the fork panel consumes these results, even after navigation away.
/// The epoch prevents a completed check from reopening a revoked screen.
pub struct Event {
    epoch: u64,
    result: ResultEvent,
}
impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForkClaimEvent")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}
enum ResultEvent {
    Signer(Box<Message>),
    Signing {
        preparation: Box<Preparation>,
        pending: Box<Message>,
        result: Result<SigningCheck, String>,
    },
    Reviewed {
        coordinator: Box<Coordinator>,
        result: Result<Review, String>,
    },
    Submitted {
        coordinator: Box<Coordinator>,
        result: Result<Outcome, String>,
    },
    Tracked {
        coordinator: Box<Coordinator>,
        result: Result<Tracking, String>,
    },
}
struct Tracking {
    status: Status,
    transaction: TransactionObservation,
    saved: bool,
}

pub struct ForkClaimPanel {
    root: CoincubeDirectory,
    context: Context,
    revoker: Revoker,
    epoch: u64,
    revoked: bool,
    busy: bool,
    pending_epoch: Option<u64>,
    error: Option<String>,
    psbt: Option<Box<PsbtState>>,
    preparation: Option<Box<Preparation>>,
    coordinator: Option<Box<Coordinator>>,
    review: Option<Review>,
    outcome: Option<Outcome>,
    tracking: Option<Tracking>,
}
impl ForkClaimPanel {
    /// `Loaded` can only be obtained by reopening the paired, owned journal.
    /// Coins are display information; construction ownership was checked by
    /// the loader and is checked again by the coordinator at signing/submission.
    pub fn new(
        root: CoincubeDirectory,
        wallet: Arc<Wallet>,
        coins: Vec<Coin>,
        loaded: Loaded,
    ) -> Result<Self, String> {
        if wallet.chain != coincube_core::chain::ChainId::BitcoinBlake2b {
            return Err("Open the paired Bitcoin Blake2b Vault to continue this Claim.".into());
        }
        let revoker = loaded.revoker();
        let (context, preparation, psbt, coordinator, outcome) = match loaded {
            Loaded::Signing {
                preparation,
                psbt,
                context,
            } => {
                let tx = SpendTx::new(
                    None,
                    psbt.psbt().clone(),
                    coins,
                    &wallet.main_descriptor,
                    &secp256k1::Secp256k1::verification_only(),
                    wallet.chain.bitcoin_network(),
                );
                let mut state = PsbtState::new(wallet, tx, false);
                // Refuse every actual signing dispatch until a fresh check is
                // returned to this screen and consumed by PsbtState.
                state.require_claim_signing_checks();
                (
                    context,
                    Some(Box::new(preparation)),
                    Some(Box::new(state)),
                    None,
                    None,
                )
            }
            Loaded::Tracking {
                coordinator,
                context,
            } => {
                let outcome = coordinator
                    .recorded_outcome()
                    .ok_or_else(|| "No fork submission is recorded for this Claim.".to_string())?;
                (
                    context,
                    None,
                    None,
                    Some(Box::new(coordinator)),
                    Some(outcome),
                )
            }
        };
        Ok(Self {
            root,
            context,
            revoker,
            epoch: NEXT_PANEL.fetch_add(1, Ordering::Relaxed),
            revoked: false,
            busy: false,
            pending_epoch: None,
            error: None,
            psbt,
            preparation,
            coordinator,
            review: None,
            outcome,
            tracking: None,
        })
    }
    pub fn is_revoked(&self) -> bool {
        self.revoked || self.revoker.is_revoked()
    }
    pub fn revoke(&mut self) {
        self.revoker.revoke();
        self.epoch = self.epoch.wrapping_add(1);
        self.revoked = true;
        self.review = None;
        self.tracking = None;
        self.preparation = None;
        self.coordinator = None;
        if let Some(psbt) = &mut self.psbt {
            psbt.interrupt();
        }
        self.error = Some(SESSION_ENDED.into());
    }
    pub fn can_return_to_bitcoin(&self) -> bool {
        !self.busy
    }
    fn ready(&self) -> bool {
        !self.revoked && !self.revoker.is_revoked() && !self.busy
    }
    fn event(epoch: u64, result: ResultEvent) -> Message {
        Message::ForkClaim(Box::new(Event { epoch, result }))
    }
    fn route_signer(epoch: u64, task: Task<Message>) -> Task<Message> {
        task.map(move |message| Self::event(epoch, ResultEvent::Signer(Box::new(message))))
    }
    fn signer_result(
        &mut self,
        message: Message,
        daemon: Option<Arc<dyn Daemon + Send + Sync>>,
        cache: &Cache,
    ) -> Task<Message> {
        match message {
            Message::View(view::Message::ShowError(error)) => {
                self.error = Some(error);
                Task::none()
            }
            message @ (Message::EnsureConnectReady
            | Message::View(view::Message::Menu(_) | view::Message::OpenConnectSignIn)) => {
                Task::done(message)
            }
            message if claim_signing_dispatch(&message) => self.check_signer(message),
            message => {
                let (Some(psbt), Some(daemon)) = (&mut self.psbt, daemon) else {
                    return Task::none();
                };
                let task = psbt.update(Arc::new(SigningOnlyDaemon(daemon)), cache, message);
                Task::batch([Self::route_signer(self.epoch, task), self.finish_signing()])
            }
        }
    }
    fn check_signer(&mut self, pending: Message) -> Task<Message> {
        if !self.ready() {
            return Task::none();
        }
        let Some(mut preparation) = self.preparation.take() else {
            return Task::none();
        };
        self.busy = true;
        self.pending_epoch = Some(self.epoch);
        self.error = None;
        let context = self.context.clone();
        let epoch = self.epoch;
        Task::perform(
            async move {
                let result = preparation.check_signing(&context).await.map_err(describe);
                ResultEvent::Signing {
                    preparation,
                    pending: Box::new(pending),
                    result,
                }
            },
            move |result| Self::event(epoch, result),
        )
    }
    fn prepare_review(&mut self) -> Task<Message> {
        if !self.ready() || self.outcome.is_some() {
            return Task::none();
        }
        let Some(mut coordinator) = self.coordinator.take() else {
            return Task::none();
        };
        self.review = None;
        self.busy = true;
        self.pending_epoch = Some(self.epoch);
        self.error = None;
        let context = self.context.clone();
        let epoch = self.epoch;
        Task::perform(
            async move {
                let result = coordinator.prepare_review(&context).await.map_err(describe);
                ResultEvent::Reviewed {
                    coordinator,
                    result,
                }
            },
            move |result| Self::event(epoch, result),
        )
    }
    fn confirm(&mut self) -> Task<Message> {
        if !self.ready()
            || self.outcome.is_some()
            || self.coordinator.is_none()
            || self.review.is_none()
        {
            return Task::none();
        }
        let (Some(mut coordinator), Some(review)) = (self.coordinator.take(), self.review.take())
        else {
            // Called only when both are present; never discard a coordinator
            // merely because a stale button event has no current review.
            return Task::none();
        };
        self.busy = true;
        self.pending_epoch = Some(self.epoch);
        self.error = None;
        let context = self.context.clone();
        let epoch = self.epoch;
        Task::perform(
            async move {
                let result = coordinator
                    .confirm_and_submit(review, &context)
                    .await
                    .map_err(describe);
                ResultEvent::Submitted {
                    coordinator,
                    result,
                }
            },
            move |result| Self::event(epoch, result),
        )
    }
    fn track(&mut self) -> Task<Message> {
        if !self.ready() || self.outcome.is_none() {
            return Task::none();
        }
        let Some(mut coordinator) = self.coordinator.take() else {
            return Task::none();
        };
        self.busy = true;
        self.pending_epoch = Some(self.epoch);
        self.tracking = None; // previously displayed confirmation is no fresh authority
        self.error = None;
        let context = self.context.clone();
        let root = self.root.clone();
        let epoch = self.epoch;
        Task::perform(
            async move {
                let result = async {
                    let (status, transaction) = coordinator
                        .reconcile_completion(&context, &root)
                        .await
                        .map_err(describe)?;
                    let saved = if let Some(evidence) = coordinator
                        .check_completion(&context)
                        .await
                        .map_err(describe)?
                    {
                        evidence
                            .persist(&root)
                            .await
                            .map_err(|e| format!("Couldn't save Claim completion: {e}"))?;
                        true
                    } else {
                        false
                    };
                    Ok(Tracking {
                        status,
                        transaction,
                        saved,
                    })
                }
                .await;
                ResultEvent::Tracked {
                    coordinator,
                    result,
                }
            },
            move |result| Self::event(epoch, result),
        )
    }
    fn finish_signing(&mut self) -> Task<Message> {
        if !self.ready() {
            return Task::none();
        }
        let Some(psbt) = &self.psbt else {
            return Task::none();
        };
        if psbt.modal.is_some() || psbt.tx.path_ready().is_none() {
            return Task::none();
        }
        let signed = match UnifiedPsbt::from_psbt(psbt.tx.psbt.clone()) {
            Ok(signed) => signed,
            Err(error) => {
                self.error = Some(error.to_string());
                return Task::none();
            }
        };
        let Some(preparation) = self.preparation.take() else {
            return Task::none();
        };
        match preparation.finish(&signed, &self.context) {
            Ok(coordinator) => {
                self.psbt = None;
                self.coordinator = Some(Box::new(coordinator));
                self.prepare_review()
            }
            Err(error) => {
                self.error = Some(describe(error));
                // Without a preparation this screen cannot dispatch another
                // signature. The caller must reopen the recorded construction.
                Task::none()
            }
        }
    }
    fn apply(
        &mut self,
        event: Event,
        daemon: Option<Arc<dyn Daemon + Send + Sync>>,
        cache: &Cache,
    ) -> Task<Message> {
        // A revoked task still owns the journal until its result is delivered.
        // Only that exact task may release the busy hold; foreign callbacks
        // cannot make navigation race a live owner.
        if self.pending_epoch == Some(event.epoch)
            && !matches!(&event.result, ResultEvent::Signer(_))
        {
            self.pending_epoch = None;
            self.busy = false;
        }
        if event.epoch != self.epoch || self.revoked || self.revoker.is_revoked() {
            return Task::none(); // drops returning journal owner, never revives authorization
        }
        if !matches!(&event.result, ResultEvent::Signer(_)) {
            self.busy = false;
        }
        match event.result {
            ResultEvent::Signer(message) => self.signer_result(*message, daemon, cache),
            ResultEvent::Signing {
                mut preparation,
                pending,
                result,
            } => {
                let dispatch = result.and_then(|check| {
                    let psbt = self
                        .psbt
                        .as_ref()
                        .ok_or_else(|| SESSION_ENDED.to_string())?;
                    let current =
                        UnifiedPsbt::from_psbt(psbt.tx.psbt.clone()).map_err(|e| e.to_string())?;
                    preparation
                        .signing_dispatch(check, &current, &self.context)
                        .map_err(describe)
                });
                self.preparation = Some(preparation);
                match (dispatch, daemon, self.psbt.as_mut()) {
                    (Ok(dispatch), Some(daemon), Some(psbt)) => {
                        if !psbt.set_claim_split_evidence(dispatch.split) {
                            self.error = Some(SESSION_ENDED.into());
                            return Task::none();
                        }
                        let task =
                            psbt.update(Arc::new(SigningOnlyDaemon(daemon)), cache, *pending);
                        Self::route_signer(self.epoch, task)
                    }
                    (Err(error), _, _) => {
                        self.error = Some(error);
                        Task::none()
                    }
                    _ => {
                        self.error = Some(SESSION_ENDED.into());
                        Task::none()
                    }
                }
            }
            ResultEvent::Reviewed {
                coordinator,
                result,
            } => {
                self.coordinator = Some(coordinator);
                match result {
                    Ok(review) => self.review = Some(review),
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            ResultEvent::Submitted {
                coordinator,
                result,
            } => {
                // Even a returned error after durable intent switches to
                // tracking; outcome never means network confirmation.
                self.outcome = coordinator.recorded_outcome();
                self.coordinator = Some(coordinator);
                match result {
                    Ok(outcome) => self.outcome = Some(outcome),
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            ResultEvent::Tracked {
                coordinator,
                result,
            } => {
                self.coordinator = Some(coordinator);
                match result {
                    Ok(tracking) => self.tracking = Some(tracking),
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
        }
    }
}
impl Drop for ForkClaimPanel {
    fn drop(&mut self) {
        self.revoke();
    }
}
impl State for ForkClaimPanel {
    fn update(
        &mut self,
        daemon: Option<Arc<dyn Daemon + Send + Sync>>,
        cache: &Cache,
        message: Message,
    ) -> Task<Message> {
        if let Message::ForkClaim(event) = message {
            return self.apply(*event, daemon, cache);
        }
        if matches!(
            &message,
            Message::View(view::Message::Claim(view::ClaimMessage::Cancel))
        ) || (self.busy
            && matches!(
                &message,
                Message::View(view::Message::Spend(view::SpendTxMessage::Cancel))
            ))
        {
            self.revoke();
            self.error = Some("Claim paused. Reopen Claim to check both chains again.".into());
            return Task::none();
        }
        if !self.ready() {
            return Task::none();
        }
        match message {
            Message::View(view::Message::Claim(view::ClaimMessage::Confirm)) => {
                if self.review.is_none() || self.coordinator.is_none() {
                    return Task::none();
                }
                self.confirm()
            }
            Message::View(view::Message::Claim(view::ClaimMessage::Refresh)) => {
                if self.outcome.is_some() {
                    self.track()
                } else {
                    self.prepare_review()
                }
            }
            Message::View(view::Message::Claim(view::ClaimMessage::Cancel)) => {
                self.revoke();
                Task::none()
            }
            message if claim_signing_dispatch(&message) => self.check_signer(message),
            message => {
                let (Some(psbt), Some(daemon)) = (&mut self.psbt, daemon) else {
                    return Task::none();
                };
                let task = psbt.update(Arc::new(SigningOnlyDaemon(daemon)), cache, message);
                Task::batch([Self::route_signer(self.epoch, task), self.finish_signing()])
            }
        }
    }
    fn subscription(&self) -> Subscription<Message> {
        self.psbt.as_ref().map_or_else(Subscription::none, |psbt| {
            let epoch = self.epoch;
            psbt.subscription()
                .with(epoch)
                .map(|(epoch, message)| Self::event(epoch, ResultEvent::Signer(Box::new(message))))
        })
    }
    fn interrupt(&mut self) {
        if let Some(psbt) = &mut self.psbt {
            psbt.interrupt();
        }
    }
    fn reload(
        &mut self,
        daemon: Option<Arc<dyn Daemon + Send + Sync>>,
        _wallet: Option<Arc<Wallet>>,
    ) -> Task<Message> {
        if !self.ready() {
            return Task::none();
        }
        if self.outcome.is_some() {
            return self.track();
        }
        if let (Some(psbt), Some(daemon)) = (&self.psbt, daemon) {
            return Self::route_signer(self.epoch, psbt.load(Arc::new(SigningOnlyDaemon(daemon))));
        }
        self.prepare_review()
    }
    fn view<'a>(&'a self, menu: &'a Menu, cache: &'a Cache) -> Element<'a, view::Message> {
        use view::{ClaimMessage, Message as V};
        let mut body = Column::new()
            .spacing(20)
            .push(h3("Claim Bitcoin Blake2b — step 2").bold())
            .push_maybe(self.error.as_ref().map(|e| card::warning(e.clone())))
            .push_maybe(self.busy.then(|| p1_regular("Checking both chains…")))
            .push(
                button::secondary(None, "Return to Bitcoin Claim").on_press_maybe(
                    self.can_return_to_bitcoin()
                        .then_some(V::ReturnBitcoinClaim),
                ),
            );
        if let Some(psbt) = &self.psbt {
            body = body.push(p1_regular("Sign the transfer to your Bitcoin Blake2b Vault. Each signing request checks Bitcoin confirmation and replay protection again."))
                .push(view::vault::psbt::spend_overview_view(&psbt.tx, &psbt.desc_policy, &psbt.wallet.keys_aliases,
                    self.busy, psbt.saved, psbt.replay_presentation(cache), false));
            let content = view::dashboard(menu, cache, body);
            return if let Some(modal) = &psbt.modal {
                modal.as_ref().view(content)
            } else {
                content
            };
        }
        if self.outcome.is_some() {
            let text = match &self.tracking {
                Some(Tracking { saved: true, .. }) => "Claim confirmed on Bitcoin Blake2b. Both Cubes have been updated; confirmations remain subject to reorgs.",
                Some(Tracking { transaction: TransactionObservation::Confirmed { .. }, .. }) => "The fork transfer is confirmed. Bitcoin protection or completion recording still needs checking.",
                Some(Tracking { transaction: TransactionObservation::Unconfirmed { .. }, .. }) => "The fork transfer is waiting for confirmation.",
                Some(Tracking { transaction: TransactionObservation::Absent, .. }) => "The recorded transfer is not currently visible on Bitcoin Blake2b. Check again; it will not be submitted again.",
                None => "A submission is recorded. Check both chains for its current status; it will not be submitted again.",
            };
            body = body.push(p1_regular(text));
            if let Some(Tracking {
                status: Status::Observation(coincube_core::claim::Assessment::Reorged),
                ..
            }) = &self.tracking
            {
                body = body.push(card::warning(
                    "Bitcoin confirmation changed. Claim completion is no longer current.".into(),
                ));
            }
        } else if let Some(review) = &self.review {
            let snapshot = review.snapshot();
            for output in &snapshot.transaction.output {
                let destination = coincube_core::miniscript::bitcoin::Address::from_script(
                    &output.script_pubkey,
                    coincube_core::miniscript::bitcoin::Network::Bitcoin,
                )
                .map(|a| a.to_string())
                .unwrap_or_else(|_| "Unrecognized destination".into());
                body = body.push(p1_regular(format!(
                    "{} sat to {}",
                    output.value.to_sat(),
                    destination
                )));
            }
            body = body.push(p1_regular(format!("Transaction: {}", snapshot.txid)))
                .push(p1_regular(format!("{} inputs · fee {} sat · {} vB", snapshot.transaction.input.len(), snapshot.fee_sats, snapshot.vsize)))
                .push(p1_regular("Confirm to submit this transfer to Bitcoin Blake2b. This cannot be undone. Both chains and node acceptance will be checked again."))
                .push(button::primary(None, "Submit to Bitcoin Blake2b").on_press_maybe(self.ready().then_some(V::Claim(ClaimMessage::Confirm))));
        }
        body = body.push(
            button::secondary(None, "Check again")
                .on_press_maybe(self.ready().then_some(V::Claim(ClaimMessage::Refresh))),
        );
        view::dashboard(menu, cache, body)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use iced::futures::StreamExt;
    #[cfg(feature = "regtest-harness")]
    pub(crate) fn live_psbt(
        panel: &ForkClaimPanel,
    ) -> coincube_core::miniscript::bitcoin::psbt::Psbt {
        panel
            .psbt
            .as_ref()
            .expect("fork signing state")
            .tx
            .psbt
            .clone()
    }
    #[cfg(feature = "regtest-harness")]
    pub(crate) fn live_snapshot(panel: &ForkClaimPanel) -> serde_json::Value {
        serde_json::json!({"busy":panel.busy, "error":panel.error,
            "review":panel.review.is_some(), "outcome":panel.outcome.map(|o| format!("{o:?}")),
            "tracking":panel.tracking.as_ref().map(|t| serde_json::json!({
                "status":format!("{:?}",t.status), "transaction":format!("{:?}",t.transaction), "saved":t.saved}))})
    }
    async fn outputs(task: Task<Message>) -> Vec<Message> {
        let mut messages = Vec::new();
        if let Some(mut stream) = iced_runtime::task::into_stream(task) {
            while let Some(action) = stream.next().await {
                if let iced_runtime::Action::Output(message) = action {
                    messages.push(message);
                }
            }
        }
        messages
    }
    pub(crate) async fn refused_signer_and_late_result(
        root: CoincubeDirectory,
        wallet: Arc<Wallet>,
        coins: Vec<Coin>,
        loaded: Loaded,
        daemon: Arc<dyn Daemon + Send + Sync>,
        cache: &Cache,
    ) -> UnifiedPsbt {
        let mut panel = ForkClaimPanel::new(root, wallet, coins, loaded).unwrap();
        let original =
            UnifiedPsbt::from_psbt(panel.psbt.as_ref().unwrap().tx.psbt.clone()).unwrap();
        // Results belonging to another panel cannot affect this one, including
        // a panel reopened for the very same transaction.
        let foreign = ForkClaimPanel::event(
            panel.epoch.wrapping_add(100),
            ResultEvent::Signer(Box::new(Message::View(view::Message::ShowError(
                "foreign panel".into(),
            )))),
        );
        assert!(outputs(panel.update(Some(daemon.clone()), cache, foreign))
            .await
            .is_empty());
        assert!(panel.error.is_none());
        let sign = || {
            Message::View(view::Message::Spend(
                view::SpendTxMessage::SelectMasterSigner,
            ))
        };
        let task = panel.update(Some(daemon.clone()), cache, sign());
        assert!(panel.busy);
        assert!(panel.preparation.is_none()); // async task owns the journal
        assert!(!panel.can_return_to_bitcoin());
        assert!(outputs(panel.update(Some(daemon.clone()), cache, sign()))
            .await
            .is_empty());
        let messages = outputs(task).await;
        assert_eq!(messages.len(), 1);
        for message in messages {
            assert!(outputs(panel.update(Some(daemon.clone()), cache, message))
                .await
                .is_empty());
        }
        assert!(!panel.busy);
        assert!(panel.preparation.is_some());
        assert!(panel
            .error
            .as_ref()
            .unwrap()
            .contains("WaitingForConfirmation"));
        assert_eq!(&panel.psbt.as_ref().unwrap().tx.psbt, original.psbt());
        assert!(panel.review.is_none());
        assert!(panel.coordinator.is_none());
        let task = panel.update(Some(daemon.clone()), cache, sign());
        let cancel = Message::View(view::Message::Spend(view::SpendTxMessage::Cancel));
        assert!(outputs(panel.update(Some(daemon.clone()), cache, cancel))
            .await
            .is_empty());
        assert!(panel.revoked);
        assert!(!panel.can_return_to_bitcoin());
        for message in outputs(task).await {
            assert!(outputs(panel.update(Some(daemon.clone()), cache, message))
                .await
                .is_empty());
        }
        assert!(panel.preparation.is_none());
        assert!(panel.review.is_none());
        assert!(panel.can_return_to_bitcoin());
        assert_eq!(&panel.psbt.as_ref().unwrap().tx.psbt, original.psbt());
        original
    }
    pub(crate) async fn recorded_submission_cannot_sign_or_confirm(
        root: CoincubeDirectory,
        wallet: Arc<Wallet>,
        loaded: Loaded,
        daemon: Arc<dyn Daemon + Send + Sync>,
        cache: &Cache,
    ) {
        let mut panel = ForkClaimPanel::new(root, wallet, Vec::new(), loaded).unwrap();
        assert!(panel.outcome.is_some());
        assert!(panel.coordinator.is_some());
        assert!(panel.psbt.is_none());
        for message in [
            Message::View(view::Message::Claim(view::ClaimMessage::Confirm)),
            Message::View(view::Message::Spend(
                view::SpendTxMessage::SelectMasterSigner,
            )),
            Message::View(view::Message::Spend(view::SpendTxMessage::Broadcast)),
        ] {
            assert!(outputs(panel.update(Some(daemon.clone()), cache, message))
                .await
                .is_empty());
            assert!(panel.coordinator.is_some());
            assert!(panel.review.is_none());
            assert!(panel.psbt.is_none());
        }
        // An attempted stale confirmation also must not drop the journal owner.
        assert!(outputs(panel.confirm()).await.is_empty());
        assert!(panel.coordinator.is_some());
        let task = panel.track();
        panel.revoke();
        for message in outputs(task).await {
            assert!(outputs(panel.update(Some(daemon.clone()), cache, message))
                .await
                .is_empty());
        }
        assert!(panel.tracking.is_none());
        assert!(panel.coordinator.is_none());
    }
    pub(crate) async fn signed_panel_awaiting_review(
        root: CoincubeDirectory,
        wallet: Arc<Wallet>,
        loaded: Loaded,
        daemon: Arc<dyn Daemon + Send + Sync>,
        cache: &Cache,
        second: coincube_core::signer::MasterSigner,
    ) -> (ForkClaimPanel, Task<Message>, UnifiedPsbt) {
        let mut panel = ForkClaimPanel::new(root, wallet, Vec::new(), loaded).unwrap();
        // The normal picker is used; discovery is unnecessary in this hot-key
        // fixture, so its initial enumeration task is deliberately unpolled.
        drop(panel.update(
            Some(daemon.clone()),
            cache,
            Message::View(view::Message::Spend(view::SpendTxMessage::Sign)),
        ));
        let task = panel.update(
            Some(daemon.clone()),
            cache,
            Message::View(view::Message::Spend(
                view::SpendTxMessage::SelectMasterSigner,
            )),
        );
        let mut pending: std::collections::VecDeque<_> = outputs(task).await.into();
        let mut count = 0;
        while let Some(message) = pending.pop_front() {
            count += 1;
            assert!(count < 20);
            pending.extend(outputs(panel.update(Some(daemon.clone()), cache, message)).await);
        }
        assert!(panel.error.is_none(), "{:?}", panel.error);
        assert!(panel.preparation.is_some());
        assert!(panel.review.is_none()); // 2-of-3, one hot signature is insufficient
        let psbt = panel.psbt.as_ref().unwrap();
        assert!(psbt.tx.path_ready().is_none());
        let curve = secp256k1::Secp256k1::new();
        let signed = second.sign_psbt(psbt.tx.psbt.clone(), &curve).unwrap();
        let signed = UnifiedPsbt::from_psbt(signed).unwrap();
        let task = panel.update(
            Some(daemon.clone()),
            cache,
            Message::Signed(second.fingerprint(&curve), Ok(signed.psbt().clone())),
        );
        let messages = outputs(task).await;
        assert_eq!(messages.len(), 1); // signed PSBT persistence is a Claim-scoped no-op
        let review_task = panel.update(Some(daemon), cache, messages.into_iter().next().unwrap());
        assert!(panel.psbt.is_none());
        assert!(panel.busy);
        assert!(panel.review.is_none()); // review has not been polled yet
        (panel, review_task, signed)
    }
    pub(crate) async fn review_and_submit_once(
        mut panel: ForkClaimPanel,
        review_task: Task<Message>,
        daemon: Arc<dyn Daemon + Send + Sync>,
        cache: &Cache,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use std::sync::atomic::Ordering;
        for message in outputs(review_task).await {
            assert!(outputs(panel.update(Some(daemon.clone()), cache, message))
                .await
                .is_empty());
        }
        assert!(panel.review.is_some(), "{:?}", panel.error);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let task = panel.update(
            Some(daemon.clone()),
            cache,
            Message::View(view::Message::Claim(view::ClaimMessage::Confirm)),
        );
        for message in outputs(task).await {
            assert!(outputs(panel.update(Some(daemon.clone()), cache, message))
                .await
                .is_empty());
        }
        assert!(panel.outcome.is_some(), "{:?}", panel.error);
        assert!(panel.review.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(outputs(panel.update(
            Some(daemon),
            cache,
            Message::View(view::Message::Claim(view::ClaimMessage::Confirm))
        ))
        .await
        .is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
