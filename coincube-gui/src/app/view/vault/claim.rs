//! Claim step 1 views (Lane B1.5). Pure render functions over
//! [`ClaimStep1Panel`]'s stage; every button is an intent the panel
//! re-checks, never a permission.

use iced::{
    widget::{Column, Container, Row, Space},
    Alignment, Length,
};

use coincube_core::{
    claim::{Assessment, MIN_CONFIRMATIONS},
    claim_spend::PoisonSelfTransfer,
    miniscript::bitcoin::Amount,
};
use coincube_ui::{
    component::{amount::*, button, card, text::*},
    icon, theme,
    widget::{ColumnExt, Element},
};

use crate::{
    app::{
        cache::Cache,
        menu::Menu,
        state::vault::{
            claim::{
                describe_duration, ClaimStep1Panel, CoinSet, ForkWindow, Preconditions, Refusal,
                StageView,
            },
            psbt::{PsbtModal, PsbtState},
        },
        view::{dashboard, message::*, vault::psbt},
    },
    services::{
        claim_coordinator::{Outcome, ReviewSnapshot},
        claim_workflow::{Phase, Status},
    },
};

const TITLE: &str = "Claim Bitcoin Blake2b — step 1";

pub fn view<'a>(
    menu: &'a Menu,
    cache: &'a Cache,
    panel: &'a ClaimStep1Panel,
) -> Element<'a, Message> {
    match panel.stage() {
        StageView::Preconditions(pre, refusal) => {
            dashboard(menu, cache, preconditions_view(cache, panel, pre, refusal))
        }
        StageView::Plan(built) => dashboard(menu, cache, plan_view(cache, panel, built)),
        StageView::Sign {
            psbt: state,
            finalizing,
            error,
        } => sign_view(menu, cache, state, finalizing, error),
        StageView::Review {
            snapshot,
            busy,
            error,
        } => dashboard(menu, cache, review_view(cache, snapshot, busy, error)),
        StageView::Track {
            outcome,
            phase,
            status,
            busy,
            error,
        } => dashboard(menu, cache, track_view(outcome, phase, status, busy, error)),
    }
}

fn header<'a>(step: &'static str) -> Element<'a, Message> {
    Column::new()
        .spacing(5)
        .push(h3(TITLE).bold())
        .push(p1_regular(step).style(theme::text::secondary))
        .into()
}

fn row<'a>(label: &'static str, value: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    Row::new()
        .spacing(10)
        .align_y(Alignment::Center)
        .push(p1_regular(label).width(Length::Fixed(180.0)))
        .push(value)
        .into()
}

fn check_row<'a>(label: &'static str, ok: Option<bool>, detail: String) -> Element<'a, Message> {
    let mark = match ok {
        Some(true) => icon::check_icon().style(theme::text::success),
        Some(false) => icon::warning_icon().style(theme::text::warning),
        None => icon::reload_icon().style(theme::text::secondary),
    };
    Row::new()
        .spacing(10)
        .align_y(Alignment::Center)
        .push(mark)
        .push(p1_regular(label).width(Length::Fixed(170.0)))
        .push(p1_regular(detail).style(theme::text::secondary))
        .into()
}

fn preconditions_view<'a>(
    cache: &'a Cache,
    panel: &'a ClaimStep1Panel,
    pre: &'a Preconditions,
    refusal: Option<Refusal>,
) -> Element<'a, Message> {
    let checked = pre.checked.as_ref();
    let window = panel.window();
    let coins = panel.coins();
    let checklist = Column::new()
        .spacing(10)
        .push(check_row(
            "Claim target",
            Some(pre.target.is_some()),
            match &pre.target {
                Some(_) => "A Bitcoin Blake2b Cube reuses this Vault on this device.".to_string(),
                None => "Not created yet.".to_string(),
            },
        ))
        .push(check_row(
            "Vault",
            Some(pre.shape.is_none()),
            match &pre.shape {
                Some(reason) => reason.clone(),
                None => "Native SegWit, single-key primary path.".to_string(),
            },
        ))
        .push(check_row(
            "Node backend",
            checked.map(|c| c.backend.is_ok()),
            match checked.map(|c| &c.backend) {
                None => "Checking…".to_string(),
                Some(Ok(())) => "Coincube's Bitcoin service.".to_string(),
                Some(Err(_)) => "Not usable for a claim.".to_string(),
            },
        ))
        .push(check_row(
            "Replay protection",
            window.map(|w| w.rdts.is_ok()),
            match (checked.map(|c| &c.window), window) {
                (None, _) => "Checking Bitcoin Blake2b…".to_string(),
                (Some(Err(_)), _) => "Couldn't read Bitcoin Blake2b's status.".to_string(),
                (_, Some(w)) => rdts_detail(w),
                (Some(Ok(_)), None) => String::new(),
            },
        ))
        .push(check_row(
            "Coins to split",
            coins.map(|c| !c.pre_fork.is_empty()),
            match (checked.map(|c| &c.coins), coins) {
                (None, _) => "Reading this Vault's coins…".to_string(),
                (Some(Err(_)), _) => "Couldn't read this Vault's coins.".to_string(),
                (_, Some(c)) => coins_detail(c, cache),
                (Some(Ok(_)), None) => String::new(),
            },
        ))
        .push(check_row(
            "Fee rate",
            checked.map(|c| c.feerate_vb.is_ok()),
            match panel.feerate_vb() {
                Some(rate) => format!("{rate} sat/vB (about an hour)."),
                None if checked.is_none() => "Fetching…".to_string(),
                None => "Couldn't fetch a fee rate.".to_string(),
            },
        ));

    let action: Element<'a, Message> = match refusal {
        Some(Refusal { reason, retry }) => Column::new()
            .spacing(15)
            .push(card::warning(reason))
            .push_maybe(retry.then(|| {
                button::secondary(Some(icon::reload_icon()), "Check again").on_press_maybe(
                    (!pre.checking).then_some(Message::Claim(ClaimMessage::Recheck)),
                )
            }))
            .into(),
        None => Row::new()
            .spacing(15)
            .push(
                button::secondary(Some(icon::reload_icon()), "Check again").on_press_maybe(
                    (!pre.checking).then_some(Message::Claim(ClaimMessage::Recheck)),
                ),
            )
            .push(
                button::primary(None, "Build the transaction")
                    .on_press_maybe(
                        panel
                            .can_build()
                            .then_some(Message::Claim(ClaimMessage::Build)),
                    )
                    .width(Length::Fixed(240.0)),
            )
            .into(),
    };

    Column::new()
        .spacing(20)
        .push(header("Step 1 of 2 — make your Bitcoin coins unspendable on Bitcoin Blake2b"))
        .push(p1_regular(
            "Step 1 sends every coin this Vault held before the fork back to itself, with a small marker \
             that Bitcoin Blake2b rejects while its replay protection is active. Nothing leaves the Vault. \
             Step 2, sweeping the same coins on Bitcoin Blake2b, is a later release.",
        ))
        .push(
            Container::new(checklist)
                .padding(20)
                .style(theme::card::simple)
                .width(Length::Fill),
        )
        .push(action)
        .into()
}

fn rdts_detail(window: &ForkWindow) -> String {
    let left = window
        .expires_at
        .saturating_sub(window.median_time_past)
        .max(0);
    match window.rdts {
        Ok(()) => format!(
            "Active on Bitcoin Blake2b; about {} until it expires.",
            describe_duration(left)
        ),
        Err(Assessment::ExpiryMargin) => format!("Expires in about {}.", describe_duration(left)),
        Err(Assessment::RdtsExpired) => "Expired.".to_string(),
        Err(Assessment::RdtsScheduled | Assessment::RdtsInactive) => "Not active yet.".to_string(),
        Err(other) => format!("{other:?}."),
    }
}

fn coins_detail(coins: &CoinSet, cache: &Cache) -> String {
    let total: Amount = coins.pre_fork.iter().map(|c| c.amount).sum();
    let mut detail = format!(
        "{} coin{} from before the fork, {}.",
        coins.pre_fork.len(),
        if coins.pre_fork.len() == 1 { "" } else { "s" },
        format_amount(total, cache),
    );
    if coins.post_fork > 0 {
        detail.push_str(&format!(
            " {} newer coin{} stay{} as {} — only Bitcoin knows {}.",
            coins.post_fork,
            if coins.post_fork == 1 { "" } else { "s" },
            if coins.post_fork == 1 { "s" } else { "" },
            if coins.post_fork == 1 {
                "it is"
            } else {
                "they are"
            },
            if coins.post_fork == 1 { "it" } else { "them" },
        ));
    }
    detail
}

fn format_amount(amount: Amount, cache: &Cache) -> String {
    match cache.bitcoin_unit {
        BitcoinDisplayUnit::Sats => format!("{} sats", format_u64_as_string(amount.to_sat(), " ")),
        _ => format!("{} BTC", format_btc_string(amount.to_btc())),
    }
}

fn plan_view<'a>(
    cache: &'a Cache,
    panel: &'a ClaimStep1Panel,
    built: &'a PoisonSelfTransfer,
) -> Element<'a, Message> {
    let tx = &built.psbt().unsigned_tx;
    let inputs: Amount = built
        .psbt()
        .inputs
        .iter()
        .filter_map(|input| input.witness_utxo.as_ref().map(|o| o.value))
        .sum();
    let outputs: Amount = tx.output.iter().map(|o| o.value).sum();
    let fee = inputs.checked_sub(outputs).unwrap_or(Amount::ZERO);
    let marker = tx
        .output
        .iter()
        .find(|o| o.script_pubkey.is_op_return())
        .map(|o| o.script_pubkey.len())
        .unwrap_or(0);
    let mut summary = Column::new()
        .spacing(10)
        .push(row(
            "Coins spent",
            p1_regular(format!(
                "{} — {}",
                tx.input.len(),
                format_amount(inputs, cache)
            )),
        ))
        .push(row(
            "Back to this Vault",
            p1_regular(format_amount(outputs, cache)),
        ))
        .push(row(
            "Marker output",
            p1_regular(format!("{marker}-byte OP_RETURN, zero value")),
        ))
        .push(row(
            "Fee",
            p1_regular(format!(
                "{} at {} sat/vB",
                format_amount(fee, cache),
                panel.feerate_vb().unwrap_or(0)
            )),
        ));
    for warning in built.warnings() {
        summary = summary.push(
            Row::new()
                .spacing(5)
                .push(icon::warning_icon().style(theme::text::warning))
                .push(p1_regular(warning.to_string()).style(theme::text::warning)),
        );
    }
    Column::new()
        .spacing(20)
        .push(header("Review the transaction, then sign it with this Vault's keys"))
        .push(
            Container::new(summary)
                .padding(20)
                .style(theme::card::simple)
                .width(Length::Fill),
        )
        .push(p1_regular(
            "Signing uses the same flow as any Vault spend. Nothing is broadcast until you confirm the \
             final review.",
        ))
        .push(
            Row::new()
                .spacing(15)
                .push(
                    button::secondary(None, "Cancel").on_press(Message::Claim(ClaimMessage::Cancel)),
                )
                .push(
                    button::primary(None, "Sign")
                        .on_press(Message::Claim(ClaimMessage::Sign))
                        .width(Length::Fixed(200.0)),
                ),
        )
        .into()
}

fn sign_view<'a>(
    menu: &'a Menu,
    cache: &'a Cache,
    state: &'a PsbtState,
    finalizing: bool,
    error: Option<&'a str>,
) -> Element<'a, Message> {
    let currently_signing = matches!(&state.modal, Some(PsbtModal::Sign(m)) if m.is_signing());
    let content = dashboard(
        menu,
        cache,
        Column::new()
            .spacing(20)
            .push(header(if finalizing {
                "Checking the signatures and recording the claim…"
            } else {
                "Sign with this Vault's keys"
            }))
            .push_maybe(error.map(|reason| card::warning(reason.to_string())))
            .push(psbt::spend_overview_view(
                &state.tx,
                &state.desc_policy,
                &state.wallet.keys_aliases,
                currently_signing,
                state.saved,
                None,
            ))
            .push_maybe((!finalizing).then(|| {
                button::secondary(None, "Cancel").on_press(Message::Claim(ClaimMessage::Cancel))
            })),
    );
    match &state.modal {
        Some(modal) => {
            let modal: &dyn crate::app::state::vault::psbt::Modal = modal.as_ref();
            modal.view(content)
        }
        None => content,
    }
}

fn review_view<'a>(
    cache: &'a Cache,
    snapshot: Option<&'a ReviewSnapshot>,
    busy: bool,
    error: Option<&'a str>,
) -> Element<'a, Message> {
    let body: Element<'a, Message> = match snapshot {
        Some(snapshot) => Column::new()
            .spacing(10)
            .push(row("Transaction", p2_regular(snapshot.txid.to_string())))
            .push(row(
                "Fee",
                p1_regular(format!(
                    "{} ({} vB)",
                    format_amount(Amount::from_sat(snapshot.fee_sats), cache),
                    snapshot.vsize
                )),
            ))
            .push(row(
                "Bitcoin tip",
                p1_regular(format!(
                    "height {} — the node accepts this transaction",
                    snapshot.observations.bitcoin.tip.height
                )),
            ))
            .push(row(
                "Bitcoin Blake2b tip",
                p1_regular(format!(
                    "height {} — replay protection active, transaction not seen there",
                    snapshot.observations.fork.tip.height
                )),
            ))
            .into(),
        None if busy => p1_regular("Reading both chains…")
            .style(theme::text::secondary)
            .into(),
        None => p1_regular("No current review. Read the chains again to get one.")
            .style(theme::text::secondary)
            .into(),
    };
    Column::new()
        .spacing(20)
        .push(header("Final review — this is the last step before broadcast"))
        .push_maybe(error.map(|reason| card::warning(reason.to_string())))
        .push(
            Container::new(body)
                .padding(20)
                .style(theme::card::simple)
                .width(Length::Fill),
        )
        .push(p1_regular(
            "Submitting broadcasts this transaction on Bitcoin. It cannot be taken back. The review \
             above is what you are confirming; if the chains move first, you will be asked to review again.",
        ))
        .push(
            Row::new()
                .spacing(15)
                .push(
                    button::secondary(Some(icon::reload_icon()), "Review again")
                        .on_press_maybe((!busy).then_some(Message::Claim(ClaimMessage::Refresh))),
                )
                .push(
                    button::primary(None, "Submit to Bitcoin")
                        .on_press_maybe(
                            (!busy && snapshot.is_some())
                                .then_some(Message::Claim(ClaimMessage::Confirm)),
                        )
                        .width(Length::Fixed(240.0)),
                ),
        )
        .into()
}

fn track_view<'a>(
    outcome: Outcome,
    phase: Option<Phase>,
    status: Option<Status>,
    busy: bool,
    error: Option<&'a str>,
) -> Element<'a, Message> {
    let (txid, submitted) = match outcome {
        Outcome::UpstreamAccepted { txid, .. } => (
            txid,
            "Accepted by the Bitcoin node. Waiting for it to confirm.".to_string(),
        ),
        Outcome::Uncertain { txid, .. } => (
            txid,
            "The submission's outcome is uncertain: the intent was recorded, but the node's answer \
             did not arrive. It is being reconciled from the chain; it will not be retried."
                .to_string(),
        ),
    };
    let progress = match status {
        None if busy => "Reading both chains…".to_string(),
        None => "Not read yet.".to_string(),
        Some(Status::Unchecked) => "Not read yet.".to_string(),
        Some(Status::Unavailable) => {
            "Couldn't reach the chains. Try again in a moment.".to_string()
        }
        Some(Status::Observation(assessment)) => match assessment {
            Assessment::WaitingForConfirmation => "Waiting for the first confirmation.".to_string(),
            Assessment::WaitingForDepth { confirmations } => {
                format!("{confirmations} of {MIN_CONFIRMATIONS} confirmations.")
            }
            Assessment::ObservationsEligibleForPreflight | Assessment::NeedsPreflightRecheck => {
                format!(
                    "Confirmed with {MIN_CONFIRMATIONS} or more confirmations. Step 2 comes in a later release."
                )
            }
            Assessment::Reorged => {
                "A reorganisation dropped this transaction from the chain. Read again; if it stays \
                 out, step 1 must be run again."
                    .to_string()
            }
            Assessment::Step1AlreadyOnFork => {
                "This transaction was seen on Bitcoin Blake2b. The split did not hold; do not \
                 proceed to step 2."
                    .to_string()
            }
            other => format!("{other:?}."),
        },
    };
    let reorged = matches!(
        status,
        Some(Status::Observation(
            Assessment::Reorged | Assessment::Step1AlreadyOnFork
        ))
    );
    Column::new()
        .spacing(20)
        .push(header("Submitted — tracking step 1 on both chains"))
        .push_maybe(error.map(|reason| card::warning(reason.to_string())))
        .push(
            Container::new(
                Column::new()
                    .spacing(10)
                    .push(row("Transaction", p2_regular(txid.to_string())))
                    .push(row("Submission", p1_regular(submitted)))
                    .push(row(
                        "Recorded as",
                        p1_regular(match phase {
                            Some(Phase::Intent) => "intent",
                            Some(Phase::BroadcastUncertain) => "broadcast (unconfirmed)",
                            Some(Phase::Tracking) => "confirmed, tracking depth",
                            None => "—",
                        }),
                    ))
                    .push(if reorged {
                        row("Progress", p1_regular(progress).style(theme::text::warning))
                    } else {
                        row("Progress", p1_regular(progress))
                    }),
            )
            .padding(20)
            .style(theme::card::simple)
            .width(Length::Fill),
        )
        .push(
            Row::new().spacing(15).push(
                button::secondary(Some(icon::reload_icon()), "Read again")
                    .on_press_maybe((!busy).then_some(Message::Claim(ClaimMessage::Refresh))),
            ),
        )
        .push(Space::new().height(Length::Fixed(10.0)))
        .into()
}
