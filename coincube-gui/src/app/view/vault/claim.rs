//! Claim step 1 views (Lane B1.5). Pure render functions over
//! [`ClaimStep1Panel`]'s stage; every button is an intent the panel
//! re-checks, never a permission.

use iced::{
    widget::{scrollable, Column, Container, Row, Space},
    Alignment, Length,
};

use coincube_core::{
    claim::{Assessment, MIN_CONFIRMATIONS},
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
            claim::{ClaimStep1Panel, CoinSet, Preconditions, Refusal, StageView},
            psbt::{PsbtModal, PsbtState},
        },
        view::{
            dashboard,
            message::*,
            vault::{ancestry_coins::CoinReview, psbt},
        },
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
        StageView::FundingChecking => dashboard(
            menu,
            cache,
            Column::new()
                .spacing(20)
                .push(header("Checking two-chain deposit addresses…"))
                .push(p1_regular("Tenshu is scanning this Vault, saving two distinct receive addresses and checking their history on both chains."))
                .push(button::secondary(None, "Cancel").on_press(Message::Claim(ClaimMessage::Cancel))),
        ),
        StageView::FundingDeposits(addresses) => dashboard(
            menu,
            cache,
            Column::new()
                .spacing(20)
                .push(header("Prepare Bitcoin and Bitcoin Blake2b deposits"))
                .push(p1_regular("These are separate receive addresses in this Vault. The Bitcoin Vault is prepared to detect funds at both indices."))
                .push(p1_bold(format!("Bitcoin deposit — at least {} sats ({} sat/vB)", addresses.bitcoin_minimum_sats, addresses.bitcoin_rate_sat_vb)))
                .push(address_row(addresses.bitcoin_deposit.clone(), "Copied Bitcoin deposit address to clipboard"))
                .push(p1_bold(format!("Bitcoin Blake2b deposit — at least {} sats ({} sat/vB)", addresses.btcb2_minimum_sats, addresses.btcb2_rate_sat_vb)))
                .push(address_row(addresses.btcb2_deposit.clone(), "Copied BTCB2 deposit address to clipboard"))
                .push(p1_regular("Send only Bitcoin to the Bitcoin address and only BTCB2 to the BTCB2 address. These minimums use fee quotes from when the addresses were prepared; if fees rise before signing, you may need to add funds. Both deposits must later pass independent ancestry, confirmation, ownership and unspent checks; neither deposit by itself splits the coins."))
                .push(button::secondary(None, if panel.funding_checking() { "Checking deposits…" } else { "Check deposits" }).on_press_maybe(
                    (!panel.funding_checking()).then_some(Message::Claim(ClaimMessage::CheckFunding))
                ))
                .push_maybe(panel.funding_status().map(|status| p1_regular(status.to_string())))
                .push(button::secondary(None, "Close").on_press(Message::Claim(ClaimMessage::Cancel))),
        ),
        StageView::Plan(built) => dashboard(menu, cache, plan_view(cache, panel, built)),
        StageView::CheckingSign => dashboard(
            menu,
            cache,
            Column::new()
                .spacing(20)
                .push(header("Checking the Claim inputs before signing…"))
                .push(
                    button::secondary(None, "Cancel")
                        .on_press(Message::Claim(ClaimMessage::Cancel)),
                ),
        ),
        StageView::Sign {
            psbt: state,
            finalizing,
            error,
        } if panel.can_sign_or_submit() => sign_view(menu, cache, state, finalizing, error),
        StageView::Sign { .. } => dashboard(
            menu,
            cache,
            Column::new()
                .spacing(20)
                .push(header("Recorded claim"))
                .push(p1_regular("This claim remains recorded on this device."))
                .push(
                    button::secondary(None, "Close").on_press(Message::Claim(ClaimMessage::Cancel)),
                ),
        ),
        StageView::Review {
            snapshot,
            busy,
            error,
        } => dashboard(
            menu,
            cache,
            review_view(
                cache,
                panel.wallet().chain,
                snapshot,
                busy,
                error,
                panel.can_broadcast_review(),
            ),
        ),
        StageView::Track {
            outcome,
            phase,
            status,
            busy,
            error,
        } => dashboard(
            menu,
            cache,
            track_view(
                outcome,
                phase,
                status,
                busy,
                error,
                panel.can_continue_on_fork(),
                (panel.reconfirmation(), panel.resubmission()),
            ),
        ),
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

fn return_address_row<'a>(address: String) -> Element<'a, Message> {
    address_row(address, "Copied return address to clipboard")
}

fn address_row<'a>(address: String, notice: &'static str) -> Element<'a, Message> {
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
    let coins = panel.coins();
    let checklist = Column::new()
        .spacing(10)
        .push(check_row(
            "Claim target",
            Some(pre.target.is_some()),
            match &pre.target {
                Some(_) => "A Bitcoin Blake2b Cube reuses this Vault on this device.".to_string(),
                None => "Missing or ambiguous target.".to_string(),
            },
        ))
        .push(check_row(
            "Vault",
            Some(pre.shape.is_none()),
            match &pre.shape {
                Some(reason) => reason.clone(),
                None => "Native SegWit Vault.".to_string(),
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

    let action: Element<'a, Message> = if !panel.can_sign_or_submit() {
        Column::new()
            .spacing(15)
            .push_maybe(
                panel
                    .funding_error()
                    .map(|reason| card::warning(reason.to_string())),
            )
            .push(
                button::primary(None, "Prepare two deposit addresses").on_press_maybe(
                    panel
                        .can_prepare_funding()
                        .then_some(Message::Claim(ClaimMessage::StartFunding)),
                ),
            )
            .into()
    } else {
        match refusal {
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
        }
    };

    Column::new()
        .spacing(20)
        .push(header("Step 1 of 2 — split on Bitcoin"))
        .push(p1_regular("Your paired Blake2b Cube is set up, but creating the Cube alone did not split or claim coins."))
        .push(p1_regular(
            "Claim requires one confirmed Bitcoin-only coin and one confirmed Bitcoin Blake2b-only coin in this Vault. Both must be verified before either transfer can be signed.",
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

fn coins_detail(coins: &CoinSet, cache: &Cache) -> String {
    let total: Amount = coins.pre_fork.iter().map(|c| c.amount).sum();
    let mut detail = format!(
        "{} coin{} from before the fork, {}.",
        coins.pre_fork.len(),
        if coins.pre_fork.len() == 1 { "" } else { "s" },
        format_amount(total, cache),
    );
    if coins.post_fork > 0 {
        // A coin confirmed after the fork is not split by this step; a
        // transaction can still be replayed onto the fork, so it may remain
        // entangled — nothing here says otherwise.
        detail.push_str(&format!(
            " {} newer coin{} {} left out of this step and may still be entangled.",
            coins.post_fork,
            if coins.post_fork == 1 { "" } else { "s" },
            if coins.post_fork == 1 { "is" } else { "are" },
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
    built: &'a crate::app::state::vault::claim::construction::Construction,
) -> Element<'a, Message> {
    let tx = &built.psbt().unsigned_tx;
    let coin_review = match built {
        crate::app::state::vault::claim::construction::Construction::Ancestry {
            transfer, ..
        } => Some(CoinReview::from_psbt(
            built.psbt(),
            transfer.claimed_prevouts(),
            transfer.poison_input(),
        )),
        crate::app::state::vault::claim::construction::Construction::OpReturn(_) => None,
    };
    let inputs: Amount = built
        .psbt()
        .inputs
        .iter()
        .filter_map(|input| input.witness_utxo.as_ref().map(|o| o.value))
        .sum();
    let outputs: Amount = tx.output.iter().map(|o| o.value).sum();
    let fee = inputs.checked_sub(outputs).unwrap_or(Amount::ZERO);
    let return_address = built.verified_return_address(panel.wallet());
    let change_index = built.change_index();
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
        ));
    match &return_address {
        Ok(address) => {
            summary = summary
                .push(p1_regular("Return address").bold())
                .push(return_address_row(address.clone()))
                .push(
                    Row::new()
                        .spacing(10)
                        .align_y(Alignment::Center)
                        .push(icon::check_icon().style(theme::text::success))
                        .push(p2_regular(format!(
                            "Matches this Vault's change branch, index {change_index}."
                        ))),
                );
        }
        Err(error) => {
            summary = summary.push(
                Row::new()
                    .spacing(5)
                    .push(icon::warning_icon().style(theme::text::warning))
                    .push(
                        p1_regular(format!("Return address verification failed: {error}"))
                            .style(theme::text::warning),
                    ),
            );
        }
    }
    match &coin_review {
        Some(Ok(review)) => {
            summary = summary.push(
                p1_regular(format!(
                    "Before fork · present on both chains — {} shared coin{} · {}",
                    review.shared.len(),
                    if review.shared.len() == 1 { "" } else { "s" },
                    format_amount(review.shared_total, cache),
                ))
                .bold(),
            );
            for coin in &review.shared {
                summary = summary.push(row(
                    "Shared coin",
                    p2_regular(format!(
                        "{} · {}",
                        coin.outpoint,
                        format_amount(coin.amount, cache)
                    )),
                ));
            }
            summary = summary.push(row(
                "Bitcoin funding coin",
                p2_regular(format!(
                    "{} · {} · post-fork candidate",
                    review.funding.outpoint,
                    format_amount(review.funding.amount, cache),
                )),
            ));
        }
        Some(Err(reason)) => {
            summary = summary.push(card::warning((*reason).to_string()));
        }
        None => {}
    }
    summary = summary.push(row(
        "Fee",
        p1_regular(match panel.feerate_vb() {
            Some(rate) => format!("{} at {} sat/vB", format_amount(fee, cache), rate),
            None => format_amount(fee, cache),
        }),
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
        .push(header(if panel.is_resuming() { "Review this recorded claim" } else { "Review the transaction, then sign it with this Vault's keys" }))
        .push_maybe(built.selected_ancestry_input().is_none().then(|| card::warning(crate::services::claim_ancestry_gate::NEW_CLAIM_MESSAGE.to_string())))
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
        .push(p2_regular(
            "For an independent check, compare the return address and change index in a separate watch-only copy of this Vault before signing.",
        ).style(theme::text::secondary))
        .push(
            Row::new()
                .spacing(20)
                .push(Space::new().width(Length::Fill))
                .push(
                    button::secondary(None, "Cancel")
                        .on_press(Message::Claim(ClaimMessage::Cancel))
                        .width(Length::Fixed(150.0)),
                )
                .push(
                    button::primary(None, "Sign")
                        .on_press_maybe((panel.can_sign_or_submit() && return_address.is_ok() && matches!(coin_review, Some(Ok(_)))).then_some(Message::Claim(ClaimMessage::Sign)))
                        .width(Length::Fixed(150.0)),
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
            .push(psbt::spend_overview_with_cancel(
                &state.tx,
                &state.desc_policy,
                &state.wallet.keys_aliases,
                currently_signing,
                state.saved,
                None,
                state.broadcast_ready(cache),
                (!finalizing).then_some(Message::Claim(ClaimMessage::Cancel)),
            )),
    );
    match &state.modal {
        Some(modal) => {
            let modal: &dyn crate::app::state::vault::psbt::Modal = modal.as_ref();
            modal.view(content)
        }
        None => content,
    }
}

/// Step 1 has exactly one spendable return output (with an optional zero-value
/// marker). Read it from the coordinator's verified signed transaction.
fn review_destination(
    tx: &coincube_core::miniscript::bitcoin::Transaction,
    chain: coincube_core::chain::ChainId,
) -> Result<(String, Amount), ()> {
    use coincube_core::{chain::ChainId, miniscript::bitcoin::Address};
    if !matches!(chain, ChainId::Bitcoin | ChainId::Testnet4) {
        return Err(());
    }
    let mut destination = None;
    for output in &tx.output {
        if output.script_pubkey.is_op_return() {
            if output.value != Amount::ZERO {
                return Err(());
            }
            continue;
        }
        if destination.is_some() || output.value == Amount::ZERO {
            return Err(());
        }
        let address =
            Address::from_script(&output.script_pubkey, chain.bitcoin_network()).map_err(|_| ())?;
        destination = Some((address.to_string(), output.value));
    }
    destination.ok_or(())
}

fn review_view<'a>(
    cache: &'a Cache,
    chain: coincube_core::chain::ChainId,
    snapshot: Option<&'a ReviewSnapshot>,
    busy: bool,
    error: Option<&'a str>,
    can_broadcast: bool,
) -> Element<'a, Message> {
    let destination = snapshot.map(|snapshot| review_destination(&snapshot.transaction, chain));
    let destination_valid = matches!(&destination, Some(Ok(_)));
    let body: Element<'a, Message> = match snapshot {
        Some(snapshot) => Column::new()
            .spacing(10)
            .push(row("Transaction", p2_regular(snapshot.txid.to_string())))
            .push(match &destination {
                Some(Ok((address, amount))) => Column::new()
                    .spacing(10)
                    .push(row("Back to this Vault", p1_regular(format_amount(*amount, cache))))
                    .push(p1_regular("Return address").bold())
                    .push(return_address_row(address.clone())),
                _ => Column::new().push(card::warning(
                    "Cannot display the return address for this transaction. Review again before submitting.".to_string(),
                )),
            })
            .push(row("Broadcast via", p2_regular(snapshot.route.label())))
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
                    "height {} — {} accepts this transaction",
                    snapshot.observations.bitcoin.tip.height,
                    snapshot.route.label()
                )),
            ))
            .push(row(
                "Bitcoin Blake2b tip",
                p1_regular(if can_broadcast {
                    format!(
                        "height {} — transaction not seen there",
                        snapshot.observations.fork.tip.height
                    )
                } else {
                    format!("height {}", snapshot.observations.fork.tip.height)
                }),
            ))
            .into(),
        None if busy => p1_regular("Reading both chains…")
            .style(theme::text::secondary)
            .into(),
        None => p1_regular("No current review. Read the chains again to get one.")
            .style(theme::text::secondary)
            .into(),
    };
    let mut actions = Row::new().spacing(15).push(
        button::secondary(Some(icon::reload_icon()), "Review again")
            .on_press_maybe((!busy).then_some(Message::Claim(ClaimMessage::Refresh))),
    );
    if can_broadcast {
        actions = actions.push(
            button::primary(None, "Broadcast")
                .on_press_maybe(
                    (!busy && destination_valid).then_some(Message::Claim(ClaimMessage::Confirm)),
                )
                .width(Length::Fixed(240.0)),
        );
    }
    Column::new()
        .spacing(20)
        .push(header(if can_broadcast {
            "Final review — this is the last step before broadcast"
        } else {
            "Recorded claim review"
        }))
        .push_maybe(error.map(|reason| card::warning(reason.to_string())))
        .push(
            Container::new(body)
                .padding(20)
                .style(theme::card::simple)
                .width(Length::Fill),
        )
        .push(p1_regular(if can_broadcast {
            "Broadcasting sends this transaction to the Bitcoin network. It cannot be taken back. The review above is what you are confirming; if the chains move first, you will be asked to review again."
        } else {
            "This recorded transaction remains available for review and tracking."
        }))
        .push(actions)
        .into()
}

fn can_review_reconfirmation(
    status: Option<Status>,
    has_resubmission: bool,
    has_reconfirmation: bool,
) -> bool {
    !has_resubmission
        && !has_reconfirmation
        && status == Some(Status::Observation(Assessment::Reorged))
}

fn track_view<'a>(
    outcome: Outcome,
    phase: Option<Phase>,
    status: Option<Status>,
    busy: bool,
    error: Option<&'a str>,
    can_continue: bool,
    recovery: (
        Option<crate::services::claim_workflow::Reconfirmation>,
        Option<(ReviewSnapshot, usize)>,
    ),
) -> Element<'a, Message> {
    let (reconfirmation, resubmission) = recovery;
    let has_resubmission = resubmission.is_some();
    let (txid, submitted) = match outcome {
        Outcome::Recorded { txid } => (txid, "A submission is recorded on this device. Its transaction has not been recovered and verified yet; it will not be retried automatically.".into()),
        Outcome::UpstreamAccepted { txid, .. } => (
            txid,
            "Submission accepted. Waiting for the transaction to confirm.".to_string(),
        ),
        Outcome::Uncertain { txid, .. } => (
            txid,
            "The submission's outcome is uncertain: the intent was recorded, but the submission response \
             did not arrive. It is being reconciled from the chain; it will not be retried automatically."
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
                    "Confirmed with {MIN_CONFIRMATIONS} or more confirmations. Continue in the paired Bitcoin Blake2b Cube."
                )
            }
            Assessment::Reorged => {
                "A reorganisation changed this transaction’s confirmation. Read again or review its new confirmation before continuing."
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
                            Some(Phase::Tracking) => "previously confirmed, checking current depth",
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
        .push_maybe(resubmission.map(|(snapshot, attempts)| {
            Column::new().spacing(10)
                .push(p1_regular("The original transaction was not found in the fresh Bitcoin checks. This resends the verified transaction shown below using its recorded inputs and fee. Older records may not identify the witness used in the first attempt. A previous attempt may still have reached the network; confirmation is not guaranteed."))
                .push(row("Transaction", p2_regular(snapshot.txid.to_string())))
            .push(row("Broadcast via", p2_regular(snapshot.route.label())))
                .push(row("Witness ID", p2_regular(snapshot.wtxid.to_string())))
                .push(row("Fee", p2_regular(format!("{} sats", snapshot.fee_sats))))
                .push(row("Previous attempts", p2_regular(attempts.to_string())))
                .push(button::primary(None, "Confirm resend of original transaction").on_press_maybe((!busy).then_some(Message::Claim(ClaimMessage::ConfirmResubmission))))
        }))
        .push_maybe((!has_resubmission && reconfirmation.is_none() && matches!(status, Some(Status::Observation(Assessment::Reorged | Assessment::WaitingForConfirmation)))).then(|| {
            button::secondary(None, "Review resend of original transaction").on_press_maybe((!busy).then_some(Message::Claim(ClaimMessage::ReviewResubmission)))
        }))
        .push_maybe(reconfirmation.map(|inclusion| {
            Column::new().spacing(10)
                .push(p1_regular("The same Bitcoin transaction has confirmed in a different block. Acknowledging this keeps its submission history and checks its confirmation depth again."))
                .push(row("Previous block", p2_regular(format!("{} — {}", inclusion.previous.height, inclusion.previous.hash))))
                .push(row("New block", p2_regular(format!("{} — {}", inclusion.confirmed.height, inclusion.confirmed.hash))))
                .push(button::primary(None, "Acknowledge new confirmation").on_press_maybe((!busy).then_some(Message::Claim(ClaimMessage::ConfirmReconfirmation))))
        }))
        .push_maybe(can_review_reconfirmation(status, has_resubmission, reconfirmation.is_some()).then(|| {
            button::secondary(None, "Review new confirmation").on_press_maybe((!busy).then_some(Message::Claim(ClaimMessage::ReviewReconfirmation)))
        }))
        .push(
            button::primary(None, "Continue in Bitcoin Blake2b")
                .on_press_maybe(can_continue.then_some(Message::ContinueForkClaim)),
        )
        .push(Space::new().height(Length::Fixed(10.0)))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_review_reads_the_actual_return_output() {
        use coincube_core::{
            chain::ChainId,
            miniscript::bitcoin::{
                absolute, hashes::Hash, transaction, Address, ScriptBuf, Transaction, TxOut,
                WScriptHash,
            },
        };
        let script = ScriptBuf::new_p2wsh(&WScriptHash::from_byte_array([1; 32]));
        let mut tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: Vec::new(),
            output: vec![TxOut {
                value: Amount::from_sat(12345),
                script_pubkey: script.clone(),
            }],
        };
        for chain in [ChainId::Bitcoin, ChainId::Testnet4] {
            assert_eq!(
                review_destination(&tx, chain),
                Ok((
                    Address::from_script(&script, chain.bitcoin_network())
                        .unwrap()
                        .to_string(),
                    Amount::from_sat(12345),
                ))
            );
        }
        // OP_RETURN and ancestry routes both show the same spendable output.
        tx.output.insert(
            0,
            TxOut {
                value: Amount::ZERO,
                script_pubkey: ScriptBuf::new_op_return([]),
            },
        );
        assert_eq!(
            review_destination(&tx, ChainId::Bitcoin)
                .unwrap()
                .1
                .to_sat(),
            12345
        );
        tx.output[1].value = Amount::from_sat(54321);
        assert_eq!(
            review_destination(&tx, ChainId::Bitcoin)
                .unwrap()
                .1
                .to_sat(),
            54321
        );
        tx.output.push(tx.output[1].clone());
        assert!(review_destination(&tx, ChainId::Bitcoin).is_err());
        tx.output.pop();
        tx.output[0].value = Amount::from_sat(1);
        assert!(review_destination(&tx, ChainId::Bitcoin).is_err());
        tx.output.remove(0);
        tx.output[0].script_pubkey = ScriptBuf::new();
        assert!(review_destination(&tx, ChainId::Bitcoin).is_err());
        tx.output.clear();
        assert!(review_destination(&tx, ChainId::Bitcoin).is_err());
    }

    /// Post-fork coins remain excluded from the shared-coin count; there is
    /// no wording that suggests OP_RETURN is a usable fallback.
    #[test]
    fn post_fork_coins_say_input_poison_is_not_available_while_closed() {
        let cache = Cache::default();
        let coins = |post_fork| CoinSet {
            pre_fork: Vec::new(),
            post_fork,
            ancestry_candidates: Vec::new(),
            tip_height: 0,
        };
        let closed = coins_detail(&coins(2), &cache);
        assert!(closed.contains("2 newer coins are left out"), "{}", closed);
        assert!(!closed.contains("OP_RETURN"), "{}", closed);
        assert!(!coins_detail(&coins(0), &cache).contains("Input poison"));
        assert!(!coins_detail(&coins(2), &cache).contains("Input poison"));
    }

    #[test]
    fn reconfirmation_review_never_competes_with_an_existing_recovery_review() {
        let reorged = Some(Status::Observation(Assessment::Reorged));

        assert!(can_review_reconfirmation(reorged, false, false));
        assert!(!can_review_reconfirmation(reorged, true, false));
        assert!(!can_review_reconfirmation(reorged, false, true));
        assert!(!can_review_reconfirmation(
            Some(Status::Observation(Assessment::WaitingForConfirmation)),
            false,
            false,
        ));
    }
}
