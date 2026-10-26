//! Claim step 1 views (Lane B1.5). Pure render functions over
//! [`ClaimStep1Panel`]'s stage; every button is an intent the panel
//! re-checks, never a permission.

use iced::{
    widget::{scrollable, Column, Container, Row, Space},
    Alignment, Length,
};

use coincube_core::{
    chain::ChainId,
    claim::{Assessment, MIN_CONFIRMATIONS},
    miniscript::bitcoin::{Address, Amount, Network, TxOut},
};
use coincube_ui::{
    component::{amount::*, button, card, text::*},
    icon, theme,
    widget::{ColumnExt, Element, RowExt},
};

use crate::{
    app::{
        cache::Cache,
        menu::Menu,
        state::vault::{
            claim::{ClaimStep1Panel, CoinSet, DepositStatus, Preconditions, Refusal, StageView},
            psbt::{PsbtModal, PsbtState},
        },
        view::{
            dashboard,
            message::*,
            vault::{
                ancestry_coins::{coin_input_row, CoinReview, VerifiedCoinPairReview},
                psbt,
            },
        },
    },
    services::{
        claim_coordinator::{Outcome, ReviewSnapshot},
        claim_workflow::{Phase, Status},
    },
};

const TITLE: &str = "Claim Bitcoin Blake2b";

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
            workflow_page(
                0,
                Column::new()
                    .spacing(12)
                    .push(
                        p1_regular("Loading your deposit addresses…").style(theme::text::secondary),
                    )
                    .push(
                        button::secondary_compact(None, "Pause")
                            .on_press(Message::Claim(ClaimMessage::Cancel)),
                    ),
            ),
        ),
        StageView::FundingDeposits(addresses) => {
            dashboard(menu, cache, funding_view(cache, panel, addresses))
        }
        StageView::PairedRecovery(recovery, checking, error) => dashboard(
            menu,
            cache,
            workflow_page(
                2,
                Column::new()
                    .spacing(20)
                    .push(p1_bold("Recorded two-chain claim"))
                    .push(p1_regular(
                        "Check the recorded transfers, then review the next available broadcast.",
                    ))
                    .push(p1_bold("Bitcoin transaction"))
                    .push_maybe(recovery.bitcoin_txid.map(|id| p2_regular(id.to_string())))
                    .push_maybe(recovery.bitcoin_output.as_ref().map(|output| {
                        p2_regular(format!(
                            "Recorded Bitcoin output: {}",
                            passive_amount(Amount::from_sat(output.amount_sats), cache)
                        ))
                    }))
                    .push_maybe(
                        recovery
                            .bitcoin_output
                            .as_ref()
                            .map(|output| p2_regular(output.destination.clone())),
                    )
                    .push(p1_regular(recovery.bitcoin_status.clone()))
                    .push(p1_bold("Bitcoin Blake2b transaction"))
                    .push_maybe(recovery.fork_txid.map(|id| p2_regular(id.to_string())))
                    .push_maybe(recovery.fork_output.as_ref().map(|output| {
                        p2_regular(format!(
                            "Recorded BTCB2 output: {}",
                            passive_amount(Amount::from_sat(output.amount_sats), cache)
                        ))
                    }))
                    .push_maybe(
                        recovery
                            .fork_output
                            .as_ref()
                            .map(|output| p2_regular(output.destination.clone())),
                    )
                    .push(p1_regular(recovery.fork_status.clone()))
                    .push_maybe(error.map(|message| card::warning(message.to_string())))
                    .push(
                        button::secondary(
                            None,
                            if checking {
                                "Checking both chains…"
                            } else {
                                "Check again"
                            },
                        )
                        .on_press_maybe(
                            (!checking).then_some(Message::Claim(ClaimMessage::Refresh)),
                        ),
                    )
                    .push_maybe(
                        (crate::services::claim_ancestry_gate::authorization().is_some()
                            && recovery.bitcoin_unattempted)
                            .then(|| {
                                button::primary(None, "Review Bitcoin broadcast").on_press_maybe(
                                    (!checking).then_some(Message::Claim(
                                        ClaimMessage::ReviewPairedBitcoin,
                                    )),
                                )
                            }),
                    )
                    .push_maybe(
                        (crate::services::claim_ancestry_gate::authorization().is_some()
                            && recovery.fork_ready_for_review)
                            .then(|| {
                                button::primary(None, "Review BTCB2 broadcast").on_press_maybe(
                                    (!checking)
                                        .then_some(Message::Claim(ClaimMessage::ReviewPairedFork)),
                                )
                            }),
                    )
                    .push(
                        button::secondary(None, "Close")
                            .on_press(Message::Claim(ClaimMessage::Cancel)),
                    ),
            ),
        ),
        StageView::PairedBitcoinReview(review) => dashboard(menu, cache, {
            let outputs =
                crate::app::state::vault::paired_summary::PairedOutputSummary::from_signed_pair(
                    &review.bitcoin,
                    &review.fork,
                );
            workflow_page(2, Column::new()
                .spacing(20)
                .push(p1_bold("Review Bitcoin broadcast"))
                .push(p1_regular("Review the Bitcoin return address and fee. BTCB2 is broadcast separately after six Bitcoin confirmations."))
                .push(p1_bold("Bitcoin — broadcast now"))
                .push(p2_regular(format!("Txid: {}", review.bitcoin.compute_txid())))
                .push(p2_regular(format!("Fee: {} sats", review.bitcoin_fee_sats)))
                .push_maybe(outputs.as_ref().map(|(output, _)| {
                    p2_regular(format!("{} sats to {}", output.amount_sats, output.destination))
                }))
                .push(p1_bold("Bitcoin Blake2b — recorded, not sent now"))
                .push(p2_regular(format!("Txid: {}", review.fork.compute_txid())))
                .push(p2_regular(format!("Fee: {} sats", review.fork_fee_sats)))
                .push_maybe(outputs.as_ref().map(|(_, output)| {
                    p2_regular(format!("{} sats to {}", output.amount_sats, output.destination))
                }))
                .push_maybe(outputs.is_none().then(|| card::warning("Cannot display both signed destinations. Review again before broadcasting.".to_string())))
                .push(button::secondary(None, "Back").on_press(Message::Claim(ClaimMessage::BackPairedBitcoin)))
                .push_maybe(outputs.is_some().then(|| button::primary(None, "Broadcast Transaction").on_press(Message::Claim(ClaimMessage::ConfirmPairedBitcoin))))
            )
        }),
        StageView::PairedBitcoinSubmitting => dashboard(
            menu,
            cache,
            workflow_page(
                2,
                Column::new()
                    .spacing(20)
                    .push(p1_bold("Checking Bitcoin broadcast"))
                    .push(p1_regular(
                        "Rechecking both chains before broadcasting Bitcoin…",
                    )),
            ),
        ),
        StageView::PairedForkReview(review) => dashboard(menu, cache, {
            let outputs =
                crate::app::state::vault::paired_summary::PairedOutputSummary::from_signed_pair(
                    &review.bitcoin,
                    &review.fork,
                );
            workflow_page(2, Column::new()
                .spacing(20)
                .push(p1_bold("Review Bitcoin Blake2b broadcast"))
                .push(p1_regular("Bitcoin has six confirmations. Review the BTCB2 return address and fee."))
                .push(p1_bold("Bitcoin Blake2b — broadcast now"))
                .push(p2_regular(format!("Txid: {}", review.fork.compute_txid())))
                .push(p2_regular(format!("Fee: {} sats", review.fork_fee_sats)))
                .push_maybe(outputs.as_ref().map(|(_, output)| {
                    p2_regular(format!("{} sats to {}", output.amount_sats, output.destination))
                }))
                .push(p1_bold("Bitcoin — recorded and confirmed"))
                .push(p2_regular(format!("Txid: {}", review.bitcoin.compute_txid())))
                .push_maybe(outputs.as_ref().map(|(output, _)| {
                    p2_regular(format!("{} sats to {}", output.amount_sats, output.destination))
                }))
                .push_maybe(outputs.is_none().then(|| card::warning("Cannot display both signed destinations. Review again before broadcasting.".to_string())))
                .push(button::secondary(None, "Back").on_press(Message::Claim(ClaimMessage::BackPairedFork)))
                .push_maybe(outputs.is_some().then(|| button::primary(None, "Broadcast Transaction").on_press(Message::Claim(ClaimMessage::ConfirmPairedFork))))
            )
        }),
        StageView::PairedForkSubmitting => dashboard(
            menu,
            cache,
            workflow_page(
                2,
                Column::new()
                    .spacing(20)
                    .push(p1_bold("Checking Bitcoin Blake2b broadcast"))
                    .push(p1_regular(
                        "Rechecking both chains before broadcasting BTCB2…",
                    )),
            ),
        ),
        StageView::PairedSign {
            bitcoin,
            fork,
            session,
            active,
            recording,
            error,
        } => paired_sign_view(
            menu, cache, bitcoin, fork, session, active, recording, error,
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

/// One persistent page. Each stage keeps its place as its body changes.
fn workflow_parts<'a>(
    active: usize,
    bodies: [Option<Element<'a, Message>>; 3],
) -> Element<'a, Message> {
    let labels = ["Deposits", "Review and sign", "Broadcast and confirm"];
    let summaries = [
        "Deposits selected in your paired Vaults.",
        if active > 1 {
            "Both transfers signed and saved."
        } else {
            "Available after both deposits are verified."
        },
        "Broadcast Bitcoin, then BTCB2 after six Bitcoin confirmations.",
    ];
    let mut steps = Row::new().spacing(10).width(Length::Fill);
    for (index, label) in labels.iter().enumerate() {
        let current = index == active;
        let number = format!("{}", index + 1);
        let marker: Element<'a, Message> = if index < active {
            icon::check_icon().style(theme::text::success).into()
        } else if current {
            p1_bold(number).into()
        } else {
            p1_regular(number).style(theme::text::secondary).into()
        };
        let title: Element<'a, Message> = if current {
            p1_bold(*label).into()
        } else {
            p1_regular(*label).style(theme::text::secondary).into()
        };
        let step = Row::new()
            .spacing(8)
            .align_y(Alignment::Center)
            .push(marker)
            .push(title);
        steps = steps.push(
            Container::new(step)
                .padding(12)
                .width(Length::FillPortion(1))
                .style(theme::card::simple),
        );
    }
    let mut page = Column::new()
        .spacing(22)
        .max_width(1100)
        .push(header(
            "Separate Bitcoin and BTCB2 with transfers back to your own Vaults.",
        ))
        .push(steps);
    for (index, body) in IntoIterator::into_iter(bodies).enumerate() {
        let mut section =
            Column::new()
                .spacing(12)
                .push(p1_bold(format!("{}. {}", index + 1, labels[index])));
        if let Some(body) = body {
            section = section.push(body);
        } else {
            section = section.push(p1_regular(summaries[index]).style(theme::text::secondary));
        }
        page = page.push(section);
    }
    page.into()
}

fn workflow_page<'a>(active: usize, body: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    let mut bodies = [None, None, None];
    bodies[active] = Some(body.into());
    workflow_parts(active, bodies)
}

/// Render deposits and the paired review workflow. Only the panel's matching
/// paired review selects completed-verification rows and the signing review;
/// search diagnostics alone never do. Controls emit intents the panel rechecks.
fn funding_view<'a>(
    cache: &Cache,
    panel: &'a ClaimStep1Panel,
    addresses: &'a crate::services::claim_workflow::CheckedClaimFundingAddresses,
) -> Element<'a, Message> {
    let verified = panel.funding_review().is_some();
    let signing_available = crate::services::claim_ancestry_gate::authorization().is_some();
    let expanded = panel.deposit_details_expanded();
    let details = Column::new().spacing(12)
        .push(p1_regular("Send BTC to the Bitcoin address and BTCB2 to the Blake2b address. Keep both Cubes open under the same Connect account."))
        .push(p1_bold(format!("Bitcoin · minimum {} sats", format_u64_as_string(addresses.bitcoin_minimum_sats, ","))))
        .push(address_row(addresses.bitcoin_deposit.clone(), "Copied Bitcoin deposit address to clipboard"))
        .push(p1_bold(format!("Bitcoin Blake2b · minimum {} sats", format_u64_as_string(addresses.btcb2_minimum_sats, ","))))
        .push(address_row(addresses.btcb2_deposit.clone(), "Copied BTCB2 deposit address to clipboard"))
        .push(p1_regular(format!("Minimums cover estimated fees at {} sat/vB on Bitcoin and {} sat/vB on BTCB2. Fees may change. Each deposit must be confirmed and verified as chain-specific.", addresses.bitcoin_rate_sat_vb, addresses.btcb2_rate_sat_vb)).style(theme::text::secondary));
    let controls = Row::new()
        .spacing(10)
        .align_y(Alignment::Center)
        .push_maybe(
            ((panel.funding_error().is_some() || panel.funding_proof_needs_retry())
                && !panel.funding_checking())
            .then(|| {
                button::secondary_compact(Some(icon::reload_icon()), "Retry verification")
                    .on_press(Message::Claim(ClaimMessage::CheckFunding))
            }),
        )
        .push_maybe(panel.verification_report().map(|_| {
            button::secondary_compact(None, "Copy verification report")
                .on_press(Message::Claim(ClaimMessage::CopyVerificationReport))
        }))
        .push_maybe((!verified).then(|| {
            button::secondary_compact(None, "Pause checks")
                .on_press(Message::Claim(ClaimMessage::Cancel))
        }));
    let deposits = Column::new().spacing(12)
        .push(p1_bold("Claim checks"))
        .push(deposit_status_view(panel, addresses.bitcoin_minimum_sats, addresses.btcb2_minimum_sats, cache.balance_amounts_masked()))
        .push(button::secondary_compact(Some(if expanded { icon::collapsed_icon() } else { icon::collapse_icon() }),
            if expanded { "Hide deposit addresses" } else { "Deposit addresses" })
            .on_press(Message::Claim(ClaimMessage::ToggleDepositDetails)))
        .push_maybe(expanded.then(|| Container::new(details).padding(18).style(theme::card::simple).width(Length::Fill)))
        .push_maybe(panel.funding_error().filter(|_| !panel.funding_inline_error()).map(|error| card::warning(error.to_string())))
        .push_maybe((!panel.paired_vault_ready()).then(|| card::warning(
            "Open and unlock the paired BTCB2 Cube under the same Connect account. Verification continues here automatically.".into())))
        .push_maybe(panel.funding_checking().then(|| p1_regular(panel.funding_status().unwrap_or("Verifying deposits…")).style(theme::text::secondary)))
        .push(controls);
    let review = verified.then(|| {
        let mut body = Column::new().spacing(12)
            .push(p1_regular("Review the coins, return addresses and fees. Both transfers return funds to your Vaults, minus network fees."));
        if let (Some(review), Some(descriptor)) = (panel.funding_review(), panel.wallet().inheritance_descriptor()) {
            let (threshold, _) = descriptor.policy().primary_path().thresh_origins();
            // This screen immediately offers Review and sign. Show exact
            // inputs, destinations and fees for transaction authorization.
            body = body.push(funding_review_view(review, threshold));
        }
        body
            .push_maybe((!signing_available).then(|| card::warning(
                "Split signing is disabled for this launch. Restart Tenshu with --enable-btcb2-split to enable live testing.".into())))
            .push(button::primary(None, if panel.paired_signing_pending() { "Checking transactions…" } else { "Review and sign" })
                .on_press_maybe((signing_available && !panel.paired_signing_pending() && !panel.funding_checking()).then_some(Message::Claim(ClaimMessage::SignPair)))
                .width(Length::Fixed(240.0)))
            .into()
    });
    workflow_parts(usize::from(verified), [Some(deposits.into()), review, None])
}

fn funding_review_view(
    review: &VerifiedCoinPairReview,
    threshold: usize,
) -> Element<'static, Message> {
    let address = |output: &TxOut| {
        Address::from_script(&output.script_pubkey, Network::Bitcoin)
            .map(|address| address.to_string())
            .unwrap_or_else(|_| format!("script {}", hex::encode(output.script_pubkey.as_bytes())))
    };
    let mut rows = Column::new()
        .spacing(12)
        .push(p1_bold("Transaction details"))
        .push(
            Row::new()
                .spacing(20)
                .push(
                    p1_regular(format!(
                        "{} shared pre-fork coins · present on both chains",
                        review.bitcoin.shared.len(),
                    ))
                    .width(Length::Fill),
                )
                .push(p1_bold(format!(
                    "{} sats",
                    review.bitcoin.shared_total.to_sat()
                ))),
        )
        .push(p2_regular(format!(
            "Signing threshold: {threshold} signature{} per input",
            if threshold == 1 { "" } else { "s" }
        )));
    for coin in &review.bitcoin.shared {
        rows = rows.push(coin_input_row(coin, "Before fork", None, false));
    }
    rows.push(p1_bold("Bitcoin transaction"))
        .push(coin_input_row(
            &review.bitcoin.funding,
            "Bitcoin-only deposit — replay protected",
            Some(review.bitcoin_confirmations),
            false,
        ))
        .push(p2_regular(format!(
            "Back to this Vault: {} · {} sats",
            address(&review.bitcoin.destination),
            review.bitcoin.destination.value.to_sat()
        )))
        .push(p2_regular(format!("Fee: {} sats", review.bitcoin.fee.to_sat())))
        .push(p1_bold("Bitcoin Blake2b transaction"))
        .push(coin_input_row(
            &review.fork.funding,
            "BTCB2-only deposit — replay protected",
            Some(review.fork_confirmations),
            false,
        ))
        .push(p2_regular(format!(
            "Back to the paired BTCB2 Vault: {} · {} sats",
            address(&review.fork.destination),
            review.fork.destination.value.to_sat()
        )))
        .push(p2_regular(format!("Fee: {} sats", review.fork.fee.to_sat())))
        .push(p1_regular("All shared coins and the verified deposit are included. Both transactions are rechecked before signing.").style(theme::text::secondary))
        .into()
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

fn deposit_heading(status: Option<&DepositStatus>, verified: bool) -> &'static str {
    if verified {
        return "Both deposits verified for replay protection";
    }
    let Some(status) = status else {
        return "Checking for funds";
    };
    let results = [&status.bitcoin, &status.btcb2];
    let received = results
        .iter()
        .filter(|result| {
            result
                .as_ref()
                .is_ok_and(|funds| funds.confirmed_sats > 0 || funds.pending_sats > 0)
        })
        .count();
    let failed = results.iter().filter(|result| result.is_err()).count();
    if failed > 0 {
        return if received > 0 {
            "Funds received at one address; the other could not be checked"
        } else if failed == 1 {
            "One deposit address could not be checked; no funds found at the other"
        } else {
            "Unable to check both deposit addresses"
        };
    }
    match received {
        2 => "Funds received at both addresses",
        1 => "Funds received at one of two addresses",
        _ => "Waiting for funds",
    }
}

/// Present address arrival observations independently from proof diagnostics.
/// A current matching paired review overrides diagnostic rows with the exact
/// replay-protected coin rows, even if the retained search hint is PathFound.
fn deposit_status_view<'a>(
    panel: &ClaimStep1Panel,
    bitcoin_minimum: u64,
    btcb2_minimum: u64,
    balance_masked: bool,
) -> Element<'a, Message> {
    let status = panel.deposit_status();
    let review = panel.funding_review();
    Container::new(
        Column::new()
            .spacing(12)
            .push(p1_bold(deposit_heading(status, review.is_some())))
            .push(match review {
                Some(review) => coin_input_row(
                    &review.bitcoin.funding,
                    "Bitcoin-only deposit — replay protected",
                    Some(review.bitcoin_confirmations),
                    balance_masked,
                ),
                None => deposit_row(
                    "Bitcoin",
                    status.map(|status| &status.bitcoin),
                    bitcoin_minimum,
                    panel.funding_proof_status(ChainId::Bitcoin),
                    panel.funding_checking(),
                    balance_masked,
                ),
            })
            .push(match review {
                Some(review) => coin_input_row(
                    &review.fork.funding,
                    "BTCB2-only deposit — replay protected",
                    Some(review.fork_confirmations),
                    balance_masked,
                ),
                None => deposit_row(
                    "Bitcoin Blake2b",
                    status.map(|status| &status.btcb2),
                    btcb2_minimum,
                    panel.funding_proof_status(ChainId::BitcoinBlake2b),
                    panel.funding_checking(),
                    balance_masked,
                ),
            })
            .push_maybe(
                (panel.deposit_polling() && status.is_some())
                    .then(|| p2_regular("Refreshing deposit status…")),
            ),
    )
    .padding(20)
    .style(theme::card::simple)
    .width(Length::Fill)
    .into()
}

/// Render one chain's address observation and its separate proof diagnostic.
/// Amounts and confirmations do not establish replay protection. The caller
/// renders completed paired verification through verified coin rows instead.
fn deposit_row<'a>(
    label: &'static str,
    observed: Option<
        &Result<
            crate::services::foreign_scan::AddressFunds,
            crate::services::foreign_scan::ScanError,
        >,
    >,
    minimum: u64,
    proof: crate::services::claim_observation::http::FundingProofStatus,
    checking: bool,
    balance_masked: bool,
) -> Element<'a, Message> {
    let display_sats = |sats: u64| {
        if balance_masked {
            "********".to_string()
        } else {
            format_u64_as_string(sats, ",")
        }
    };
    let (mark, detail) = match observed {
        None => (None, "Checking the reserved address…".to_string()),
        Some(Err(crate::services::foreign_scan::ScanError::RateLimited(delay))) => (
            Some(false),
            format!(
                "Indexer rate limited; retrying after its cooldown (about {} minutes).",
                delay.as_secs().div_ceil(60)
            ),
        ),
        Some(Err(_)) => (
            Some(false),
            "Indexer status unavailable; retrying automatically.".to_string(),
        ),
        Some(Ok(funds)) if funds.largest_mature_sats >= minimum => (
            None,
            format!(
                "{} sats · {MIN_CONFIRMATIONS}+ confirmations",
                display_sats(funds.mature_sats)
            ),
        ),
        Some(Ok(funds)) if funds.largest_confirmed_sats >= minimum => (
            None,
            format!(
                "{} sats · {} of {MIN_CONFIRMATIONS} confirmations",
                display_sats(funds.confirmed_sats),
                funds.min_confirmations
            ),
        ),
        Some(Ok(funds)) if funds.pending_sats > 0 => (
            None,
            format!(
                "{} sats · awaiting confirmation",
                display_sats(funds.pending_sats)
            ),
        ),
        Some(Ok(funds)) if funds.confirmed_sats > 0 => (
            Some(false),
            format!(
                "{} sats confirmed, but no single deposit reaches the {minimum}-sat minimum.",
                display_sats(funds.confirmed_sats)
            ),
        ),
        Some(Ok(_)) => (None, "Waiting for a deposit at this address.".to_string()),
    };
    let proof_detail = deposit_proof_detail(proof, checking);
    let proof_failed =
        matches!(proof,
        crate::services::claim_observation::http::FundingProofStatus::Failed(_)
        | crate::services::claim_observation::http::FundingProofStatus::NoQualifyingAncestry);
    let mark = if mark == Some(false) || proof_failed {
        icon::warning_icon().style(theme::text::warning)
    } else {
        icon::receive_icon().style(theme::text::secondary)
    };
    Row::new()
        .spacing(10)
        .align_y(Alignment::Start)
        .push(mark)
        .push(p1_regular(label).width(Length::Fixed(170.0)))
        .push(
            Column::new()
                .spacing(4)
                .push(p1_regular(detail).style(theme::text::secondary))
                .push(p2_regular(proof_detail).style(if proof_failed {
                    theme::text::warning
                } else {
                    theme::text::secondary
                }))
                .width(Length::Fill),
        )
        .into()
}

/// Format a typed search or validation diagnostic without promoting it to a
/// completed paired proof. PathFound describes the ancestry subcheck; accepted
/// paired reviews bypass this helper. A bounded search finding no qualifying
/// path, or unavailable evidence, is not by itself advice to send more funds.
pub(crate) fn deposit_proof_detail(
    proof: crate::services::claim_observation::http::FundingProofStatus,
    checking: bool,
) -> &'static str {
    use crate::services::claim_observation::{
        http::{DiscoveryError, FundingProofStatus},
        FailureKind,
    };
    use coincube_core::claim_ancestry::Error;
    match proof {
        FundingProofStatus::Pending if checking => "Verifying replay protection…",
        FundingProofStatus::Pending => "Replay protection not verified yet.",
        FundingProofStatus::PathFound => "Post-fork ancestry found; paired verification incomplete.",
        FundingProofStatus::NoQualifyingAncestry => "No eligible deposit traces to a post-fork mining reward unique to this chain.",
        FundingProofStatus::Failed(DiscoveryError::Structural(Error::PathLimit | Error::ByteLimit)) =>
            "Search limit reached before this deposit’s ancestry could be verified.",
        FundingProofStatus::Failed(DiscoveryError::Structural(_)) =>
            "Could not verify a link to a post-fork mining reward. Retry verification.",
        FundingProofStatus::Failed(DiscoveryError::Observation(error)) => match error {
            FailureKind::AncestryRootShared { .. } => "Ancestry is shared by both chains; this deposit cannot establish chain-specific replay protection.",
            FailureKind::AncestryRootChanged { .. } | FailureKind::Changed => "The deposit or chain changed during verification. Wait for confirmations and retry verification.",
            FailureKind::Http(429) => "Indexer rate limited. Retry verification after the cooldown.",
            FailureKind::Unavailable | FailureKind::Http(_) | FailureKind::Stale | FailureKind::FreshnessUnverified => "Fresh indexer data is unavailable. Retry verification when the indexer recovers.",
            FailureKind::Deadline => "Ancestry verification timed out. This deposit remains unverified.",
            FailureKind::CollectionLimit => "Verification reached its data limit. This deposit remains unverified.",
            FailureKind::Cancelled => "The Connect session changed. Reopen this Cube under the same account and retry verification.",
            _ => "Could not verify this deposit’s replay protection. Retry verification.",
        },
    }
}

fn check_row<'a>(label: &'static str, ok: Option<bool>, detail: String) -> Element<'a, Message> {
    let status: Element<'a, Message> = match ok {
        Some(true) => p2_regular("Ready")
            .style(theme::text::success)
            .width(Length::Fixed(110.0))
            .into(),
        Some(false) => p2_regular("Needs attention")
            .style(theme::text::warning)
            .width(Length::Fixed(110.0))
            .into(),
        None => p2_regular("Checking")
            .style(theme::text::secondary)
            .width(Length::Fixed(110.0))
            .into(),
    };
    let mark = match ok {
        Some(true) => icon::check_icon().style(theme::text::success),
        Some(false) => icon::warning_icon().style(theme::text::warning),
        None => icon::reload_icon().style(theme::text::secondary),
    };
    Row::new()
        .spacing(10)
        .align_y(Alignment::Center)
        .push(mark)
        .push(p1_regular(label).width(Length::Fixed(145.0)))
        .push(
            p1_regular(detail)
                .style(theme::text::secondary)
                .width(Length::Fill),
        )
        .push(status)
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
                Some(Ok(())) => "Bitcoin Vault identified; deposit checks available.".to_string(),
                Some(Err(_)) => "Not usable for a claim.".to_string(),
            },
        ))
        .push(check_row(
            "Coins to split",
            coins.map(|c| !c.pre_fork.is_empty()),
            match (checked.map(|c| &c.coins), coins) {
                (None, _) => "Reading this Vault's coins…".to_string(),
                (Some(Err(_)), _) if checked.is_some_and(|c| c.window.is_err()) => {
                    "Waiting for Bitcoin Blake2b's fork status to classify coins.".to_string()
                }
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

    if !panel.can_sign_or_submit() {
        let problem = panel
            .funding_setup_refusal()
            .map(|refusal| refusal.reason)
            .or_else(|| panel.funding_error().map(str::to_owned));
        let summary = coins.map(|coins| {
            let total: Amount = coins.pre_fork.iter().map(|coin| coin.amount).sum();
            format!(
                "{} shared coin{} · {}",
                coins.pre_fork.len(),
                if coins.pre_fork.len() == 1 { "" } else { "s" },
                passive_amount(total, cache)
            )
        });
        return workflow_page(
            0,
            Column::new()
                .spacing(12)
                .push(p1_bold("Before you start"))
                .push_maybe(summary.map(p1_regular))
                .push_maybe(problem.as_ref().map(|reason| card::warning(reason.clone())))
                .push_maybe((problem.is_none()).then(|| {
                    p1_regular(if pre.checking {
                        "Checking this Vault…"
                    } else {
                        "Load your deposit addresses to receive funds on both networks."
                    })
                    .style(theme::text::secondary)
                }))
                .push({
                    button::secondary_compact(
                        Some(icon::reload_icon()),
                        if panel.can_prepare_funding() {
                            if panel.funding_error().is_some() {
                                "Retry loading deposits"
                            } else {
                                "Load deposits"
                            }
                        } else {
                            "Check again"
                        },
                    )
                    .on_press_maybe((!pre.checking).then_some(Message::Claim(
                        if panel.can_prepare_funding() {
                            ClaimMessage::StartFunding
                        } else {
                            ClaimMessage::Recheck
                        },
                    )))
                })
                .push(
                    button::secondary_compact(
                        Some(if panel.setup_details_expanded() {
                            icon::collapsed_icon()
                        } else {
                            icon::collapse_icon()
                        }),
                        "Wallet details",
                    )
                    .on_press(Message::Claim(ClaimMessage::ToggleSetupDetails)),
                )
                .push_maybe(
                    panel.setup_details_expanded().then_some(
                        Container::new(checklist)
                            .padding(20)
                            .style(theme::card::simple)
                            .width(Length::Fill),
                    ),
                ),
        );
    }

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
        .push(header("Prepare or resume your split"))
        .push(p1_regular("Your paired Blake2b Cube is set up, but creating the Cube alone did not split or claim coins."))
        .push(p1_regular(
            "Deposit Bitcoin into this Vault and BTCB2 into its paired Vault. Tenshu checks both chains here, then combines each chain-specific deposit with the shared coins in a transfer back to your own Vault. You will review and sign both transfers before broadcasting.",
        ))
        .push(
            Container::new(checklist)
                .padding(20)
                .style(theme::card::simple)
                .width(Length::Fill),
        )
        .push(action)
        .push(p1_bold("2. Review and sign"))
        .push(p2_regular("Once both deposits are verified, review the coins, return addresses and fees, then sign the two self-transfers here."))

        .into()
}

fn coins_detail(coins: &CoinSet, cache: &Cache) -> String {
    let total: Amount = coins.pre_fork.iter().map(|c| c.amount).sum();
    let mut detail = format!(
        "{} coin{} from before the fork, {}.",
        coins.pre_fork.len(),
        if coins.pre_fork.len() == 1 { "" } else { "s" },
        passive_amount(total, cache),
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

/// Wallet inventory shown before transaction composition obeys the shared
/// device privacy receipt. Transaction output and fee review uses format_amount.
fn passive_amount(amount: Amount, cache: &Cache) -> String {
    if cache.balance_amounts_masked() {
        "********".to_string()
    } else {
        format_amount(amount, cache)
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
            .push_maybe(state.desc_policy.as_ref().map(|policy| {
                psbt::spend_overview_with_cancel(
                    &state.tx,
                    policy,
                    &state.wallet.keys_aliases,
                    currently_signing,
                    state.saved,
                    None,
                    state.broadcast_ready(cache),
                    (!finalizing).then_some(Message::Claim(ClaimMessage::Cancel)),
                )
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

#[allow(clippy::too_many_arguments)]
fn paired_sign_view<'a>(
    menu: &'a Menu,
    cache: &'a Cache,
    bitcoin: &'a PsbtState,
    fork: &'a PsbtState,
    session: u64,
    active: coincube_core::chain::ChainId,
    recording: bool,
    error: Option<&'a str>,
) -> Element<'a, Message> {
    let shared_count = bitcoin
        .tx
        .psbt
        .unsigned_tx
        .input
        .len()
        .checked_sub(1)
        .filter(|count| *count > 0 && fork.tx.psbt.unsigned_tx.input.len() == count + 1);
    let state = if active == coincube_core::chain::ChainId::Bitcoin {
        bitcoin
    } else {
        fork
    };
    let currently_signing = matches!(&state.modal, Some(PsbtModal::Sign(m)) if m.is_signing());
    let content = dashboard(
        menu,
        cache,
        workflow_page(1, Column::new()
            .spacing(16)
            .push_maybe(recording.then(|| p1_regular("Checking both chains and recording the signed pair…")))
            .push(p1_regular("Sign both transfers with this Vault’s normal keys. You will review each broadcast separately."))
            .push_maybe(shared_count.map(|count| p1_regular(format!("Each transfer includes {count} shared coin{} and one verified chain-specific deposit.", if count == 1 { "" } else { "s" })).style(theme::text::secondary)))
            .push(Row::new().spacing(10)
                .push(button::secondary(None, "Bitcoin transaction").on_press(Message::Claim(ClaimMessage::ShowPairBitcoin)))
                .push(button::secondary(None, "Bitcoin Blake2b transaction").on_press(Message::Claim(ClaimMessage::ShowPairBtcb2))))
            .push(p1_bold(if active == coincube_core::chain::ChainId::Bitcoin {
                "Bitcoin — return to this Vault"
            } else {
                "Bitcoin Blake2b — return to the paired Vault"
            }))
            .push_maybe(error.map(|reason| card::warning(reason.to_string())))
            .push_maybe(state.desc_policy.as_ref().map(|policy| psbt::spend_overview_with_cancel(
                &state.tx,
                policy,
                &state.wallet.keys_aliases,
                currently_signing,
                state.saved,
                None,
                false,
                Some(Message::Claim(ClaimMessage::Cancel)),
            )))),
    );
    let rendered = match &state.modal {
        Some(modal) => {
            let modal: &dyn crate::app::state::vault::psbt::Modal = modal.as_ref();
            modal.view(content)
        }
        None => content,
    };
    rendered.map(move |message| match message {
        Message::Claim(_)
        | Message::ShowError(_)
        | Message::ShowToast(_, _)
        | Message::Clipboard(_)
        | Message::CopyWithToast(_, _)
        | Message::OpenUrl(_) => message,
        other => Message::ClaimPairedSigner(session, active, Box::new(other)),
    })
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
    use crate::app::state::preferences::{BalancePrivacy, BalancePrivacyChrome};
    use crate::app::view::shared::landing::capture;
    use crate::app::view::vault::ancestry_coins::{CoinPairReview, CoinRow};

    #[test]
    fn mounted_claim_checks_and_deposit_states_fit_both_viewports() {
        use crate::services::{
            claim_observation::http::FundingProofStatus,
            foreign_scan::{AddressFunds, ScanError},
        };
        use std::time::Duration;

        for (width, height) in [(1440, 900), (1024, 768)] {
            for (appearance, palette) in [
                ("dark", theme::Theme::dark()),
                ("light", theme::Theme::light()),
            ] {
                let checks = Column::new()
                    .spacing(10)
                    .push(check_row(
                        "Claim target",
                        Some(true),
                        "Paired Cube found.".into(),
                    ))
                    .push(check_row(
                        "Vault",
                        Some(true),
                        "Native SegWit Vault.".into(),
                    ))
                    .push(check_row(
                        "Node backend",
                        None,
                        "Checking both chains…".into(),
                    ))
                    .push(check_row(
                        "Coins to split",
                        Some(false),
                        "Fork status unavailable.".into(),
                    ))
                    .push(check_row(
                        "Fee rate",
                        Some(true),
                        "4 sat/vB (about an hour).".into(),
                    ));
                let mounted = capture::inspect(
                    "claim-five-checks",
                    workflow_page(0, checks),
                    (width, height),
                    appearance,
                    palette,
                );
                for label in [
                    "Deposits",
                    "Review and sign",
                    "Broadcast and confirm",
                    "Claim target",
                    "Vault",
                    "Node backend",
                    "Coins to split",
                    "Fee rate",
                    "Needs attention",
                ] {
                    assert!(
                        mounted.contains(label),
                        "missing {} at {} {}",
                        label,
                        width,
                        appearance
                    );
                }

                let waiting = Ok(AddressFunds {
                    pending_sats: 75_000,
                    ..AddressFunds::default()
                });
                let limited = Err(ScanError::RateLimited(Duration::from_secs(120)));
                let deposits = Column::new()
                    .spacing(12)
                    .push(p1_bold("Claim checks"))
                    .push(deposit_row(
                        "Bitcoin",
                        Some(&waiting),
                        100_000,
                        FundingProofStatus::Pending,
                        false,
                        true,
                    ))
                    .push(deposit_row(
                        "Bitcoin Blake2b",
                        Some(&limited),
                        100_000,
                        FundingProofStatus::Pending,
                        false,
                        true,
                    ));
                let mounted = capture::inspect(
                    "claim-deposit-waiting-and-rate-limited",
                    workflow_page(0, deposits),
                    (width, height),
                    appearance,
                    palette,
                );
                assert!(mounted.contains("Bitcoin Blake2b"));
                assert!(mounted.contains("awaiting confirmation"));
                assert!(mounted.contains("Indexer rate limited"));
                assert!(mounted.contains("Replay protection not verified yet"));
                assert!(!mounted.contains("75,000"));
            }
        }
    }

    #[test]
    fn active_funding_review_shows_exact_inputs_while_passive_deposit_stays_hidden() {
        use crate::services::{
            claim_observation::http::FundingProofStatus, foreign_scan::AddressFunds,
        };
        use coincube_core::miniscript::bitcoin::{OutPoint, ScriptBuf, Txid};
        use std::str::FromStr;

        let row = |id: u8, sats| {
            let output = TxOut {
                value: Amount::from_sat(sats),
                script_pubkey: ScriptBuf::new(),
            };
            CoinRow {
                outpoint: OutPoint::new(Txid::from_str(&format!("{id:064x}")).unwrap(), 0),
                amount: output.value,
                output,
            }
        };
        let shared = row(1, 123_456_789);
        let bitcoin_funding = row(2, 22_222_222);
        let fork_funding = row(3, 33_333_333);
        let review = VerifiedCoinPairReview {
            coins: CoinPairReview {
                bitcoin: CoinReview {
                    shared: vec![shared.clone()],
                    funding: bitcoin_funding,
                    shared_total: shared.amount,
                    input_total: Amount::from_sat(145_679_011),
                    fee: Amount::from_sat(1_000),
                    destination: TxOut {
                        value: Amount::from_sat(145_678_011),
                        script_pubkey: ScriptBuf::new(),
                    },
                },
                fork: CoinReview {
                    shared: vec![shared],
                    funding: fork_funding,
                    shared_total: Amount::from_sat(123_456_789),
                    input_total: Amount::from_sat(156_790_122),
                    fee: Amount::from_sat(2_000),
                    destination: TxOut {
                        value: Amount::from_sat(156_788_122),
                        script_pubkey: ScriptBuf::new(),
                    },
                },
            },
            bitcoin_confirmations: 6,
            fork_confirmations: 6,
        };
        let active = capture::inspect(
            "claim-active-funding-review",
            funding_review_view(&review, 2),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        for sats in ["123456789 sats", "22222222 sats", "33333333 sats"] {
            assert!(
                active.contains(sats),
                "missing authorization amount {}",
                sats
            );
        }
        assert!(!active.contains("********"));

        let passive = capture::inspect(
            "claim-passive-deposit",
            deposit_row(
                "Bitcoin",
                Some(&Ok(AddressFunds {
                    confirmed_sats: 987_654_321,
                    largest_confirmed_sats: 987_654_321,
                    min_confirmations: 3,
                    ..AddressFunds::default()
                })),
                100_000,
                FundingProofStatus::Pending,
                false,
                true,
            ),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        assert!(passive.contains("********"));
        assert!(!passive.contains("987,654,321"));
    }

    #[test]
    fn mounted_deposit_observation_masks_only_owned_amounts() {
        use crate::services::{
            claim_observation::http::FundingProofStatus, foreign_scan::AddressFunds,
        };
        let funds = Ok(AddressFunds {
            confirmed_sats: 987_654_321,
            largest_confirmed_sats: 987_654_321,
            min_confirmations: 3,
            ..AddressFunds::default()
        });
        let mut cache = Cache::default();
        for (privacy, visible) in [
            (BalancePrivacy::Unresolved, false),
            (BalancePrivacy::Hidden, false),
            (BalancePrivacy::Shown, true),
        ] {
            cache.balance_privacy = Some(BalancePrivacyChrome {
                privacy,
                busy: false,
                interactive: true,
            });
            let mounted = capture::inspect(
                "claim-deposit-observation",
                deposit_row(
                    "Bitcoin",
                    Some(&funds),
                    100_000,
                    FundingProofStatus::Pending,
                    false,
                    cache.balance_amounts_masked(),
                ),
                (1024, 768),
                "dark",
                coincube_ui::theme::Theme::dark(),
            );
            assert!(mounted.contains("3 of 6 confirmations"));
            assert!(mounted.contains("Replay protection not verified yet"));
            assert_eq!(mounted.contains("987,654,321"), visible);
            assert_eq!(mounted.contains("********"), !visible);
            assert_eq!(
                passive_amount(Amount::from_sat(98_765_432), &cache).contains("98 765 432"),
                visible
            );
        }
        cache.balance_privacy = None;
        assert_eq!(
            passive_amount(Amount::from_sat(98_765_432), &cache),
            "********"
        );
    }

    #[test]
    fn deposit_heading_distinguishes_arrival_verification_and_failed_reads() {
        use crate::services::foreign_scan::{AddressFunds, ScanError};
        let empty = AddressFunds::default();
        let pending = AddressFunds {
            pending_sats: 1_000,
            ..empty
        };
        let confirmed = AddressFunds {
            confirmed_sats: 2_000,
            ..empty
        };
        assert_eq!(deposit_heading(None, false), "Checking for funds");
        for (bitcoin, btcb2, expected) in [
            (Ok(empty), Ok(empty), "Waiting for funds"),
            (
                Ok(pending),
                Ok(empty),
                "Funds received at one of two addresses",
            ),
            (
                Ok(empty),
                Ok(confirmed),
                "Funds received at one of two addresses",
            ),
            (
                Ok(pending),
                Ok(confirmed),
                "Funds received at both addresses",
            ),
            (
                Err(ScanError::Unavailable),
                Ok(empty),
                "One deposit address could not be checked; no funds found at the other",
            ),
            (
                Ok(empty),
                Err(ScanError::Unavailable),
                "One deposit address could not be checked; no funds found at the other",
            ),
            (
                Err(ScanError::Unavailable),
                Err(ScanError::Unavailable),
                "Unable to check both deposit addresses",
            ),
            (
                Err(ScanError::Unavailable),
                Ok(pending),
                "Funds received at one address; the other could not be checked",
            ),
            (
                Ok(confirmed),
                Err(ScanError::Unavailable),
                "Funds received at one address; the other could not be checked",
            ),
        ] {
            let status = DepositStatus { bitcoin, btcb2 };
            assert_eq!(deposit_heading(Some(&status), false), expected);
        }
        assert_eq!(
            deposit_heading(None, true),
            "Both deposits verified for replay protection"
        );
    }

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
