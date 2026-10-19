//! Recollect ancestry and owned-input state before opening any signer.
use super::*;
use coincube_core::miniscript::bitcoin::hashes::sha256;
use std::collections::BTreeSet;

/// Recover only the saved unsigned Claim pair from a new two-chain Vault
/// scan. The signed journal supplies exact funding outpoints and fee rates;
/// it does not restore ancestry proof, complete-wallet reconciliation or
/// permission to sign or send. A later review must run
/// `prepare_exact_pair_intent` and reverify both saved witnesses.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // The resumed paired broadcast review will use this.
pub(super) async fn restore_recorded_initial_pair(
    directory: PathBuf,
    bitcoin_cube: String,
    fork_cube: String,
    wallet: Arc<Wallet>,
    daemon: Arc<dyn Daemon + Send + Sync>,
    fork_binding: ClaimForkBinding,
    connect: ConnectSession,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<PreparedClaimPair, String> {
    let current = || {
        if !crate::services::claim_observation::session_current(&generation, expected) {
            Err("The Connect session changed during Claim recovery.".to_string())
        } else {
            Ok(())
        }
    };
    current()?;
    let descriptor = wallet.main_descriptor.clone();
    let fork_wallet = fork_binding
        .wallet()
        .ok_or_else(|| "The paired BTCB2 Vault closed during Claim recovery.".to_string())?;
    if wallet.chain != ChainId::Bitcoin
        || !fork_binding.current()
        || fork_wallet.chain != ChainId::BitcoinBlake2b
        || fork_wallet.main_descriptor != descriptor
    {
        return Err("The paired Bitcoin and BTCB2 Vaults changed. Reopen both Cubes.".into());
    }
    let context = ClaimStep1Panel::funding_context(&connect, expected, generation.clone())?;
    let identity = claim_workflow::WalletIdentity {
        bitcoin_cube,
        fork_cube,
        descriptor_digest: sha256::Hash::hash(descriptor.to_string().as_bytes()),
    };
    let mut controller =
        claim_workflow::Controller::reopen_settling(&directory, &identity, context.clone())
            .await
            .map_err(|error| {
                format!("The signed Claim journal could not be reopened: {error:?}")
            })?;
    let plan = controller
        .recorded_initial_pair_plan(&context)
        .map_err(|error| {
            format!("This signed Claim cannot be prepared for a first send: {error:?}")
        })?;
    drop(controller);
    let info = daemon
        .get_info()
        .await
        .map_err(|_| "The Bitcoin Vault is unavailable for Claim recovery.".to_string())?;
    if !bitcoin_funding_wallet_ready(&info, &descriptor) {
        return Err("The Bitcoin Vault is not ready for Claim recovery.".into());
    }
    let end_exclusive = info
        .receive_index
        .max(info.change_index)
        .saturating_add(128)
        .max(256);
    if end_exclusive > 20_000 {
        return Err("The Vault's address range is too large for this bounded scan.".into());
    }
    let inventory = crate::services::claim_inventory::ClaimInventory::scan(
        connect.client,
        &descriptor,
        end_exclusive,
        64,
        expected,
        generation.clone(),
    )
    .await
    .map_err(|error| inventory_scan_refusal(error, ScanRetry::Manual))?;
    current()?;
    if !fork_binding.current() {
        return Err("The paired BTCB2 Vault changed during Claim recovery.".into());
    }
    let mut controller =
        claim_workflow::Controller::reopen_settling(&directory, &identity, context.clone())
            .await
            .map_err(|error| format!("The signed Claim journal changed: {error:?}"))?;
    if controller
        .recorded_initial_pair_plan(&context)
        .map_err(|error| format!("The signed Claim pair changed: {error:?}"))?
        != plan
    {
        return Err("The signed Claim pair changed during the scan.".into());
    }
    let (bitcoin, fork) = inventory
        .restore_unattempted_claim_pair_from_scan(&mut controller, &context, &descriptor)
        .map_err(|error| format!("The saved Claim pair no longer matches this Vault: {error:?}"))?;
    drop(controller);
    Ok(PreparedClaimPair {
        bitcoin,
        fork,
        bitcoin_funding: plan.bitcoin_funding,
        fork_funding: plan.fork_funding,
        bitcoin_rate_vb: plan.bitcoin_rate_vb,
        fork_rate_vb: plan.fork_rate_vb,
    })
}

/// Reconstruct, requalify and reverify a saved unattempted Claim pair under
/// one live journal controller. This returns a short-lived review; it does
/// not perform Connect preflight, record an attempt or send a transaction.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // The paired recovery screen will consume this boundary.
pub(super) async fn prepare_recorded_initial_review(
    directory: PathBuf,
    bitcoin_cube: String,
    fork_cube: String,
    wallet: Arc<Wallet>,
    daemon: Arc<dyn Daemon + Send + Sync>,
    fork_binding: ClaimForkBinding,
    connect: ConnectSession,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<
    (
        claim_workflow::Controller,
        claim_workflow::InitialPairReview,
    ),
    String,
> {
    let pair = restore_recorded_initial_pair(
        directory.clone(),
        bitcoin_cube.clone(),
        fork_cube.clone(),
        wallet.clone(),
        daemon.clone(),
        fork_binding.clone(),
        connect.clone(),
        expected,
        generation.clone(),
    )
    .await?;
    let requalified = prepare_exact_pair_intent(
        pair.clone(),
        directory.clone(),
        bitcoin_cube.clone(),
        fork_cube.clone(),
        wallet.clone(),
        daemon,
        fork_binding.clone(),
        connect.clone(),
        expected,
        generation.clone(),
    )
    .await?;
    if !fork_binding.current()
        || !crate::services::claim_observation::session_current(&generation, expected)
    {
        return Err("The paired BTCB2 Vault or Connect session changed before review.".into());
    }
    let context = ClaimStep1Panel::funding_context(&connect, expected, generation.clone())?;
    let identity = claim_workflow::WalletIdentity {
        bitcoin_cube,
        fork_cube,
        descriptor_digest: sha256::Hash::hash(wallet.main_descriptor.to_string().as_bytes()),
    };
    let mut controller =
        claim_workflow::Controller::reopen_settling(&directory, &identity, context.clone())
            .await
            .map_err(|error| format!("The signed Claim journal changed: {error:?}"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "The current time is unavailable for Claim review.".to_string())?
        .as_secs() as i64;
    let review = requalified
        .prepare_recorded_pair_review(
            &mut controller,
            &pair.bitcoin,
            &pair.fork,
            CHECK_POLICY.observations,
            now,
        )
        .map_err(|error| format!("The signed Claim pair is no longer verified: {error:?}"))?;
    if !fork_binding.current()
        || !crate::services::claim_observation::session_current(&generation, expected)
    {
        return Err(
            "The paired BTCB2 Vault or Connect session changed during Claim review.".into(),
        );
    }
    Ok((controller, review))
}

/// An explicit Bitcoin broadcast always reconstructs the saved Claim pair
/// and repeats the live two-chain proof. The displayed bytes and fees must
/// still match exactly before preflight or a possible-send journal write.
#[allow(clippy::too_many_arguments)]
pub(super) async fn confirm_recorded_first_broadcast(
    directory: PathBuf,
    bitcoin_cube: String,
    fork_cube: String,
    wallet: Arc<Wallet>,
    daemon: Arc<dyn Daemon + Send + Sync>,
    fork_binding: ClaimForkBinding,
    connect: ConnectSession,
    expected: u64,
    generation: watch::Receiver<u64>,
    displayed: claim_workflow::InitialPairReviewSnapshot,
) -> Result<claim_workflow::InitialPairSubmissionOutcome, String> {
    if crate::services::claim_ancestry_gate::authorization().is_none() {
        return Err("The paired claim is not ready for broadcast.".into());
    }
    let (mut controller, review) = prepare_recorded_initial_review(
        directory,
        bitcoin_cube,
        fork_cube,
        wallet,
        daemon,
        fork_binding,
        connect.clone(),
        expected,
        generation.clone(),
    )
    .await?;
    let context = ClaimStep1Panel::funding_context(&connect, expected, generation.clone())?;
    let preflight = crate::services::claim_preflight::PreflightClient::new(
        &connect.client.base_url,
        CollectionContext {
            expected_generation: expected,
            generation,
        },
    )
    .map_err(|error| format!("The Connect broadcast check is unavailable: {error:?}"))?;
    review
        .confirm_exact_snapshot_and_submit(
            &displayed,
            &mut controller,
            &context,
            &preflight,
            CHECK_POLICY.preflight,
        )
        .await
        .map_err(|error| format!("The Bitcoin broadcast was refused: {error:?}"))
}

/// A recorded Bitcoin attempt is only a hint to start the distinct stage-two
/// check. Scan both chains again, reconstruct the saved pair from source-owned
/// bytes, reconcile the complete BTCB2 Vault and independently verify the
/// canonical six-confirmation Bitcoin witness before offering review.
#[allow(clippy::too_many_arguments)]
pub(super) async fn prepare_recorded_post_bitcoin_review(
    directory: PathBuf,
    bitcoin_cube: String,
    fork_cube: String,
    wallet: Arc<Wallet>,
    daemon: Arc<dyn Daemon + Send + Sync>,
    fork_binding: ClaimForkBinding,
    connect: ConnectSession,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<
    (
        claim_workflow::Controller,
        claim_workflow::PostBitcoinPairReview,
    ),
    String,
> {
    let current_session = || {
        if !crate::services::claim_observation::session_current(&generation, expected) {
            Err("The Connect session changed during BTCB2 review.".to_string())
        } else {
            Ok(())
        }
    };
    current_session()?;
    let descriptor = wallet.main_descriptor.clone();
    let fork_wallet = fork_binding
        .wallet()
        .ok_or_else(|| "The paired BTCB2 Vault closed during review.".to_string())?;
    let fork_daemon = fork_binding
        .upgrade()
        .ok_or_else(|| "The paired BTCB2 Vault closed during review.".to_string())?;
    if wallet.chain != ChainId::Bitcoin
        || fork_wallet.chain != ChainId::BitcoinBlake2b
        || fork_wallet.main_descriptor != descriptor
        || !fork_binding.current()
    {
        return Err("The paired Bitcoin and BTCB2 Vaults changed. Reopen both Cubes.".into());
    }
    let context = ClaimStep1Panel::funding_context(&connect, expected, generation.clone())?;
    let identity = claim_workflow::WalletIdentity {
        bitcoin_cube,
        fork_cube,
        descriptor_digest: sha256::Hash::hash(descriptor.to_string().as_bytes()),
    };
    let mut controller =
        claim_workflow::Controller::reopen_settling(&directory, &identity, context.clone())
            .await
            .map_err(|error| {
                format!("The signed Claim journal could not be reopened: {error:?}")
            })?;
    let plan = controller
        .recorded_post_bitcoin_pair_plan(&context)
        .map_err(|error| format!("The BTCB2 send cannot be prepared: {error:?}"))?;
    drop(controller);
    let info = daemon
        .get_info()
        .await
        .map_err(|_| "The Bitcoin Vault is unavailable for the two-chain scan.".to_string())?;
    if !bitcoin_funding_wallet_ready(&info, &descriptor) {
        return Err("The Bitcoin Vault is not ready for BTCB2 review.".into());
    }
    let fork_info = fork_daemon
        .get_info()
        .await
        .map_err(|_| "The BTCB2 Vault is unavailable for the two-chain scan.".to_string())?;
    if fork_info.chain_id != Some(ChainId::BitcoinBlake2b)
        || fork_daemon
            .config()
            .is_none_or(|config| config.main_descriptor != descriptor)
    {
        return Err("The paired BTCB2 Vault is not ready for review.".into());
    }
    let end_exclusive = info
        .receive_index
        .max(info.change_index)
        .max(fork_info.receive_index)
        .max(fork_info.change_index)
        .saturating_add(128)
        .max(256);
    if end_exclusive > 20_000 {
        return Err("The Vault's address range is too large for this bounded scan.".into());
    }
    let inventory = crate::services::claim_inventory::ClaimInventory::scan(
        connect.client.clone(),
        &descriptor,
        end_exclusive,
        64,
        expected,
        generation.clone(),
    )
    .await
    .map_err(|_| "The post-Bitcoin two-chain Vault scan did not complete.".to_string())?;
    current_session()?;
    if !fork_binding.current() {
        return Err("The paired BTCB2 Vault changed during the scan.".into());
    }
    let saved_directory = directory.clone();
    let saved_descriptor = descriptor.clone();
    let saved_identity = identity.clone();
    let saved_context = context.clone();
    let reservation = tokio::task::spawn_blocking(move || {
        claim_workflow::load_claim_funding(
            &saved_directory,
            &saved_descriptor,
            &saved_identity,
            &saved_context,
        )
    })
    .await
    .map_err(|_| "Reading the saved Claim deposits was interrupted.".to_string())?
    .map_err(|_| "The saved Claim deposits could not be verified.".to_string())?
    .ok_or_else(|| "The saved Claim deposits are missing.".to_string())?;
    let source = HttpObservationSource::new(
        connect.client.clone(),
        ChainId::Bitcoin,
        ChainId::BitcoinBlake2b,
        CollectionContext {
            expected_generation: expected,
            generation: generation.clone(),
        },
    )
    .map_err(|_| "The two-chain observation source is unavailable.".to_string())?;
    let observed = source
        .collect_post_bitcoin_inputs(
            &plan.bitcoin_path,
            &plan.fork_path,
            &plan.shared,
            &plan.signed_bitcoin,
            CHECK_POLICY.observations,
        )
        .await
        .map_err(|error| format!("The Bitcoin confirmation or BTCB2 inputs changed: {error:?}"))?;
    let mut controller =
        claim_workflow::Controller::reopen_settling(&directory, &identity, context.clone())
            .await
            .map_err(|error| format!("The signed Claim journal changed: {error:?}"))?;
    let current_plan = controller
        .recorded_post_bitcoin_pair_plan(&context)
        .map_err(|error| format!("The recorded Claim pair changed: {error:?}"))?;
    if current_plan.signed_bitcoin != plan.signed_bitcoin
        || current_plan.shared != plan.shared
        || current_plan.bitcoin_path.selected() != plan.bitcoin_path.selected()
        || current_plan.fork_path.selected() != plan.fork_path.selected()
        || current_plan.bitcoin_path.encode() != plan.bitcoin_path.encode()
        || current_plan.fork_path.encode() != plan.fork_path.encode()
        || current_plan.bitcoin_rate_vb != plan.bitcoin_rate_vb
        || current_plan.fork_rate_vb != plan.fork_rate_vb
    {
        return Err("The signed Claim pair changed during the scan.".into());
    }
    let (bitcoin, fork) = inventory
        .restore_post_bitcoin_claim_pair_from_scan(
            &mut controller,
            &context,
            &plan,
            observed.shared_blocks(),
            reservation.indices.bitcoin_deposit,
            reservation.indices.btcb2_deposit,
        )
        .map_err(|error| format!("The saved Claim pair no longer matches the scan: {error:?}"))?;
    drop(controller);
    let requalified = reservation
        .requalify_post_bitcoin_fork(
            directory.clone(),
            &inventory,
            &identity,
            &context,
            &*fork_daemon,
            &source,
            CHECK_POLICY.observations,
            &bitcoin,
            &fork,
            &plan.signed_bitcoin,
            &plan.bitcoin_path,
            &plan.fork_path,
        )
        .await
        .map_err(|error| format!("The BTCB2 Claim inputs are no longer verified: {error:?}"))?;
    current_session()?;
    if !fork_binding.current() {
        return Err("The paired BTCB2 Vault changed during verification.".into());
    }
    let mut controller =
        claim_workflow::Controller::reopen_settling(&directory, &identity, context.clone())
            .await
            .map_err(|error| format!("The signed Claim journal changed: {error:?}"))?;
    let reconciliation = controller
        .reconcile_possible_ancestry_attempts(
            &context,
            &source,
            CHECK_POLICY.observations.max_observation_age_seconds,
        )
        .await
        .map_err(|error| format!("Bitcoin confirmation could not be checked: {error:?}"))?;
    let confirmation = reconciliation
        .bitcoin_confirmation()
        .cloned()
        .ok_or_else(|| {
            "The exact Bitcoin transaction needs six current canonical confirmations.".to_string()
        })?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "The current time is unavailable for BTCB2 review.".to_string())?
        .as_secs() as i64;
    let review = controller
        .prepare_post_bitcoin_claim_review(
            &context,
            &requalified,
            confirmation,
            CHECK_POLICY.observations,
            now,
        )
        .map_err(|error| format!("The BTCB2 Claim pair is no longer verified: {error:?}"))?;
    current_session()?;
    if !fork_binding.current() {
        return Err("The paired BTCB2 Vault changed during review.".into());
    }
    Ok((controller, review))
}

/// A BTCB2 approval repeats the complete post-Bitcoin check and compares the
/// exact finalized pair shown to the user before any policy read or durable
/// possible-send record. Any outcome after recording is reconciled, not
/// silently retried.
#[allow(clippy::too_many_arguments)]
pub(super) async fn confirm_recorded_second_broadcast(
    directory: PathBuf,
    bitcoin_cube: String,
    fork_cube: String,
    wallet: Arc<Wallet>,
    daemon: Arc<dyn Daemon + Send + Sync>,
    fork_binding: ClaimForkBinding,
    connect: ConnectSession,
    expected: u64,
    generation: watch::Receiver<u64>,
    displayed: claim_workflow::InitialPairReviewSnapshot,
) -> Result<claim_workflow::InitialPairSubmissionOutcome, String> {
    if crate::services::claim_ancestry_gate::authorization().is_none() {
        return Err("The paired BTCB2 claim is not ready for broadcast.".into());
    }
    let (mut controller, review) = prepare_recorded_post_bitcoin_review(
        directory,
        bitcoin_cube,
        fork_cube,
        wallet,
        daemon,
        fork_binding,
        connect.clone(),
        expected,
        generation.clone(),
    )
    .await?;
    let context = ClaimStep1Panel::funding_context(&connect, expected, generation.clone())?;
    let preflight = crate::services::claim_preflight::PreflightClient::new(
        &connect.client.base_url,
        CollectionContext {
            expected_generation: expected,
            generation,
        },
    )
    .map_err(|error| format!("The Connect BTCB2 broadcast check is unavailable: {error:?}"))?;
    review
        .confirm_exact_snapshot_and_submit(
            &displayed,
            &mut controller,
            &context,
            &preflight,
            CHECK_POLICY.preflight,
        )
        .await
        .map_err(|error| format!("The BTCB2 broadcast was refused: {error:?}"))
}

/// Recollect the exact ordinary pair from a new two-chain scan and both
/// complete Vault coin sets before recording its version-10 intent. A caller
/// must repeat this at each signer dispatch: the journal is structural state,
/// never spend authority. No transaction is signed or sent here.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // The paired signer UI is not yet wired.
pub(super) async fn prepare_exact_pair_intent(
    pair: PreparedClaimPair,
    directory: PathBuf,
    bitcoin_cube: String,
    fork_cube: String,
    wallet: Arc<Wallet>,
    daemon: Arc<dyn Daemon + Send + Sync>,
    fork_binding: ClaimForkBinding,
    connect: ConnectSession,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<claim_workflow::RequalifiedInitialClaim, String> {
    let current = || {
        if !crate::services::claim_observation::session_current(&generation, expected) {
            Err("The Connect session changed during Claim preparation.".to_string())
        } else {
            Ok(())
        }
    };
    current()?;
    if wallet.chain != ChainId::Bitcoin || !fork_binding.current() {
        return Err("The paired Bitcoin and BTCB2 Vaults are not open for this Claim.".into());
    }
    let fork_daemon = fork_binding
        .upgrade()
        .ok_or_else(|| "The paired BTCB2 Vault closed during Claim preparation.".to_string())?;
    let fork_wallet = fork_binding
        .wallet()
        .ok_or_else(|| "The paired BTCB2 Vault closed during Claim preparation.".to_string())?;
    let descriptor = wallet.main_descriptor.clone();
    if fork_wallet.chain != ChainId::BitcoinBlake2b
        || fork_wallet.main_descriptor != descriptor
        || pair.bitcoin.descriptor() != &descriptor
        || pair.fork.descriptor() != &descriptor
    {
        return Err("The paired Vault descriptors changed. Reopen both Cubes.".into());
    }
    let context = ClaimStep1Panel::funding_context(&connect, expected, generation.clone())?;
    let identity = claim_workflow::WalletIdentity {
        bitcoin_cube: bitcoin_cube.clone(),
        fork_cube: fork_cube.clone(),
        descriptor_digest: sha256::Hash::hash(descriptor.to_string().as_bytes()),
    };
    let info = daemon
        .get_info()
        .await
        .map_err(|_| "The Bitcoin Vault is unavailable for Claim preparation.".to_string())?;
    if !bitcoin_funding_wallet_ready(&info, &descriptor) {
        return Err("The Bitcoin Vault is not ready for Claim preparation.".into());
    }
    let end_exclusive = info
        .receive_index
        .max(info.change_index)
        .saturating_add(128)
        .max(256);
    if end_exclusive > 20_000 {
        return Err("The Vault's address range is too large for this bounded scan.".into());
    }
    let inventory = crate::services::claim_inventory::ClaimInventory::scan(
        connect.client.clone(),
        &descriptor,
        end_exclusive,
        64,
        expected,
        generation.clone(),
    )
    .await
    .map_err(|error| inventory_scan_refusal(error, ScanRetry::Manual))?;
    current()?;
    if !fork_binding.current() {
        return Err("The paired BTCB2 Vault changed during Claim preparation.".into());
    }
    let saved_directory = directory.clone();
    let saved_descriptor = descriptor.clone();
    let saved_identity = identity.clone();
    let saved_context = context.clone();
    let reservation = tokio::task::spawn_blocking(move || {
        claim_workflow::load_claim_funding(
            &saved_directory,
            &saved_descriptor,
            &saved_identity,
            &saved_context,
        )
    })
    .await
    .map_err(|_| "Reading the saved Claim deposits was interrupted.".to_string())?
    .map_err(|_| "The saved Claim deposits could not be verified.".to_string())?
    .ok_or_else(|| "The saved Claim deposits are missing.".to_string())?;
    let (minimums, rates) = claim_workflow::quote_claim_deposit_terms(
        &descriptor,
        reservation.indices,
        inventory.shared().len(),
        connect.client.clone(),
    )
    .await
    .map_err(|_| "Current Bitcoin and BTCB2 fee quotes are unavailable.".to_string())?;
    current()?;
    if rates != (pair.bitcoin_rate_vb, pair.fork_rate_vb) {
        return Err(
            "Claim fees changed. Check both deposits and review the transactions again.".into(),
        );
    }
    let source = HttpObservationSource::new(
        connect.client,
        ChainId::Bitcoin,
        ChainId::BitcoinBlake2b,
        CollectionContext {
            expected_generation: expected,
            generation: generation.clone(),
        },
    )
    .map_err(|_| "The two-chain observation source is unavailable.".to_string())?;
    let funding = reservation
        .requalify_exact_pair(
            directory.clone(),
            &inventory,
            &identity,
            &context,
            &*daemon,
            &*fork_daemon,
            &source,
            CHECK_POLICY.observations,
            minimums,
            &pair.bitcoin,
            &pair.fork,
            rates,
        )
        .await
        .map_err(|error| format!("The exact Claim pair is no longer verified: {error:?}"))?;
    current()?;
    if !fork_binding.current() {
        return Err("The paired BTCB2 Vault changed during Claim verification.".into());
    }
    let bitcoin = pair.bitcoin.clone();
    let fork = pair.fork.clone();
    let bitcoin_path = funding.funding().bitcoin_path.clone();
    let fork_path = funding.funding().fork_path.clone();
    let fork_derivation = (
        funding.funding().fork.deriv_index,
        funding.funding().fork.is_change,
    );
    let result = tokio::task::spawn_blocking(move || {
        let controller = claim_workflow::Controller::create_or_reopen_ancestry_pair(
            &directory,
            bitcoin_cube,
            fork_cube,
            &bitcoin,
            &bitcoin_path,
            &fork,
            &fork_path,
            fork_derivation,
            rates.0,
            rates.1,
            context,
        )?;
        drop(controller);
        Ok::<(), claim_workflow::Error>(())
    })
    .await
    .map_err(|_| "Recording the exact Claim intent was interrupted.".to_string())?
    .map_err(|error| format!("The exact Claim intent could not be recorded: {error:?}"));
    current()?;
    if !fork_binding.current() {
        return Err("The paired BTCB2 Vault changed while recording the Claim intent.".into());
    }
    result.map(|()| funding)
}

/// Finish both ordinary PSBTs under a new exact-pair requalification, then
/// atomically save their finalized witnesses. A partial signer result never
/// enters the journal, and this function performs no broadcast or preflight.
#[allow(clippy::too_many_arguments)]
pub(super) async fn record_exact_signed_pair(
    pair: PreparedClaimPair,
    bitcoin_signed: Psbt,
    fork_signed: Psbt,
    directory: PathBuf,
    bitcoin_cube: String,
    fork_cube: String,
    wallet: Arc<Wallet>,
    daemon: Arc<dyn Daemon + Send + Sync>,
    fork_binding: ClaimForkBinding,
    connect: ConnectSession,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<super::SignedPairDetails, String> {
    let fork_return = UnifiedPsbt::from_psbt(fork_signed)
        .map_err(|_| "The BTCB2 signer PSBT could not be decoded.".to_string())?;
    let combined = crate::services::claim_pair_signing::combine_ancestry_claim_pair(
        &pair.bitcoin,
        &[bitcoin_signed],
        &pair.fork,
        &[fork_return],
    )
    .map_err(|error| format!("The exact Claim signatures could not be verified: {error:?}"))?;
    if !combined.complete {
        return Err("Both Claim transactions need all required signatures.".into());
    }
    let requalified = prepare_exact_pair_intent(
        pair.clone(),
        directory.clone(),
        bitcoin_cube.clone(),
        fork_cube.clone(),
        wallet.clone(),
        daemon,
        fork_binding.clone(),
        connect,
        expected,
        generation.clone(),
    )
    .await?;
    let context = requalified.context().clone();
    let tips = requalified.tips();
    let identity = claim_workflow::WalletIdentity {
        bitcoin_cube,
        fork_cube,
        descriptor_digest: sha256::Hash::hash(wallet.main_descriptor.to_string().as_bytes()),
    };
    let mut controller =
        claim_workflow::Controller::reopen_settling(&directory, &identity, context.clone())
            .await
            .map_err(|error| format!("The exact Claim intent could not be reopened: {error:?}"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "The system clock is unavailable.".to_string())?
        .as_secs() as i64;
    let (verified, _) = controller
        .finalize_ancestry_pair_for_review(
            &context,
            &requalified,
            &pair.bitcoin,
            &pair.fork,
            &combined.bitcoin,
            &combined.fork,
            CHECK_POLICY.observations,
            now,
            tips,
        )
        .map_err(|error| format!("The signed Claim pair is no longer verified: {error:?}"))?;
    if !crate::services::claim_observation::session_current(&generation, expected)
        || !fork_binding.current()
    {
        return Err("The Claim session changed before saving the signed pair.".into());
    }
    controller
        .record_signed_ancestry_claim_pair(&context, &verified)
        .map_err(|error| format!("The signed Claim pair could not be recorded: {error:?}"))?;
    Ok(super::SignedPairDetails {
        bitcoin_txid: verified.bitcoin().transaction().compute_txid(),
        fork_txid: verified.fork().transaction().compute_txid(),
        bitcoin_output: super::super::paired_summary::PairedOutputSummary::from_signed(
            verified.bitcoin().transaction(),
        ),
        fork_output: super::super::paired_summary::PairedOutputSummary::from_signed(
            verified.fork().transaction(),
        ),
    })
}

/// One-use result of checking this exact unsigned construction and session.
/// No serialization or clone: a delayed GUI result must be consumed afresh.
#[derive(Debug)]
pub struct CheckedInputs {
    coins: Vec<Coin>,
    psbt_digest: sha256::Hash,
    path_digest: sha256::Hash,
    descriptor: String,
    account: String,
    origin: String,
    expected: u64,
    generation: watch::Receiver<u64>,
    deadline: std::time::Instant,
}
impl CheckedInputs {
    pub(super) fn consume(
        self,
        built: &Construction,
        wallet: &Wallet,
        connect: Option<&ConnectSession>,
        generation: u64,
    ) -> Result<Vec<Coin>, String> {
        let Construction::Ancestry { path, .. } = built else {
            return Err("The ancestry construction changed.".into());
        };
        let valid_session = connect.is_some_and(|c| {
            c.account == self.account && c.client.base_url.trim_end_matches('/') == self.origin
        });
        if std::time::Instant::now() >= self.deadline
            || generation != self.expected
            || !crate::services::claim_observation::session_current(&self.generation, self.expected)
            || !valid_session
            || wallet.chain != ChainId::Bitcoin
            || wallet.main_descriptor.to_string() != self.descriptor
            || sha256::Hash::hash(&path.encode()) != self.path_digest
            || sha256::Hash::hash(&built.psbt().serialize()) != self.psbt_digest
        {
            return Err(
                "The ancestry signing check expired or changed. Check the Claim again.".into(),
            );
        }
        // Hashing bounded proof data may still take time or be descheduled.
        // Recheck live authority at the final consumption boundary as well.
        if std::time::Instant::now() >= self.deadline
            || !crate::services::claim_observation::session_current(&self.generation, self.expected)
        {
            return Err(
                "The ancestry signing check expired or changed. Check the Claim again.".into(),
            );
        }
        Ok(self.coins)
    }
}

pub(super) async fn check(
    built: &Construction,
    daemon: Arc<dyn Daemon + Send + Sync>,
    wallet: Arc<Wallet>,
    connect: ConnectSession,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<CheckedInputs, String> {
    let _checked = check_inputs(built, daemon, wallet, connect, expected, generation).await?;
    // Historical one-sided checks remain testable, but no production switch
    // can turn their result into signer authority. The paired path needs a
    // separate check of both chains and both complete wallet coin sets.
    Err(crate::services::claim_ancestry_gate::CLOSED_MESSAGE.to_string())
}

pub(super) async fn check_inputs(
    built: &Construction,
    daemon: Arc<dyn Daemon + Send + Sync>,
    wallet: Arc<Wallet>,
    connect: ConnectSession,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<CheckedInputs, String> {
    let current = || {
        if !crate::services::claim_observation::session_current(&generation, expected) {
            Err(SESSION_ENDED.to_string())
        } else {
            Ok(())
        }
    };
    current()?;
    let Construction::Ancestry { transfer, path } = built else {
        return Err("An ancestry construction is required for this check.".into());
    };
    let production = Production::new(
        connect.client.clone(),
        daemon.clone(),
        connect.account.clone(),
        expected,
        generation.clone(),
    )
    .map_err(describe_production)?;
    if transfer.descriptor() != &wallet.main_descriptor
        || daemon
            .config()
            .is_none_or(|c| c.main_descriptor != wallet.main_descriptor)
    {
        return Err("The Claim does not match this Vault and backend.".into());
    }
    drop(production);
    let account = connect.account.clone();
    let origin = connect.client.base_url.trim_end_matches('/').to_string();
    let source = HttpObservationSource::new(
        connect.client,
        ChainId::Bitcoin,
        ChainId::BitcoinBlake2b,
        CollectionContext {
            expected_generation: expected,
            generation: generation.clone(),
        },
    )
    .map_err(|e| format!("Couldn't prepare ancestry checks: {e:?}"))?;
    tokio::time::timeout(CHECK_POLICY.collection_budget, async {
        let input_check_started = std::time::Instant::now();
        let requested: Vec<_> = transfer
            .psbt()
            .unsigned_tx
            .input
            .iter()
            .map(|i| i.previous_output)
            .collect();
        let mut coins = daemon
            .list_coins(&[CoinStatus::Confirmed], &requested)
            .await
            .map_err(|e| e.to_string())?
            .coins;
        current()?;
        let tip = daemon
            .get_info()
            .await
            .map_err(|e| e.to_string())?
            .block_height;
        current()?;
        wallet.apply_coin_overrides(&mut coins);
        let actual: BTreeSet<_> = coins.iter().map(|c| c.outpoint).collect();
        if actual.len() != coins.len()
            || actual != requested.iter().copied().collect()
            || coins.iter().any(|c| {
                c.spend_info.is_some()
                    || c.is_immature
                    || c.block_height.is_none_or(|h| h < 0 || h > tip)
            })
        {
            return Err(
                "Claim inputs changed, are spent, or are not yet mature. Read the Vault again."
                    .into(),
            );
        }
        let secp = secp256k1::Secp256k1::verification_only();
        for coin in &coins {
            let index = requested
                .iter()
                .position(|p| *p == coin.outpoint)
                .ok_or("A Claim input is missing.")?;
            let prevout = transfer.psbt().inputs[index]
                .witness_utxo
                .as_ref()
                .ok_or("A Claim prevout is missing.")?;
            if coin.derivation_index.is_hardened() {
                return Err("The Claim input derivation is invalid.".into());
            }
            let derived = if coin.is_change {
                wallet.main_descriptor.change_descriptor()
            } else {
                wallet.main_descriptor.receive_descriptor()
            }
            .derive(coin.derivation_index, &secp);
            if coin.amount != prevout.value || derived.script_pubkey() != prevout.script_pubkey {
                return Err("The owned Claim input metadata changed. Read the Vault again.".into());
            }
        }
        let plan = claim::ClaimPlan {
            bitcoin_chain: wallet.chain,
            fork_chain: ChainId::BitcoinBlake2b,
            step1: transfer.psbt().unsigned_tx.clone(),
            claimed_prevouts: transfer.claimed_prevouts().to_vec(),
            poison: claim::Poison::InputAncestry,
            previous_confirmation: None,
            tracked_txid: None,
        };
        let collected = source
            .collect_ancestry(
                path,
                &plan,
                CHECK_POLICY.observations,
                CHECK_POLICY.collection_budget,
            )
            .await
            .map_err(|e| format!("Couldn't recheck the Bitcoin-only input: {e:?}"))?;
        current()?;
        let assessment = collected
            .assess_verified_observations(
                path,
                &plan,
                crate::services::claim_observation::http::AncestryContext {
                    provider: &source.provider_identity(),
                    generation: expected,
                    policy: CHECK_POLICY.observations,
                    now: source.now(),
                    tips: collected.assessment().observations.preflight,
                },
            )
            .map_err(|e| format!("The ancestry signing observations changed: {e:?}"))?;
        if assessment.assessment != Assessment::WaitingForConfirmation {
            return Err("The ancestry transaction is no longer ready for signing.".into());
        }
        // Anchor before sampling wall time or hashing: local computation and
        // scheduling delays consume freshness rather than extending it.
        let sampled = std::time::Instant::now();
        let deadline = evidence_deadline(
            sampled,
            source.now(),
            collected.observed_at(),
            CHECK_POLICY.observations.max_observation_age_seconds,
        )
        .ok_or("The ancestry signing check expired.")?
        .min(
            input_check_started
                .checked_add(CHECK_POLICY.collection_budget)
                .ok_or("The ancestry signing check expired.")?,
        );
        current()?;
        Ok(CheckedInputs {
            coins,
            psbt_digest: sha256::Hash::hash(&built.psbt().serialize()),
            path_digest: sha256::Hash::hash(&path.encode()),
            descriptor: wallet.main_descriptor.to_string(),
            account,
            origin,
            expected,
            generation: generation.clone(),
            deadline,
        })
    })
    .await
    .map_err(|_| "Checking the Claim inputs timed out. Read the Vault again.".to_string())?
}

// Match the coordinator's conservative allowance for UNIX-second quantization.
fn evidence_deadline(
    sampled: std::time::Instant,
    now: i64,
    observed_at: i64,
    max_age: i64,
) -> Option<std::time::Instant> {
    if observed_at < 0 || max_age <= 0 {
        return None;
    }
    let age = now.checked_sub(observed_at).filter(|age| *age >= 0)?;
    let remaining = max_age
        .checked_sub(age)?
        .checked_sub(1)
        .filter(|v| *v > 0)?;
    sampled.checked_add(Duration::from_secs(remaining as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_excludes_quantization_and_delayed_construction() {
        let sampled = std::time::Instant::now();
        assert_eq!(evidence_deadline(sampled, 100, 99, 2), None);
        assert_eq!(
            evidence_deadline(sampled, 100, 100, 2),
            Some(sampled + Duration::from_secs(1))
        );
        assert_eq!(evidence_deadline(sampled, 100, 101, 20), None);
        assert_eq!(evidence_deadline(sampled, 100, -1, 20), None);
        assert_eq!(evidence_deadline(sampled, i64::MAX, 0, 20), None);
        // Model two seconds of work after the wall-time sample without sleeping.
        let delayed = sampled.checked_sub(Duration::from_secs(2)).unwrap();
        assert!(evidence_deadline(delayed, 100, 100, 2).unwrap() < sampled);
    }
}
