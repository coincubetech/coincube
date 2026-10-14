//! Split panel view (#568 B1b, step 2 in B3b-2b-2). Pure rendering of
//! [`SplitPanel`]; every action is a [`SplitMessage`]. There is no "start"
//! action here: a fresh split starts only from the sweep review's "Start
//! split" (#568 B5c-2), and step 2 is reachable only from a journal whose
//! step 1 has six confirmations.

use coincube_core::{
    chain::ChainId,
    claim::MIN_CONFIRMATIONS,
    miniscript::bitcoin::{Address, Network},
};
use coincube_ui::{
    component::{button, card, text::*},
    theme,
    widget::{ColumnExt, Element},
};
use iced::{
    widget::{scrollable, Column, Container, Row, Space},
    Alignment, Length,
};

use crate::{
    app::{
        state::vault::split::{
            device::{self, DeviceMessage, DeviceStep},
            step1::{self, describe_route},
            step2,
            unified::{self, SeedText, UnifiedMessage, UnifiedStage},
            SplitMessage, SplitPanel, Stage, Step2Stage, Work,
        },
        view::Message,
    },
    services::{claim_coordinator::Outcome, claim_workflow::Phase, split_psbt_file::Encoding},
};

fn action(label: &'static str, message: SplitMessage) -> Element<'static, Message> {
    button::secondary(None, label)
        .on_press(Message::Split(message))
        .into()
}

fn primary(label: &'static str, message: SplitMessage) -> Element<'static, Message> {
    button::primary(None, label)
        .on_press(Message::Split(message))
        .into()
}

fn funding_address_row(address: String, notice: &'static str) -> Element<'static, Message> {
    Row::new()
        .spacing(10)
        .align_y(Alignment::Center)
        .push(
            Container::new(
                scrollable(p2_regular(address.clone()).style(theme::text::secondary))
                    .width(Length::Fill)
                    .direction(scrollable::Direction::Horizontal(
                        scrollable::Scrollbar::new().width(2).scroller_width(2),
                    )),
            )
            .width(Length::Fill)
            .max_width(750.0),
        )
        .push(
            button::secondary_compact(None, "Copy address")
                .on_press(Message::CopyWithToast(address, notice.to_string())),
        )
        .into()
}

fn signed_pair_destination(psbt: &coincube_core::miniscript::bitcoin::psbt::Psbt) -> String {
    psbt.unsigned_tx
        .output
        .first()
        .and_then(|output| Address::from_script(&output.script_pubkey, Network::Bitcoin).ok())
        .map(|address| address.to_string())
        .unwrap_or_else(|| "Unavailable; inspect the PSBT output on your signer".into())
}

fn signed_pair_amount(psbt: &coincube_core::miniscript::bitcoin::psbt::Psbt) -> String {
    psbt.unsigned_tx
        .output
        .first()
        .map(|output| format!("{} sats", output.value.to_sat()))
        .unwrap_or_else(|| "Unavailable; inspect the PSBT output on your signer".into())
}

fn working(work: Work) -> &'static str {
    match work {
        Work::ReservingFunding => "Saving fresh deposit addresses and checking both chains for prior use…",
        Work::CheckingFunding => "Scanning both chains and checking deposit ancestry…",
        Work::PreparingPairedSign => "Rechecking both chains and saving the exact signing intent…",
        Work::CheckingPairedSignatures => "Checking both chains and the returned signatures…",
        Work::RecordingPairedSignatures => "Recording both signed transactions on this device…",
        Work::ReviewingPairedBitcoin => "Rechecking both chains for Bitcoin broadcast review…",
        Work::SubmittingPairedBitcoin => "Rechecking both chains before Bitcoin broadcast…",
        Work::ReviewingPairedFork => "Rechecking the Bitcoin confirmation and BTCB2 inputs…",
        Work::SubmittingPairedFork => "Rechecking both chains before BTCB2 broadcast…",
        Work::Checking => "Checking the fork, fees and the fresh address…",
        Work::Building => "Building step 1…",
        Work::Exporting => "Saving the PSBT file…",
        Work::Importing => "Checking the signed files…",
        Work::Recording => "Recording the split on this device…",
        Work::Restoring => "Rebuilding the recorded split from the chain…",
        Work::Reviewing => "Preparing a fresh review…",
        Work::Submitting => "Submitting through Connect…",
        Work::Reconciling => "Checking the chains…",
        Work::Recovering => "Checking what happened to step 1…",
        Work::Acknowledging => "Checking the new block again…",
        Work::Resending => "Sending step 1 again through Connect…",
        Work::CheckingAbandon => "Checking Bitcoin before abandoning…",
        Work::Abandoning => "Abandoning…",
        // #662 F2: the two-step and the single-step close share these.
        Work::CheckingClose => "Checking both chains before closing the split…",
        Work::Closing => "Closing the split…",
        Work::CheckingConflictClose => "Checking Bitcoin before closing the split…",
        Work::ClosingConflict => "Closing the split…",
        Work::Restarting => "Reading the split recorded on this device…",
        Work::Entering => "Opening step 2…",
        Work::Leaving => "Returning to step 1…",
        Work::Step2Checking => "Checking step 1's confirmations on both chains…",
        Work::Reserving => step2::RESERVING,
        Work::Step2Building => "Building step 2…",
        Work::Step2Exporting => "Saving the step-2 PSBT file…",
        Work::Step2Importing => "Checking the signed step-2 files…",
        Work::Finishing => "Verifying the signed step 2…",
        Work::Step2Reviewing => "Preparing a fresh step-2 review…",
        Work::Step2Submitting => "Submitting step 2…",
        Work::Step2Reconciling => "Checking Bitcoin Blake2b for step 2…",
        Work::Step2ResendReviewing => {
            "Checking both chains and the claimed coins before a resend of step 2…"
        }
        Work::Step2Resending => "Sending step 2 again…",
        Work::Step2ReconfirmationReviewing => "Checking step 1's new Bitcoin block on both chains…",
        Work::Step2Reconfirming => {
            "Checking step 1's new Bitcoin block again before recording it…"
        }
        Work::Step2Completing => {
            "Checking both chains, then recording the completion on this Cube…"
        }
        Work::Step2CompletionChecking => "Checking that the completion still stands…",
        Work::ListingDevices => "Looking for connected hardware wallets…",
        Work::SigningOnDevice => {
            "Confirm on your hardware wallet. Check the amounts and the address on its screen before you approve."
        }
        Work::SweepOpening => "Checking the coins on both chains…",
        Work::AddingSeed => "Checking the recovery phrase against this wallet's keys…",
        Work::SweepSigning => {
            "Reserving a fresh Vault address, building the sweep and signing it…"
        }
        Work::SweepReviewing => "Checking Bitcoin Blake2b and the fee before the review…",
        Work::SweepSubmitting => "Submitting the sweep…",
        Work::SweepReconciling => "Checking Bitcoin Blake2b for the sweep…",
    }
}

fn unified_action(label: &'static str, message: UnifiedMessage) -> Element<'static, Message> {
    action(label, SplitMessage::Unified(message))
}

fn unified_primary(label: &'static str, message: UnifiedMessage) -> Element<'static, Message> {
    primary(label, SplitMessage::Unified(message))
}

/// A seed input: `.secure(true)`, and what is typed goes straight into a
/// zeroizing [`SeedText`].
fn seed_input<'a>(
    placeholder: &'static str,
    value: &'a SeedText,
    message: fn(SeedText) -> UnifiedMessage,
) -> Element<'a, Message> {
    iced::widget::text_input(placeholder, value.expose_for_secure_input())
        .secure(true)
        .on_input(move |typed| {
            Message::Split(SplitMessage::Unified(message(SeedText::from(typed))))
        })
        .padding(8)
        .into()
}

/// The single-step stages (#568 B4b-3c).
fn unified_body<'a>(
    panel: &'a SplitPanel,
    stage: UnifiedStage,
    mut body: coincube_ui::widget::Column<'a, Message>,
    mut actions: coincube_ui::widget::Row<'a, Message>,
) -> (
    coincube_ui::widget::Column<'a, Message>,
    coincube_ui::widget::Row<'a, Message>,
) {
    let state = panel.unified();
    if !panel.can_sign_or_submit() {
        body = body.push(p1_regular(
            "This recorded split remains available for inspection.",
        ));
        if matches!(stage, UnifiedStage::Submitted | UnifiedStage::Reconcile) {
            actions = actions.push(unified_action("Refresh", UnifiedMessage::Reconcile));
        }
        return (body, actions);
    }
    match stage {
        UnifiedStage::EnterSeeds => {
            body = body
                .push(p1_regular(format!(
                    "{} of {} recovery phrase(s) entered. Each one is checked against this wallet's keys and kept in memory only until the sweep is signed.",
                    state.held(),
                    state.threshold()
                )))
                .push(seed_input("Recovery phrase", state.typed_words(), UnifiedMessage::Words))
                .push(seed_input(
                    "Passphrase (leave empty if none)",
                    state.typed_passphrase(),
                    UnifiedMessage::Passphrase,
                ));
            actions = actions
                .push(unified_action("Add phrase", UnifiedMessage::AddSeed))
                .push(unified_action("Clear phrases", UnifiedMessage::ClearSeeds));
            if panel.can_build_unified() {
                actions = actions.push(unified_primary(
                    "Build and sign",
                    UnifiedMessage::BuildAndSign,
                ));
            }
            actions = actions.push(unified_action("Cancel", UnifiedMessage::Cancel));
        }
        UnifiedStage::Signed => {
            body = body.push(p1_regular(
                "The sweep is signed and the recovery phrases were cleared. Nothing has been recorded or sent. Review it to submit.",
            ));
            actions = actions
                .push(unified_primary("Review sweep", UnifiedMessage::Review))
                .push(unified_action("Cancel", UnifiedMessage::Cancel));
        }
        UnifiedStage::Review => {
            if let Some(review) = state.review() {
                body = body
                    .push(p1_bold(review.protected.clone()).style(theme::text::success))
                    .push(caption(review.limitation).style(theme::text::secondary))
                    .push(p1_bold(format!("Sweep txid {}", review.txid)))
                    .push(p1_regular(format!(
                        "Fee {} sats · {} vB · route {}",
                        review.fee_sats, review.vsize, review.route_label
                    )));
                if let Some(note) = review.privacy_note {
                    body = body.push(p1_regular(note).style(theme::text::warning));
                }
            }
            actions = actions
                .push(unified_primary("Submit sweep", UnifiedMessage::Confirm))
                .push(unified_action("Review again", UnifiedMessage::Review))
                .push(unified_action("Cancel", UnifiedMessage::Cancel));
        }
        UnifiedStage::Submitted | UnifiedStage::Reconcile => {
            body = body.push(p1_regular(match state.outcome() {
                Some(Outcome::UpstreamAccepted { txid, .. }) => {
                    format!("The sweep {txid} was accepted for relay. Waiting for confirmation.")
                }
                Some(Outcome::Uncertain { txid, .. }) | Some(Outcome::Recorded { txid }) => format!(
                    "A submission of the sweep ({txid}) is recorded. It is never sent again; check its status."
                ),
                None => "A submission of the sweep is recorded. Check its status.".to_string(),
            }));
            if let Some(seen) = state.seen() {
                body = body.push(caption(format!("Bitcoin Blake2b: {seen:?}")));
            }
            actions = actions.push(unified_action("Refresh", UnifiedMessage::Reconcile));
        }
    }
    (body, actions)
}

/// "Sign with connected device" (#568 B4b-3b): what the device will show,
/// then each listed device. Only a supported device holding one of this
/// wallet's keys can be asked to sign; a multisig adds what its
/// registration means on that device.
fn device_section<'a>(
    panel: &'a SplitPanel,
    step: DeviceStep,
    mut body: coincube_ui::widget::Column<'a, Message>,
    mut actions: coincube_ui::widget::Row<'a, Message>,
) -> (
    coincube_ui::widget::Column<'a, Message>,
    coincube_ui::widget::Row<'a, Message>,
) {
    let signer = panel.device();
    if signer.is_open() && signer.step() == Some(step) {
        body = body.push(p1_bold("Sign with a connected hardware wallet"));
        for line in device::step_copy(step) {
            body = body.push(p1_regular(*line).style(theme::text::warning));
        }
        let rows = signer.rows();
        if rows.is_empty() {
            body = body.push(
                p1_regular("Connect and unlock a hardware wallet, and open its Bitcoin app.")
                    .style(theme::text::secondary),
            );
        }
        for row in rows {
            let mut line = Row::new()
                .spacing(10)
                .align_y(Alignment::Center)
                .push(p1_regular(row.label).width(Length::Fill));
            if let Some(id) = row.sign {
                line = line.push(button::secondary(None, "Sign").on_press(Message::Split(
                    SplitMessage::Device(DeviceMessage::Sign(id)),
                )));
            }
            body = body.push(line);
            if let Some(notice) = row.notice {
                body = body.push(caption(notice).style(theme::text::secondary));
            }
        }
        actions = actions.push(action(
            "Close device list",
            SplitMessage::Device(DeviceMessage::Cancel),
        ));
    } else if panel.can_open_paired_device(step) {
        let chain = if step == DeviceStep::PairedBitcoin {
            ChainId::Bitcoin
        } else {
            ChainId::BitcoinBlake2b
        };
        actions = actions.push(action(
            if chain == ChainId::Bitcoin {
                "Sign Bitcoin on device"
            } else {
                "Sign BTCB2 on device"
            },
            SplitMessage::Device(DeviceMessage::OpenPaired(chain)),
        ));
    } else if panel.can_open_device() {
        actions = actions.push(action(
            "Sign with connected device",
            SplitMessage::Device(DeviceMessage::Open),
        ));
    }
    (body, actions)
}

/// The warning lines under every stage: the step-1 evidence's warning after
/// the step-2 submission, then the last operation's notice. Each has its own
/// line, so a saved file, a failed export or check, or an ended session never
/// hides the warning (#637 review 5971166062 F1).
pub(crate) fn warning_lines(panel: &SplitPanel) -> Vec<String> {
    panel
        .step2_warning()
        .into_iter()
        .chain(panel.notice().map(str::to_string))
        .collect()
}

/// The step-2 stages.
fn step2_body<'a>(
    panel: &'a SplitPanel,
    stage: Step2Stage,
    mut body: coincube_ui::widget::Column<'a, Message>,
    mut actions: coincube_ui::widget::Row<'a, Message>,
) -> (
    coincube_ui::widget::Column<'a, Message>,
    coincube_ui::widget::Row<'a, Message>,
) {
    if !panel.can_sign_or_submit()
        && matches!(
            stage,
            Step2Stage::Ready | Step2Stage::Sign | Step2Stage::Signed | Step2Stage::Review
        )
    {
        body = body.push(p1_regular(
            "This recorded split can be inspected, but no new transaction can be signed or sent from it.",
        ));
        return (body, actions);
    }
    if let Some(label) = panel.replay_label() {
        body = body.push(p1_bold(label).style(theme::text::success));
    }
    match stage {
        Step2Stage::Ready => {
            body = body.push(p1_regular(
                "Step 2 moves the same coins on Bitcoin Blake2b to a fresh address of this Vault. Check step 1's confirmations, reserve the address, then build.",
            ));
            if let Some(index) = panel.target_index() {
                body = body.push(caption(format!(
                    "Fresh Vault address reserved (receive index {index}); Connect shows no use on either chain."
                )));
            }
            actions = actions
                .push(action("Check confirmations", SplitMessage::Step2Check))
                .push(action("Reserve address", SplitMessage::Step2Reserve));
            if panel.target_index().is_some() && panel.replay_label().is_some() {
                actions = actions.push(primary("Build step 2", SplitMessage::Step2Build));
            }
            actions = actions.push(action("Back to step 1", SplitMessage::LeaveStep2));
        }
        Step2Stage::Sign => {
            if let Some(psbt) = panel.step2_psbt() {
                body = body.push(caption(format!(
                    "Unsigned step-2 txid {}",
                    psbt.unsigned_tx.compute_txid()
                )));
            }
            body = body.push(p1_regular(match panel.step2_exported() {
                Some(path) => format!(
                    "Saved to {}. Sign it in the wallet that holds the keys (SIGHASH_ALL, without finalizing), then import the signed file(s).",
                    path.display()
                ),
                None => "Save the unsigned step-2 PSBT, sign it in the wallet that holds the keys, then import the signed file(s).".to_string(),
            }));
            if panel.step2_files() > 0 {
                body = body.push(caption(format!(
                    "{} signed file(s) loaded",
                    panel.step2_files()
                )));
            }
            actions = actions
                .push(action(
                    "Save PSBT",
                    SplitMessage::Step2Export(Encoding::Binary),
                ))
                .push(action(
                    "Save as text",
                    SplitMessage::Step2Export(Encoding::Base64),
                ))
                .push(primary("Import signed", SplitMessage::Step2Import))
                .push(action("Back to step 1", SplitMessage::LeaveStep2));
            if panel.can_retry_step2_handoff() {
                actions = actions.push(primary("Retry handoff", SplitMessage::Step2RetryHandoff));
            }
            (body, actions) = device_section(panel, DeviceStep::Step2, body, actions);
        }
        Step2Stage::Signed => {
            body = body.push(p1_regular(
                "Step 2 is signed. Nothing has been sent. Review it to submit.",
            ));
            actions = actions.push(primary("Review step 2", SplitMessage::Step2Review));
        }
        Step2Stage::Review => {
            if let Some(review) = panel.step2_review() {
                body = body
                    .push(p1_bold(format!("Step-2 txid {}", review.txid)))
                    .push(p1_regular(format!(
                        "Fee {} sats · {} vB · route {}",
                        review.fee_sats, review.vsize, review.route_label
                    )));
                if let Some(note) = review.privacy_note {
                    body = body.push(p1_regular(note).style(theme::text::warning));
                }
            }
            actions = actions
                .push(primary("Submit step 2", SplitMessage::Step2Confirm))
                .push(action("Review again", SplitMessage::Step2Review));
        }
        Step2Stage::Submitted | Step2Stage::Reconcile => {
            body = body.push(p1_regular(match panel.step2_outcome() {
                Some(Outcome::UpstreamAccepted { txid, .. }) => {
                    format!("Step 2 {txid} was accepted for relay. Waiting for confirmation.")
                }
                Some(Outcome::Uncertain { txid, .. }) | Some(Outcome::Recorded { txid }) => format!(
                    "A submission of step 2 ({txid}) is recorded. It is never sent again automatically; check its status."
                ),
                None => "A submission of step 2 is recorded. Check its status.".to_string(),
            }));
            if let Some(seen) = panel.step2_seen() {
                body = body.push(caption(format!("Bitcoin Blake2b: {seen:?}")));
            }
            // P3-3: a resend of exactly the recorded step 2, only from a
            // live review.
            if let Some(resend) = panel
                .step2_resend_review()
                .filter(|_| panel.can_sign_or_submit())
            {
                body = body
                    .push(p1_bold(format!("Send step 2 ({}) again", resend.txid)))
                    .push(p1_regular(format!(
                        "Exactly the recorded signed step 2 · resend {} of at most {} · route {}",
                        resend.attempt, resend.max_attempts, resend.route_label
                    )))
                    .push(caption(format!(
                        "Fresh checks found it absent from Bitcoin Blake2b and every claimed coin unspent there. This review expires at {}; after that, review it again.",
                        resend.expires_at.format("%H:%M:%S")
                    )));
                if let Some(note) = resend.privacy_note {
                    body = body.push(p1_regular(note).style(theme::text::warning));
                }
                actions = actions
                    .push(primary("Send again", SplitMessage::Step2ConfirmResend))
                    .push(action("Review again", SplitMessage::Step2ReviewResend));
            } else if panel.can_sign_or_submit() && panel.can_review_resend() {
                actions = actions.push(action("Review resend", SplitMessage::Step2ReviewResend));
            }
            // #568 S4b, O1: step 1 re-mined in another Bitcoin block; a live
            // review shows both blocks and the new one's depth.
            if let Some(review) = panel.reconfirmation_review() {
                body = body
                    .push(p1_bold(format!(
                        "Step 1 is now in Bitcoin block {} ({} of {MIN_CONFIRMATIONS} confirmations)",
                        review.confirmed.height,
                        review.confirmations.min(MIN_CONFIRMATIONS)
                    )))
                    .push(caption(format!(
                        "New block {} · recorded block {} at height {}",
                        review.confirmed.hash, review.previous.hash, review.previous.height
                    )))
                    .push(p1_regular(format!(
                        "Acknowledging records the new block for step 1. Nothing is sent. Step 2's Bitcoin replay protection is established once step 1 has {MIN_CONFIRMATIONS} confirmations in it. This review expires at {}; after that, review it again.",
                        review.expires_at.format("%H:%M:%S")
                    )));
                actions = actions
                    .push(primary(
                        "Acknowledge new block",
                        SplitMessage::Step2ConfirmReconfirmation,
                    ))
                    .push(action(
                        "Review again",
                        SplitMessage::Step2ReviewReconfirmation,
                    ));
            } else if panel.can_review_reconfirmation() {
                actions = actions.push(action(
                    "Review step 1's new block",
                    SplitMessage::Step2ReviewReconfirmation,
                ));
            }
            if panel.can_check_close() {
                body = body.push(p1_regular(
                    match panel.dead_end().and_then(|dead_end| dead_end.conflict) {
                        // #568 S4b, O4: a terminal step-1 conflict.
                        Some(conflict) => step2::conflict_close_copy(&conflict),
                        None => "This version can't send this step 2 again: its last attempt was accepted or may have left, or no resend is left. If Bitcoin Blake2b never shows it, you can abandon this split after a check.".to_string(),
                    },
                ));
            }
            // #568 B5b: only after this session's reconcile saw step 2
            // confirmed with step 1 eligible.
            if panel.can_complete() {
                body = body.push(p1_regular(
                    "Step 2 is confirmed on Bitcoin Blake2b and step 1 is six deep on Bitcoin. Completing checks both chains once more, records the split on this Cube only by a digest (a hash) of the source wallet's descriptor, then deletes the source wallet's descriptors from this device.",
                ));
                actions = actions.push(primary("Complete split", SplitMessage::Step2Complete));
            }
            actions = actions.push(action("Refresh", SplitMessage::Step2Reconcile));
        }
        Step2Stage::Completed => {
            body = body.push(p1_bold(step2::SPLIT_COMPLETED).style(theme::text::success));
            if let Some(completion) = panel.completion() {
                body = body.push(caption(completion.history_row()));
            }
            if let Some(seen) = panel.step2_seen() {
                body = body.push(caption(format!("Bitcoin Blake2b: {seen:?}")));
            }
            actions = actions.push(action("Refresh", SplitMessage::Step2Reconcile));
        }
    }
    (body, actions)
}

/// #662 F2: the Closed stage, for either route (a two-step split closed in
/// its step-2 dead end, or a single-step one, sent or not).
pub const CLOSED_COPY: &str = "This split was closed on this device. Its record, with any signed transaction, is kept, and a new split of this wallet is refused until that record is reset.";

pub fn split_panel(panel: &SplitPanel) -> Element<'_, Message> {
    let single = matches!(panel.stage(), Stage::Unified(_) | Stage::ChooseRoute)
        || panel.unified().record().is_some();
    let mut body = Column::new().spacing(10).max_width(640);
    body = if matches!(
        panel.stage(),
        Stage::PairedRecovery | Stage::PairedBitcoinReview | Stage::PairedForkReview
    ) {
        body.push(h3("Recorded two-chain split"))
    } else if matches!(panel.stage(), Stage::PairedSign) {
        body.push(h3("Split: sign both transactions"))
    } else if single {
        body.push(h3("Split")).push(
            p1_regular(
                "Split moves the foreign wallet's pre-fork coins on Bitcoin Blake2b into this Vault so they can't be replayed on Bitcoin.",
            )
            .style(theme::text::secondary),
        )
    } else {
        body.push(h3("Split: step 1 on Bitcoin")).push(
            p1_regular(
                "A recorded step 1 can be checked here. New splits require confirmed funding coins specific to each chain before signing.",
            )
            .style(theme::text::secondary),
        )
    };
    let mut actions = Row::new().spacing(10).align_y(Alignment::Center);

    match panel.stage() {
        Stage::PairedRecovery => {
            body = body.push(p1_regular("This device has a two-chain split record. Each possible broadcast is checked against its own chain. Checking status does not send either transaction."));
            if let Some(recovery) = panel.paired_recovery() {
                body = body
                    .push(p1_bold("Bitcoin transaction"))
                    .push_maybe(recovery.bitcoin_txid.map(|id| caption(id.to_string())))
                    .push_maybe(recovery.bitcoin_output.as_ref().map(|output| {
                        caption(format!(
                            "Recorded Bitcoin output: {} sats",
                            output.amount_sats
                        ))
                    }))
                    .push_maybe(
                        recovery
                            .bitcoin_output
                            .as_ref()
                            .map(|output| caption(output.destination.clone())),
                    )
                    .push(p1_regular(recovery.bitcoin_status.clone()))
                    .push(p1_bold("Bitcoin Blake2b transaction"))
                    .push_maybe(recovery.fork_txid.map(|id| caption(id.to_string())))
                    .push_maybe(recovery.fork_output.as_ref().map(|output| {
                        caption(format!(
                            "Recorded BTCB2 output: {} sats",
                            output.amount_sats
                        ))
                    }))
                    .push_maybe(
                        recovery
                            .fork_output
                            .as_ref()
                            .map(|output| caption(output.destination.clone())),
                    )
                    .push(p1_regular(recovery.fork_status.clone()));
            }
            actions = actions.push(action("Check again", SplitMessage::CheckPaired));
            if crate::services::claim_ancestry_gate::authorization().is_some()
                && panel
                    .paired_recovery()
                    .is_some_and(|recovery| recovery.bitcoin_unattempted)
            {
                actions = actions.push(action(
                    "Review Bitcoin broadcast",
                    SplitMessage::ReviewPairedBitcoin,
                ));
            }
            if crate::services::claim_ancestry_gate::authorization().is_some()
                && panel
                    .paired_recovery()
                    .is_some_and(|recovery| recovery.fork_ready_for_review)
            {
                actions = actions.push(action(
                    "Review BTCB2 broadcast",
                    SplitMessage::ReviewPairedFork,
                ));
            }
        }
        Stage::PairedBitcoinReview => {
            if let Some(review) = panel.paired_bitcoin_review() {
                body = body
                    .push(p1_regular("Review these exact signed transactions. Only the Bitcoin transaction will be broadcast now; BTCB2 requires a separate review after Bitcoin confirmation."))
                    .push(p1_bold("Bitcoin — broadcast now"))
                    .push(caption(format!("Txid: {}", review.bitcoin.compute_txid())))
                    .push(caption(format!("Fee: {} sats", review.bitcoin_fee_sats)))
                    .push_maybe(crate::app::state::vault::paired_summary::PairedOutputSummary::from_signed(&review.bitcoin).map(|output| {
                        caption(format!("{} sats to {}", output.amount_sats, output.destination))
                    }))
                    .push(p1_bold("Bitcoin Blake2b — recorded, not sent now"))
                    .push(caption(format!("Txid: {}", review.fork.compute_txid())))
                    .push(caption(format!("Fee: {} sats", review.fork_fee_sats)))
                    .push_maybe(crate::app::state::vault::paired_summary::PairedOutputSummary::from_signed(&review.fork).map(|output| {
                        caption(format!("{} sats to {}", output.amount_sats, output.destination))
                    }));
                actions = actions.push(action("Back", SplitMessage::CheckPaired));
                actions = actions.push(action(
                    "Broadcast Transaction",
                    SplitMessage::ConfirmPairedBitcoin,
                ));
            }
        }
        Stage::PairedForkReview => {
            if let Some(review) = panel.paired_fork_review() {
                body = body
                    .push(p1_regular("Bitcoin has six current confirmations. Review the exact signed BTCB2 destination and fee before this separate broadcast."))
                    .push(p1_bold("Bitcoin — already recorded"))
                    .push(caption(format!("Txid: {}", review.bitcoin.compute_txid())))
                    .push_maybe(crate::app::state::vault::paired_summary::PairedOutputSummary::from_signed(&review.bitcoin).map(|output| {
                        caption(format!("{} sats to {}", output.amount_sats, output.destination))
                    }))
                    .push(p1_bold("Bitcoin Blake2b — broadcast now"))
                    .push(caption(format!("Txid: {}", review.fork.compute_txid())))
                    .push(caption(format!("Fee: {} sats", review.fork_fee_sats)))
                    .push_maybe(crate::app::state::vault::paired_summary::PairedOutputSummary::from_signed(&review.fork).map(|output| {
                        caption(format!("{} sats to {}", output.amount_sats, output.destination))
                    }));
                actions = actions.push(action("Back", SplitMessage::CheckPaired));
                actions = actions.push(action(
                    "Broadcast Transaction",
                    SplitMessage::ConfirmPairedFork,
                ));
            }
        }
        Stage::FundingDeposits => {
            if let Some(addresses) = panel.funding_addresses() {
                body = body
                    .push(p1_bold("Fund the source wallet on each chain"))
                    .push(p1_regular(format!(
                        "Send at least {} sats of Bitcoin to this Bitcoin address of the wallet being split (fee estimate when prepared: {} sat/vB):",
                        addresses.fee_quote.bitcoin_minimum_sats,
                        addresses.fee_quote.bitcoin_rate_sat_vb,
                    )))
                    .push(funding_address_row(
                        addresses.bitcoin_deposit.clone(),
                        "Copied Bitcoin deposit address to clipboard",
                    ))
                    .push(p1_regular(format!(
                        "Send at least {} sats of Bitcoin Blake2b to this BTCB2 address of the same wallet (fee estimate when prepared: {} sat/vB):",
                        addresses.fee_quote.btcb2_minimum_sats,
                        addresses.fee_quote.btcb2_rate_sat_vb,
                    )))
                    .push(funding_address_row(
                        addresses.btcb2_deposit.clone(),
                        "Copied BTCB2 deposit address to clipboard",
                    ))
                    .push(p1_regular("Send only the named coin to each address and wait for confirmation. These minimums reflect current fees; if fees rise before signing, you may need to add funds. The deposits are fee inputs, not the split itself. Both coins must pass independent ancestry, ownership, confirmation and unspent checks before signing."))
                    .push(caption("The Bitcoin return address is reserved separately and is not a deposit destination."));
                if let Some(progress) = panel.funding_progress() {
                    let candidate = |seen| {
                        if seen {
                            "post-fork coin found; checking paired proof"
                        } else {
                            "no confirmed post-fork coin found at the reserved address"
                        }
                    };
                    body = body
                        .push(caption(format!(
                            "Bitcoin deposit: {}",
                            candidate(progress.bitcoin_candidate)
                        )))
                        .push(caption(format!(
                            "Bitcoin Blake2b deposit: {}",
                            candidate(progress.btcb2_candidate)
                        )));
                    if let Some(review) = &progress.review {
                        let address = |output: &coincube_core::miniscript::bitcoin::TxOut| {
                            Address::from_script(&output.script_pubkey, Network::Bitcoin)
                                .map(|address| address.to_string())
                                .unwrap_or_else(|_| {
                                    format!(
                                        "script {}",
                                        hex::encode(output.script_pubkey.as_bytes())
                                    )
                                })
                        };
                        body = body
                            .push(p1_bold("Paired transaction preview — read only"))
                            .push(p1_regular(format!(
                                "{} shared pre-fork coins · {} sats on each chain",
                                review.bitcoin.shared.len(),
                                review.bitcoin.shared_total.to_sat()
                            )));
                        if let Some(threshold) = panel.funding_signing_threshold() {
                            body = body.push(caption(format!(
                                "Signing threshold: {threshold} signature{} per input",
                                if threshold == 1 { "" } else { "s" }
                            )));
                        }
                        for row in &review.bitcoin.shared {
                            body = body.push(caption(format!(
                                "Before fork · present on both chains: {} · {} sats",
                                row.outpoint,
                                row.amount.to_sat()
                            )));
                        }
                        body = body
                            .push(p1_bold("Bitcoin transaction"))
                            .push(caption(format!("Bitcoin-only · verified ({} confirmations): {} · {} sats", review.bitcoin_confirmations, review.bitcoin.funding.outpoint, review.bitcoin.funding.amount.to_sat())))
                            .push(caption(format!("Return to this source wallet: {} · {} sats", address(&review.bitcoin.destination), review.bitcoin.destination.value.to_sat())))
                            .push(caption(format!("Fee: {} sats", review.bitcoin.fee.to_sat())))
                            .push(p1_bold("Bitcoin Blake2b transaction"))
                            .push(caption(format!("BTCB2-only · verified ({} confirmations): {} · {} sats", review.fork_confirmations, review.fork.funding.outpoint, review.fork.funding.amount.to_sat())))
                            .push(caption(format!("Send to your paired BTCB2 Cube: {} · {} sats", address(&review.fork.destination), review.fork.destination.value.to_sat())))
                            .push(caption(format!("Fee: {} sats", review.fork.fee.to_sat())))
                            .push(p1_regular("Each transaction spends every shared coin together with its own verified chain-specific funding coin. This preview is not a signing or broadcast request; both proofs, wallet state and fees must be checked again before either transaction can be signed."));
                    } else if progress.bitcoin_candidate && progress.btcb2_candidate {
                        body = body.push(p1_regular("Both deposits are visible, but their paired chain-specific ancestry has not been established. No transaction can be signed."));
                    }
                }
                if crate::services::claim_ancestry_gate::authorization().is_some()
                    && panel
                        .funding_progress()
                        .is_some_and(|progress| progress.both_verified && progress.review.is_some())
                {
                    actions = actions.push(primary(
                        "Review and sign both transactions",
                        SplitMessage::BeginPairedSigning,
                    ));
                }
                actions = actions.push(action("Check deposits", SplitMessage::CheckDeposits));
            }
        }
        Stage::PairedSign => {
            body = body.push(p1_regular("Both ordinary transactions are bound to the same source coins and separately verified chain-specific deposits. Recheck the full destination and fee on each signing device. Nothing has been sent."));
            if let Some((bitcoin, fork, bitcoin_files, fork_files)) = panel.paired_signing_view() {
                body = body.push(p1_regular(format!(
                    "Both transactions spend the same {} shared pre-fork coin(s).",
                    bitcoin.claimed_prevouts().len()
                )));
                if let Some(threshold) = panel.funding_signing_threshold() {
                    body = body.push(caption(format!(
                        "Signing threshold: {threshold} signature{} for each input on both chains",
                        if threshold == 1 { "" } else { "s" }
                    )));
                }
                body = body
                    .push(p1_bold("Bitcoin transaction"))
                    .push(caption(format!(
                        "Unsigned txid {} · Bitcoin-only input {} · fee {} sats · {} signer return(s)",
                        bitcoin.psbt().unsigned_tx.compute_txid(),
                        bitcoin.funding_prevout(),
                        bitcoin.fee().to_sat(),
                        bitcoin_files,
                    )))
                    .push(caption(format!(
                        "Return to source wallet: {} · {}",
                        signed_pair_destination(bitcoin.psbt()),
                        signed_pair_amount(bitcoin.psbt())
                    )))
                    .push(p1_bold("Bitcoin Blake2b transaction"))
                    .push(caption(format!(
                        "Unsigned txid {} · BTCB2-only input {} · fee {} sats · {} signer return(s)",
                        fork.psbt().unsigned_tx.compute_txid(),
                        fork.funding_prevout(),
                        fork.fee().to_sat(),
                        fork_files,
                    )))
                    .push(caption(format!(
                        "Target BTCB2 Cube: {} · {}",
                        signed_pair_destination(fork.psbt()),
                        signed_pair_amount(fork.psbt())
                    )));
                for coin in bitcoin.claimed_prevouts() {
                    body = body.push(caption(format!("Shared input: {coin}")));
                }
            }
            body = body
                .push(
                    Row::new()
                        .spacing(10)
                        .push(action(
                            "Save Bitcoin PSBT",
                            SplitMessage::ExportPairedUnsigned(ChainId::Bitcoin, Encoding::Binary),
                        ))
                        .push(action(
                            "Import Bitcoin signatures",
                            SplitMessage::ImportPairedSigned(ChainId::Bitcoin),
                        )),
                )
                .push(
                    Row::new()
                        .spacing(10)
                        .push(action(
                            "Save BTCB2 PSBT",
                            SplitMessage::ExportPairedUnsigned(
                                ChainId::BitcoinBlake2b,
                                Encoding::Binary,
                            ),
                        ))
                        .push(action(
                            "Import BTCB2 signatures",
                            SplitMessage::ImportPairedSigned(ChainId::BitcoinBlake2b),
                        )),
                );
            (body, actions) = device_section(panel, DeviceStep::PairedBitcoin, body, actions);
            (body, actions) = device_section(panel, DeviceStep::PairedFork, body, actions);
        }
        Stage::ChooseRoute => {
            body = body.push(p1_regular(step1::RETIRED_MARKER).style(theme::text::warning));
        }
        Stage::Unified(stage) => {
            (body, actions) = unified_body(panel, *stage, body, actions);
        }
        Stage::NeedsSession => {
            body = body.push(p1_regular(
                "Sign in to Connect in this Cube to continue the split recorded on this device.",
            ));
            actions = actions.push(action("Try again", SplitMessage::Retry));
        }
        Stage::Working(work) => {
            body = body.push(p1_bold(working(*work)));
        }
        Stage::Sign => {
            if !panel.can_sign_or_submit() {
                body = body.push(p1_regular(
                    "This recorded split remains available for inspection.",
                ));
            } else {
                if let Some(construction) = panel.construction() {
                    body = body
                        .push(p1_regular(format!(
                            "{} input(s) · fee {} sats · at most {} vB",
                            construction.claimed_prevouts().len(),
                            construction.fee().to_sat(),
                            construction.maximum_signed_vbytes()
                        )))
                        .push(caption(format!("Unsigned txid {}", construction.txid())));
                }
                if let Some(prepared) = panel.prepared() {
                    body = body.push(caption(format!(
                        "Fresh destination (receive index {}): {}",
                        prepared.destination, prepared.address
                    )));
                }
                body = body.push(p1_regular(match panel.exported() {
                Some(path) => format!(
                    "Saved to {}. Sign it in the wallet that holds the keys (SIGHASH_ALL, without finalizing), then import the signed file(s).",
                    path.display()
                ),
                None => "Save the unsigned PSBT, sign it in the wallet that holds the keys, then import the signed file(s).".to_string(),
            }));
                if panel.files() > 0 {
                    body = body.push(caption(format!("{} signed file(s) loaded", panel.files())));
                }
                actions = actions
                    .push(action(
                        "Save PSBT",
                        SplitMessage::ExportUnsigned(Encoding::Binary),
                    ))
                    .push(action(
                        "Save as text",
                        SplitMessage::ExportUnsigned(Encoding::Base64),
                    ))
                    .push(primary("Import signed", SplitMessage::ImportSigned));
                (body, actions) = device_section(panel, DeviceStep::Step1, body, actions);
            }
        }
        Stage::Ready => {
            if panel.can_sign_or_submit() {
                body = body.push(p1_regular(
                "Step 1 is signed and recorded on this device. Nothing has been sent. Review it to submit through Connect.",
            ));
                actions = actions.push(primary("Review", SplitMessage::Review));
            } else {
                body = body.push(p1_regular(
                    "This signed split remains recorded on this device.",
                ));
            }
        }
        Stage::Review => {
            if let Some(review) = panel.review() {
                body = body
                    .push(p1_bold(format!("Txid {}", review.txid)))
                    .push(p1_regular(format!(
                        "Fee {} sats · {} vB · route {}",
                        review.fee_sats,
                        review.vsize,
                        describe_route(&review.route)
                    )))
                    .push(caption(format!(
                        "Bitcoin tip {} · Bitcoin Blake2b tip {}",
                        review.bitcoin_tip, review.fork_tip
                    )));
                if let Some(left) = review.rdts_left {
                    body = body.push(caption(format!(
                        "Replay protection left: {}",
                        crate::app::state::vault::claim::describe_duration(left)
                    )));
                }
            }
            if panel.can_sign_or_submit() {
                actions = actions
                    .push(primary("Submit", SplitMessage::Confirm))
                    .push(action("Review again", SplitMessage::Review));
            }
        }
        Stage::Tracking => {
            body = body.push(p1_regular(match (panel.phase(), panel.outcome()) {
                (_, Some(Outcome::UpstreamAccepted { txid, .. })) => {
                    format!("Connect accepted {txid}. Waiting for confirmation.")
                }
                (Some(Phase::Tracking), _) => "Step 1 was seen on chain.".to_string(),
                _ => "A submission of step 1 is recorded but not confirmed. It is never sent again automatically; check its status.".to_string(),
            }));
            if panel.reorged() {
                body = body.push(p1_regular(step1::REORGED).style(theme::text::warning));
                actions = actions.push(primary("Check the reorg", SplitMessage::CheckReorg));
            } else if let Some(confirmations) = panel.confirmations() {
                body = body.push(p1_bold(format!(
                    "Bitcoin confirmations: {confirmations} of {MIN_CONFIRMATIONS}"
                )));
                body = body.push(p1_regular(if confirmations >= MIN_CONFIRMATIONS {
                    if panel.can_enter_step2() {
                        "Step 1 has the confirmations step 2 needs. Continue to step 2 on Bitcoin Blake2b.".to_string()
                    } else if let Some(copy) = panel.step2_unavailable_copy() {
                        // S3-D4: why there is no step-2 port.
                        copy.to_string()
                    } else {
                        "Step 1 has the confirmations step 2 needs; the split stays recorded on this device.".to_string()
                    }
                } else {
                    format!(
                        "Step 2 waits for {MIN_CONFIRMATIONS} confirmations of step 1, rechecked at the tip of both chains. Check again later."
                    )
                }));
            } else if let Some(status) = panel.status() {
                body = body.push(caption(format!("Last check: {status:?}")));
            }
            if panel.can_enter_step2() {
                actions = actions.push(primary("Continue to step 2", SplitMessage::EnterStep2));
            }
            actions = actions.push(action("Refresh", SplitMessage::Reconcile));
        }
        Stage::Step2(stage) => {
            (body, actions) = step2_body(panel, *stage, body, actions);
        }
        Stage::Reconfirm {
            previous,
            confirmed,
        } => {
            body = body
                .push(p1_bold("Step 1 was mined again in another block"))
                .push(p1_regular(format!(
                    "It was confirmed at height {} (block {}) and is now at height {} (block {}). Step 2 stays blocked until you acknowledge the new block; its confirmations then count from it.",
                    previous.height, previous.hash, confirmed.height, confirmed.hash
                )));
            actions = actions
                .push(primary(
                    "Acknowledge new block",
                    SplitMessage::AcknowledgeReconfirmation,
                ))
                .push(action("Refresh", SplitMessage::Reconcile));
        }
        Stage::Resend => {
            body = body.push(p1_bold(
                "Step 1 was dropped from Bitcoin by a reorg. Step 2 is blocked.",
            ));
            if let Some(review) = panel.review() {
                body = body
                    .push(p1_regular(format!(
                        "Recorded step 1 ({}): fee {} sats · {} vB · route {}.",
                        review.txid,
                        review.fee_sats,
                        review.vsize,
                        describe_route(&review.route)
                    )))
                    .push(caption(format!(
                        "Bitcoin tip {} · Bitcoin Blake2b tip {}",
                        review.bitcoin_tip, review.fork_tip
                    )));
            }
            if panel.can_sign_or_submit() {
                actions = actions.push(primary("Send again", SplitMessage::ConfirmResend));
            }
            actions = actions.push(action("Refresh", SplitMessage::Reconcile));
        }
        Stage::Refused(refusal) => {
            body = body.push(p1_regular(refusal.reason.clone()).style(theme::text::warning));
            if refusal.retry {
                actions = actions.push(action("Try again", SplitMessage::Retry));
            } else if refusal.recovery == step1::RefusalRecovery::ReopenCube {
                body = body.push(p1_regular(
                    "This split cannot continue in the current session. Close and reopen the Cube to reload its recorded split. If this refusal persists, the connection or wallet identity must be corrected before continuing.",
                ));
            }
        }
        Stage::Abandoned => {
            body = body.push(p1_regular(
                "The unsubmitted split was abandoned and its record deleted from this device.",
            ));
        }
        Stage::Closed => {
            body = body.push(p1_regular(
                // #662 F2: a closed split restarts here before its kind is
                // read, so one sentence fits both routes.
                CLOSED_COPY,
            ));
        }
    }
    if let Some(txid) = panel.tracked_txid() {
        body = body.push(caption(format!("Tracked step-1 txid {txid}")));
    }
    for line in warning_lines(panel) {
        body = body.push(p1_regular(line).style(theme::text::warning));
    }
    // A completed split offers only Refresh and Close.
    if panel.can_sign_or_submit()
        && panel.signed().is_some()
        && !matches!(
            panel.stage(),
            Stage::Working(_) | Stage::Step2(Step2Stage::Completed)
        )
    {
        actions = actions.push(action(
            "Save signed transaction",
            SplitMessage::ExportSigned,
        ));
    }
    if panel.can_confirm_abandon() {
        body = body.push(caption(
            "Bitcoin shows neither this step 1 nor any spend of its coins. Abandoning deletes the record from this device.",
        ));
        actions = actions.push(action("Abandon split", SplitMessage::ConfirmAbandon));
    } else if panel.can_check_abandon() {
        actions = actions.push(action(
            "Check before abandoning",
            SplitMessage::CheckAbandon,
        ));
    } else if let Some(conflict) = panel
        .dead_end()
        .and_then(|dead_end| dead_end.conflict)
        .filter(|_| panel.can_check_close())
    {
        // #568 S4b, O4: the exit of a terminal step-1 conflict.
        if panel.can_confirm_close() {
            body = body.push(caption(step2::conflict_closable_copy(&conflict)));
            actions = actions.push(action("Close split", SplitMessage::ConfirmAbandon));
        } else {
            actions = actions.push(action("Check before closing", SplitMessage::CheckAbandon));
        }
    } else if panel.can_confirm_close() {
        body = body.push(caption(
            "Bitcoin Blake2b shows neither this step 2 nor any spend of its coins, and step 1 is six deep on Bitcoin. Abandoning closes this split on this device: its record and the signed step 2 are kept, and a new split of this wallet stays refused until that record is reset.",
        ));
        actions = actions.push(action("Abandon split", SplitMessage::ConfirmAbandon));
    } else if panel.can_check_close() {
        actions = actions.push(action(
            "Check before abandoning",
            SplitMessage::CheckAbandon,
        ));
    }
    // B4b-3c (C6): the fork-only close.
    if panel.can_confirm_unified_close() {
        let submitted = panel.unified().record().is_some_and(|r| r.sweep.is_some());
        body = body.push(caption(if submitted {
            unified::CLOSE_CHECKED
        } else {
            unified::CLOSE_UNSUBMITTED
        }));
        actions = actions.push(action(
            "Close this split",
            SplitMessage::Unified(UnifiedMessage::ConfirmClose),
        ));
    } else if panel.can_check_unified_close() {
        actions = actions.push(action(
            "Check before closing",
            SplitMessage::Unified(UnifiedMessage::CheckClose),
        ));
    }
    actions = actions.push(action("Close", SplitMessage::Close));

    Container::new(card::simple(
        body.push(Space::new().height(Length::Fixed(4.0)))
            .push(actions)
            .padding(24),
    ))
    .width(Length::Fill)
    .height(Length::Fill)
    .center_x(Length::Fill)
    .center_y(Length::Fill)
    .padding(24)
    .style(theme::container::custom(iced::Color::from_rgba(
        0.0, 0.0, 0.0, 0.6,
    )))
    .into()
}
