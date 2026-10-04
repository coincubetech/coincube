//! Split panel view (#568 B1b, step 2 in B3b-2b-2). Pure rendering of
//! [`SplitPanel`]; every action is a [`SplitMessage`]. There is no "start"
//! action: a fresh split is not reachable before B5 (D1), and step 2 is
//! reachable only from a resumed journal whose step 1 has six confirmations.

use coincube_core::claim::MIN_CONFIRMATIONS;
use coincube_ui::{
    component::{button, card, text::*},
    theme,
    widget::Element,
};
use iced::{
    widget::{Column, Container, Row, Space},
    Alignment, Length,
};

use crate::{
    app::{
        state::vault::split::{
            step1::{self, describe_route},
            step2, SplitMessage, SplitPanel, Stage, Step2Stage, Work,
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

fn working(work: Work) -> &'static str {
    match work {
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
        Work::CheckingClose => "Checking both chains before abandoning…",
        Work::Closing => "Abandoning and closing the split…",
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
    }
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
            if panel.can_check_close() {
                body = body.push(p1_regular(
                    "This version can't send this step 2 again: its last attempt was accepted or may have left, or no resend is left. If Bitcoin Blake2b never shows it, you can abandon this split after a check.",
                ));
            }
            actions = actions.push(action("Refresh", SplitMessage::Step2Reconcile));
        }
    }
    (body, actions)
}

pub fn split_panel(panel: &SplitPanel) -> Element<'_, Message> {
    let mut body = Column::new()
        .spacing(10)
        .max_width(640)
        .push(h3("Split: step 1 on Bitcoin"))
        .push(
            p1_regular(
                "Step 1 moves the foreign wallet's pre-fork coins to a fresh address of the same wallet on Bitcoin, with an output Bitcoin Blake2b refuses. Nothing here holds keys; signatures come back in PSBT files.",
            )
            .style(theme::text::secondary),
        );
    let mut actions = Row::new().spacing(10).align_y(Alignment::Center);

    match panel.stage() {
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
        }
        Stage::Ready => {
            body = body.push(p1_regular(
                "Step 1 is signed and recorded on this device. Nothing has been sent. Review it to submit through Connect.",
            ));
            actions = actions.push(primary("Review", SplitMessage::Review));
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
            actions = actions
                .push(primary("Submit", SplitMessage::Confirm))
                .push(action("Review again", SplitMessage::Review));
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
                    } else {
                        "Step 1 has the confirmations step 2 needs. Step 2 needs this Vault's wallet engine running; the split stays recorded on this device.".to_string()
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
                        "Send exactly the same signed step 1 again ({}): fee {} sats · {} vB · route {}. A fresh preflight accepted it.",
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
            actions = actions
                .push(primary("Send again", SplitMessage::ConfirmResend))
                .push(action("Refresh", SplitMessage::Reconcile));
        }
        Stage::Refused(refusal) => {
            body = body.push(p1_regular(refusal.reason.clone()).style(theme::text::warning));
            if refusal.retry {
                actions = actions.push(action("Try again", SplitMessage::Retry));
            }
        }
        Stage::Abandoned => {
            body = body.push(p1_regular(
                "The unsubmitted split was abandoned and its record deleted from this device.",
            ));
        }
        Stage::Closed => {
            body = body.push(p1_regular(
                "This split was abandoned and closed on this device. Its record, with the signed step 2, is kept, and a new split of this wallet is refused until that record is reset.",
            ));
        }
    }
    if let Some(txid) = panel.tracked_txid() {
        body = body.push(caption(format!("Tracked step-1 txid {txid}")));
    }
    for line in warning_lines(panel) {
        body = body.push(p1_regular(line).style(theme::text::warning));
    }
    if panel.signed().is_some() && !matches!(panel.stage(), Stage::Working(_)) {
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
