//! Safe first surface for canonical BTCB2 plan PR 8.
//!
//! This panel deliberately stops after authenticated, bounded discovery. A scan
//! report is evidence about UTXOs, not permission to sign or spend them. The
//! poison-split actions remain unavailable until the shared Claim ancestry,
//! finalisation and reorg primitives are merged.

use coincube_ui::{
    component::{button, card, text::*},
    theme,
    widget::{Column, Container, Element, Row, RowExt},
};
use iced::{widget::text_input, Alignment, Length, Subscription, Task};
use std::sync::Arc;
use tokio::sync::watch;

use crate::{
    app::split_intent::SplitIntent,
    chain::ChainId,
    dir::CoincubeDirectory,
    services::{
        coincube::CoincubeClient,
        foreign_scan::{self, Branch, BranchRange, ForkSide, ScanDescriptor, ScanError, ScanPlan},
    },
    split_hardware::{self, HardwareMessage, HardwareSource},
};

const DEFAULT_GAP: u32 = 20;
const DEFAULT_RANGE_END: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetCube {
    pub id: String,
    pub name: String,
}

impl std::fmt::Display for TargetCube {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSummary {
    pub generation: u64,
    pub addresses: u32,
    pub coins: usize,
    pub confirmed: usize,
    pub sats: u64,
    /// Confirmed below the observed fork height: the only splittable coins.
    pub pre_fork: usize,
    pub pre_fork_sats: u64,
    /// Confirmed at or after the fork: never swept, and not known to be
    /// BTCB2-only (a replayed transaction may still exist on Bitcoin).
    pub post_fork: usize,
    /// Confirmed but not classifiable (no observed fork or block height).
    pub unclassified: usize,
    /// Every selected source descriptor has a signing route (`tr` has none).
    pub signable: bool,
    pub tip: String,
}

impl ScanSummary {
    /// Whether "Continue to destination Cube" is offered: a signing route
    /// exists, at least one pre-fork coin was proven, and nothing is pending.
    pub fn reviewable(&self) -> bool {
        self.signable && self.pre_fork > 0 && self.confirmed == self.coins
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Editing,
    Scanning,
    Complete(ScanSummary),
    Failed(String),
}

/// Where the foreign wallet's public keys come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Descriptor,
    Hardware,
}

#[derive(Debug, Clone)]
pub enum Message {
    SourceSelected(Source),
    Hardware(HardwareMessage),
    ExternalEdited(String),
    InternalEdited(String),
    TargetSelected(TargetCube),
    Scan,
    Scanned(Result<Arc<ScanEvidence>, String>, u64, u64),
    Continue,
    Cancel,
}

#[derive(Debug, Clone)]
pub struct ScanEvidence {
    report: foreign_scan::ScanReport,
    external: ScanDescriptor,
    internal: Option<ScanDescriptor>,
    summary: ScanSummary,
}

impl ScanEvidence {
    /// Summarise authenticated evidence. Sums are checked: an overflow means
    /// the evidence is not coherent, so no summary (and no handoff) exists.
    fn new(
        report: foreign_scan::ScanReport,
        external: ScanDescriptor,
        internal: Option<ScanDescriptor>,
    ) -> Result<Self, String> {
        let overflow =
            || "The scan totals are out of range. No balance conclusion was made.".to_string();
        let (mut pre_fork, mut pre_fork_sats, mut post_fork, mut unclassified) = (0, 0_u64, 0, 0);
        let mut sats = 0_u64;
        for coin in report.coins() {
            sats = sats
                .checked_add(coin.output.value.to_sat())
                .ok_or_else(overflow)?;
            match report.fork_side(coin) {
                ForkSide::PreFork => {
                    pre_fork += 1;
                    pre_fork_sats = pre_fork_sats
                        .checked_add(coin.output.value.to_sat())
                        .ok_or_else(overflow)?;
                }
                ForkSide::PostFork => post_fork += 1,
                ForkSide::Unknown => unclassified += 1,
                ForkSide::Unconfirmed => {}
            }
        }
        let summary = ScanSummary {
            generation: report.generation(),
            addresses: report.addresses_scanned(),
            coins: report.coins().len(),
            confirmed: report.coins().iter().filter(|coin| coin.confirmed).count(),
            sats,
            pre_fork,
            pre_fork_sats,
            post_fork,
            unclassified,
            signable: std::iter::once(&external)
                .chain(internal.as_ref())
                .all(|descriptor| descriptor.capabilities().signing.psbt_file),
            tip: report.tip().to_string(),
        };
        Ok(Self {
            report,
            external,
            internal,
            summary,
        })
    }
}

pub struct SplitWalletPanel {
    targets: Vec<TargetCube>,
    selected: Option<TargetCube>,
    external: String,
    internal: String,
    status: Status,
    evidence: Option<Arc<ScanEvidence>>,
    generation: u64,
    cancel: watch::Sender<u64>,
    source: Source,
    hardware: HardwareSource,
}

impl Default for SplitWalletPanel {
    fn default() -> Self {
        let (cancel, _) = watch::channel(0);
        Self {
            targets: Vec::new(),
            selected: None,
            external: String::new(),
            internal: String::new(),
            status: Status::Editing,
            evidence: None,
            generation: 0,
            cancel,
            source: Source::Descriptor,
            hardware: HardwareSource::default(),
        }
    }
}

impl SplitWalletPanel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_targets(&mut self, targets: Vec<TargetCube>) {
        self.cancel();
        self.targets = targets;
        if self
            .selected
            .as_ref()
            .is_none_or(|selected| !self.targets.iter().any(|target| target.id == selected.id))
        {
            self.selected = self.targets.first().cloned();
        }
        self.status = Status::Editing;
    }

    pub fn cancel(&mut self) {
        crate::app::split_intent::clear();
        self.generation = self.generation.wrapping_add(1);
        let _ = self.cancel.send(self.generation);
        self.evidence = None;
        self.status = Status::Editing;
        self.hardware.clear();
    }

    #[cfg(test)]
    pub(crate) fn hardware_mut(&mut self) -> &mut HardwareSource {
        &mut self.hardware
    }

    /// Session-only root for the hardware device list; nothing is written.
    pub fn set_hardware_root(&mut self, datadir: CoincubeDirectory) {
        self.hardware.set_root(datadir);
    }

    /// Device polling runs only while the hardware source is chosen.
    pub fn polls_hardware(&self) -> bool {
        self.source == Source::Hardware
    }

    pub fn subscription(&self) -> Subscription<Message> {
        if self.polls_hardware() {
            self.hardware.subscription().map(Message::Hardware)
        } else {
            Subscription::none()
        }
    }

    /// Consume exact scan evidence for the selected target and account
    /// session. The returned intent is memory-only and is still not authority
    /// to construct a transaction.
    pub fn take_handoff(
        &mut self,
        target_source: ChainId,
        account_session_generation: u64,
        client: &CoincubeClient,
    ) -> Option<SplitIntent> {
        let target = self.selected.as_ref()?;
        let evidence = self.evidence.take()?;
        if !matches!(&self.status, Status::Complete(summary) if summary.generation == self.generation)
            || evidence.summary.generation != self.generation
            || !evidence.summary.signable
        {
            self.cancel();
            return None;
        }
        let ScanEvidence {
            report,
            external,
            internal,
            ..
        } = Arc::try_unwrap(evidence).unwrap_or_else(|shared| (*shared).clone());
        let intent = SplitIntent::new(
            target.id.clone(),
            target_source,
            account_session_generation,
            client,
            report,
            external,
            internal,
        );
        self.generation = self.generation.wrapping_add(1);
        let _ = self.cancel.send(self.generation);
        self.status = Status::Editing;
        intent
    }

    pub fn selected_target_id(&self) -> Option<&str> {
        self.selected.as_ref().map(|target| target.id.as_str())
    }

    pub fn update(
        &mut self,
        message: Message,
        client: Option<CoincubeClient>,
        session_generation: u64,
    ) -> Task<Message> {
        match message {
            Message::SourceSelected(source) => {
                if source != self.source {
                    self.cancel();
                    self.source = source;
                }
                Task::none()
            }
            Message::Hardware(message) => {
                let (task, invalidated) = self.hardware.update(message);
                if invalidated {
                    // Scan evidence belongs to the account it was built from.
                    self.generation = self.generation.wrapping_add(1);
                    let _ = self.cancel.send(self.generation);
                    self.evidence = None;
                    self.status = Status::Editing;
                }
                task.map(Message::Hardware)
            }
            Message::ExternalEdited(value) => {
                self.cancel();
                self.external = value;
                self.status = Status::Editing;
                Task::none()
            }
            Message::InternalEdited(value) => {
                self.cancel();
                self.internal = value;
                self.status = Status::Editing;
                Task::none()
            }
            Message::TargetSelected(target) => {
                if self
                    .targets
                    .iter()
                    .any(|candidate| candidate.id == target.id)
                {
                    self.cancel();
                    self.selected = Some(target);
                    self.status = Status::Editing;
                }
                Task::none()
            }
            Message::Cancel => {
                self.cancel();
                Task::none()
            }
            Message::Scan => {
                let Some(client) = client else {
                    self.status = Status::Failed(
                        "Sign in to Connect before scanning a Bitcoin wallet.".to_string(),
                    );
                    return Task::none();
                };
                if self.selected.is_none() {
                    self.status = Status::Failed(
                        "Create a Bitcoin Blake2b Vault before scanning a Bitcoin wallet."
                            .to_string(),
                    );
                    return Task::none();
                }
                if self.source == Source::Hardware && self.hardware.account().is_none() {
                    self.status = Status::Failed(
                        "Read the account from your hardware wallet before scanning.".to_string(),
                    );
                    return Task::none();
                }
                let plan = match self.plan() {
                    Ok(plan) => plan,
                    Err(error) => {
                        self.status = Status::Failed(scan_error_copy(error));
                        return Task::none();
                    }
                };
                self.generation = self.generation.wrapping_add(1);
                let generation = self.generation;
                let _ = self.cancel.send(generation);
                let receiver = self.cancel.subscribe();
                let external = plan.branches[0].descriptor.clone();
                let internal = plan
                    .branches
                    .iter()
                    .find(|range| range.descriptor.branch() == Branch::Internal)
                    .map(|range| range.descriptor.clone());
                self.status = Status::Scanning;
                Task::perform(
                    async move {
                        foreign_scan::scan(client, plan, generation, receiver)
                            .await
                            .map_err(scan_error_copy)
                            .and_then(|report| ScanEvidence::new(report, external, internal))
                            .map(Arc::new)
                    },
                    move |result| Message::Scanned(result, generation, session_generation),
                )
            }
            Message::Scanned(result, generation, fired_session) => {
                if generation != self.generation {
                    return Task::none();
                }
                if fired_session != session_generation {
                    self.cancel();
                    return Task::none();
                }
                match result {
                    Ok(evidence) => {
                        self.status = Status::Complete(evidence.summary.clone());
                        self.evidence = Some(evidence);
                    }
                    Err(error) => {
                        self.evidence = None;
                        self.status = Status::Failed(error);
                    }
                }
                Task::none()
            }
            Message::Continue => Task::none(),
        }
    }

    fn plan(&self) -> Result<ScanPlan, ScanError> {
        if self.source == Source::Hardware {
            let account = self.hardware.account().ok_or(ScanError::Descriptor)?;
            let branches = vec![account.external.clone(), account.internal.clone()]
                .into_iter()
                .map(|descriptor| BranchRange {
                    end_exclusive: descriptor.end_exclusive(DEFAULT_RANGE_END),
                    start: 0,
                    descriptor,
                })
                .collect();
            return Ok(ScanPlan {
                chain: ChainId::BitcoinBlake2b,
                branches,
                gap: DEFAULT_GAP,
            });
        }
        let external = ScanDescriptor::parse(Branch::External, self.external.trim())?;
        let external_end = external.end_exclusive(DEFAULT_RANGE_END);
        let mut branches = vec![BranchRange {
            descriptor: external,
            start: 0,
            end_exclusive: external_end,
        }];
        if !self.internal.trim().is_empty() {
            let internal = ScanDescriptor::parse(Branch::Internal, self.internal.trim())?;
            let internal_end = internal.end_exclusive(DEFAULT_RANGE_END);
            branches.push(BranchRange {
                descriptor: internal,
                start: 0,
                end_exclusive: internal_end,
            });
        }
        Ok(ScanPlan {
            chain: ChainId::BitcoinBlake2b,
            branches,
            gap: DEFAULT_GAP,
        })
    }

    #[cfg(test)]
    fn status(&self) -> &Status {
        &self.status
    }
}

fn scan_error_copy(error: ScanError) -> String {
    match error {
        ScanError::Descriptor => "Enter a supported public mainnet descriptor. Private keys, hardened public derivation and ambiguous multipath descriptors are refused.".to_string(),
        ScanError::RangeLimit | ScanError::AddressLimit => "The wallet history did not reach a safe gap inside this bounded scan. No zero-balance conclusion was made.".to_string(),
        ScanError::Freshness | ScanError::Changed => "The chain changed or fresh evidence could not be proved. Scan again before continuing.".to_string(),
        ScanError::Cancelled => "The scan was cancelled.".to_string(),
        ScanError::Deadline => "The bounded scan timed out before proving a complete result.".to_string(),
        ScanError::Http(401) => "Sign in to Connect before scanning a Bitcoin wallet.".to_string(),
        ScanError::Http(_) | ScanError::Unavailable => "The Bitcoin Blake2b scanner is temporarily unavailable. No balance conclusion was made.".to_string(),
        ScanError::Prevout | ScanError::Malformed => "The scan response could not be authenticated. No balance conclusion was made.".to_string(),
        ScanError::BodyLimit => "The scan exceeded its safety budget before completing.".to_string(),
        ScanError::UnsupportedChain | ScanError::InvalidLimits => "This scan request is not supported.".to_string(),
    }
}

pub fn view(panel: &SplitWalletPanel) -> Element<'_, Message> {
    let header = Column::new()
        .spacing(6)
        .push(h3("Split a Bitcoin wallet"))
        .push(
            p1_regular("Find BTCB2 held by a Sparrow, Electrum, Coldcard or other non-Cube Bitcoin wallet. This step scans public keys only; it cannot sign or move funds.")
                .style(theme::text::secondary),
        );

    let target = iced::widget::pick_list(
        panel.targets.clone(),
        panel.selected.clone(),
        Message::TargetSelected,
    )
    .placeholder("Choose a Bitcoin Blake2b Cube")
    .width(Length::Fill);

    let source_button = |label, source| {
        if panel.source == source {
            button::primary(None, label)
        } else {
            button::secondary(None, label).on_press(Message::SourceSelected(source))
        }
    };
    let form = Column::new()
        .spacing(12)
        .push(p1_bold("Destination Cube"))
        .push(target)
        .push(p1_bold("Source wallet"))
        .push(
            Row::new()
                .spacing(10)
                .push(source_button("Public descriptor", Source::Descriptor))
                .push(source_button("Hardware wallet", Source::Hardware)),
        );
    let form = if panel.source == Source::Hardware {
        form.push(split_hardware::view(&panel.hardware).map(Message::Hardware))
            .push(
                caption("Hardware accounts use standard singlesig paths (BIP84, BIP49, BIP44) on Bitcoin mainnet. Only pre-fork coins can be split. The bounded scan uses a gap of 20 and checks at most 100 addresses per branch.")
                    .style(theme::text::secondary),
            )
    } else {
        form.push(p1_bold("External / receive descriptor"))
        .push(
            text_input("wpkh([fingerprint/path]xpub.../0/*)", &panel.external)
                .on_input(Message::ExternalEdited)
                .padding(10),
        )
        .push(p1_bold("Internal / change descriptor (optional)"))
        .push(
            text_input("wpkh([fingerprint/path]xpub.../1/*)", &panel.internal)
                .on_input(Message::InternalEdited)
                .padding(10),
        )
        .push(
            caption("Supported for discovery: pkh, sh(wpkh), wpkh, wsh(multi/sortedmulti), and tr key-path (scan only: tr has no signing route, so it cannot be split). Only pre-fork coins can be split. Only public mainnet descriptors are accepted. The bounded scan uses a gap of 20 and checks at most 100 addresses per branch.")
                .style(theme::text::secondary),
        )
    };

    let status: Element<Message> = match &panel.status {
        Status::Editing => caption("Ready to scan. An incomplete scan is always an error, never a zero balance.")
            .style(theme::text::secondary)
            .into(),
        Status::Scanning => p1_regular("Scanning with fresh, authenticated chain observations…")
            .style(theme::text::secondary)
            .into(),
        Status::Failed(error) => p1_regular(error).style(theme::text::warning).into(),
        Status::Complete(summary) => Column::new()
            .spacing(6)
            .push(p1_bold(format!(
                "Found {} UTXO{} ({} confirmed), totaling {} sats",
                summary.coins,
                if summary.coins == 1 { "" } else { "s" },
                summary.confirmed,
                summary.sats
            )))
            .push(p1_regular(format!(
                "Pre-fork (splittable): {} UTXO{}, {} sats · Confirmed after the fork (not part of this split; may still be replayable): {} · Unclassified (excluded): {}",
                summary.pre_fork,
                if summary.pre_fork == 1 { "" } else { "s" },
                summary.pre_fork_sats,
                summary.post_fork,
                summary.unclassified
            )))
            .push(caption(format!(
                "{} addresses checked at BTCB2 tip {}",
                summary.addresses, summary.tip
            )).style(theme::text::secondary))
            .push(
                p1_regular(if summary.coins == 0 {
                    "No spendable outputs were found in this bounded scan."
                } else if summary.confirmed != summary.coins {
                    "Wait for every discovered output to confirm, then scan again before reviewing a sweep."
                } else if !summary.signable {
                    "Taproot (tr) wallets can be scanned but not split: there is no signing route for them yet."
                } else if summary.pre_fork == 0 {
                    "No confirmed pre-fork coins were proven. Only pre-fork coins can be split; post-fork or unclassified coins are excluded."
                } else {
                    "Continue to unlock the destination Cube and review bounded sweep economics. Spending remains locked."
                })
                    .style(theme::text::warning),
            )
            .into(),
    };

    let scanning = matches!(panel.status, Status::Scanning);
    let reviewable = matches!(&panel.status, Status::Complete(summary) if summary.reviewable());
    let actions = Row::new()
        .spacing(10)
        .align_y(Alignment::Center)
        .push(
            button::primary(
                None,
                if scanning {
                    "Scanning…"
                } else {
                    "Scan wallet"
                },
            )
            .on_press_maybe((!scanning).then_some(Message::Scan)),
        )
        .push_maybe(scanning.then(|| button::secondary(None, "Cancel").on_press(Message::Cancel)));
    let actions = actions.push_maybe(reviewable.then(|| {
        button::primary(None, "Continue to destination Cube").on_press(Message::Continue)
    }));

    Container::new(
        Column::new()
            .spacing(20)
            .push(header)
            .push(card::simple(form.padding(20)))
            .push(status)
            .push(actions),
    )
    .padding(30)
    .max_width(760)
    .center_x(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::{
        bip32::{Xpriv, Xpub},
        hashes::Hash,
        secp256k1::Secp256k1,
        BlockHash,
    };

    const FIXED_DESCRIPTOR: &str =
        "wpkh(02c6047f9441ed7d6d3045406e95c07cd85aeb5c6b7a3c2e21b73cdb1e24ff3a64)";

    fn target() -> TargetCube {
        TargetCube {
            id: "cube-1".into(),
            name: "Fork Vault".into(),
        }
    }

    fn ranged_descriptor(branch: u32) -> String {
        let secp = Secp256k1::new();
        let xpub = Xpub::from_priv(
            &secp,
            &Xpriv::new_master(
                coincube_core::miniscript::bitcoin::Network::Bitcoin,
                &[42; 32],
            )
            .unwrap(),
        );
        format!("wpkh({xpub}/{branch}/*)")
    }

    #[test]
    fn refuses_scan_without_destination_cube() {
        let mut panel = SplitWalletPanel::new();
        panel.external = FIXED_DESCRIPTOR.into();
        let _ = panel.update(Message::Scan, Some(CoincubeClient::new()), 0);
        assert!(
            matches!(panel.status(), Status::Failed(copy) if copy.contains("Create a Bitcoin Blake2b Vault"))
        );
    }

    #[test]
    fn target_refresh_cancels_inflight_and_drops_missing_selection() {
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        panel.status = Status::Scanning;
        let before = panel.generation;
        panel.set_targets(Vec::new());
        assert_eq!(panel.generation, before + 1);
        assert!(panel.selected.is_none());
        assert_eq!(panel.status(), &Status::Editing);
    }

    #[test]
    fn stale_scan_result_never_paints_new_session() {
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        panel.status = Status::Scanning;
        let generation = panel.generation;
        let _ = panel.update(
            Message::Scanned(Err("old account".into()), generation, 3),
            None,
            4,
        );
        assert_eq!(panel.status(), &Status::Editing);
    }

    #[test]
    fn edits_and_target_changes_invalidate_inflight_results() {
        let other_target = TargetCube {
            id: "cube-2".into(),
            name: "Other Vault".into(),
        };
        for message in [
            Message::ExternalEdited(FIXED_DESCRIPTOR.into()),
            Message::InternalEdited(FIXED_DESCRIPTOR.into()),
            Message::TargetSelected(other_target.clone()),
        ] {
            let mut panel = SplitWalletPanel::new();
            panel.set_targets(vec![target(), other_target.clone()]);
            panel.status = Status::Scanning;
            let stale_generation = panel.generation;

            let _ = panel.update(message, None, 7);
            assert_eq!(panel.generation, stale_generation + 1);
            assert_eq!(panel.status(), &Status::Editing);

            let _ = panel.update(
                Message::Scanned(Err("stale".into()), stale_generation, 7),
                None,
                7,
            );
            assert_eq!(panel.status(), &Status::Editing);
        }
    }

    #[test]
    fn plan_uses_single_index_for_fixed_descriptors_and_bound_for_wildcards() {
        let mut panel = SplitWalletPanel::new();
        panel.external = FIXED_DESCRIPTOR.into();
        panel.internal = FIXED_DESCRIPTOR.into();
        let fixed = panel.plan().unwrap();
        assert_eq!(fixed.branches[0].end_exclusive, 1);
        assert_eq!(fixed.branches[1].end_exclusive, 1);

        panel.external = ranged_descriptor(0);
        panel.internal = ranged_descriptor(1);
        let wildcard = panel.plan().unwrap();
        assert_eq!(wildcard.branches[0].end_exclusive, DEFAULT_RANGE_END);
        assert_eq!(wildcard.branches[1].end_exclusive, DEFAULT_RANGE_END);
    }

    #[test]
    fn parse_failure_is_explicit_and_does_not_start_scan() {
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        panel.external = "not a descriptor".into();
        let _ = panel.update(Message::Scan, Some(CoincubeClient::new()), 0);
        assert!(
            matches!(panel.status(), Status::Failed(copy) if copy.contains("public mainnet descriptor"))
        );
    }

    #[test]
    fn completed_scan_handoff_retains_exact_report_and_descriptors() {
        let mut client = CoincubeClient::new();
        client.set_token("session-a");
        let external = ScanDescriptor::parse(Branch::External, FIXED_DESCRIPTOR).unwrap();
        let report = foreign_scan::ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            17,
            BlockHash::from_byte_array([9; 32]),
            Vec::new(),
        );
        let evidence = ScanEvidence::new(report, external, None).unwrap();
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        panel.generation = 17;
        panel.status = Status::Complete(evidence.summary.clone());
        panel.evidence = Some(Arc::new(evidence));

        let intent = panel
            .take_handoff(ChainId::BitcoinBlake2b, 4, &client)
            .unwrap();
        assert_eq!(intent.report.generation(), 17);
        assert_eq!(intent.report.tip(), BlockHash::from_byte_array([9; 32]));
        assert_eq!(
            intent.external.canonical(),
            ScanDescriptor::parse(Branch::External, FIXED_DESCRIPTOR)
                .unwrap()
                .canonical()
        );
        assert!(intent.internal.is_none());
        assert!(panel.evidence.is_none());
        assert!(matches!(panel.status, Status::Editing));
    }

    #[test]
    fn summary_counts_pre_and_post_fork_coins_separately() {
        use coincube_core::miniscript::bitcoin::{
            absolute, transaction, Amount, OutPoint, Transaction, TxIn, TxOut,
        };
        let external = ScanDescriptor::parse(Branch::External, FIXED_DESCRIPTOR).unwrap();
        let coin = |vout: u32, height: Option<u32>, sats: u64| {
            let previous = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![TxIn::default()],
                output: vec![TxOut {
                    value: Amount::from_sat(sats),
                    script_pubkey: external.script(0).unwrap(),
                }],
            };
            foreign_scan::DiscoveredCoin {
                branch: Branch::External,
                index: 0,
                outpoint: OutPoint::new(previous.compute_txid(), vout),
                output: previous.output[0].clone(),
                previous,
                confirmed: true,
                block_height: height,
                block_hash: height.map(|_| BlockHash::from_byte_array([1; 32])),
            }
        };
        let coins = vec![
            coin(0, Some(99), 1_000),
            coin(1, Some(100), 2_000),
            coin(2, None, 4_000),
        ];
        let report = foreign_scan::ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            3,
            BlockHash::from_byte_array([9; 32]),
            coins.clone(),
        )
        .with_fork_height(Some(100));
        let summary = ScanEvidence::new(report, external.clone(), None)
            .unwrap()
            .summary;
        assert_eq!(
            (
                summary.pre_fork,
                summary.pre_fork_sats,
                summary.post_fork,
                summary.unclassified
            ),
            (1, 1_000, 1, 1)
        );

        // No observed fork height: nothing is splittable.
        let report = foreign_scan::ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            3,
            BlockHash::from_byte_array([9; 32]),
            coins,
        );
        let summary = ScanEvidence::new(report, external, None).unwrap().summary;
        assert_eq!((summary.pre_fork, summary.unclassified), (0, 3));
    }

    fn hardware_account() -> split_hardware::HardwareAccount {
        use coincube_core::miniscript::bitcoin::bip32::DerivationPath;
        let secp = Secp256k1::new();
        let root = Xpriv::new_master(
            coincube_core::miniscript::bitcoin::Network::Bitcoin,
            &[42; 32],
        )
        .unwrap();
        let path: DerivationPath = "m/84'/0'/0'".parse().unwrap();
        let xpub = Xpub::from_priv(&secp, &root.derive_priv(&secp, &path).unwrap());
        let fingerprint = root.fingerprint(&secp);
        split_hardware::account_from_device(
            "dev".into(),
            split_hardware::Purpose::Bip84,
            0,
            fingerprint,
            fingerprint,
            xpub,
        )
        .unwrap()
    }

    #[test]
    fn split_hardware_source_scans_both_branches_and_refuses_without_account() {
        // cancel() clears the process-wide Split intent slot.
        let _guard = crate::app::session::test_guard();
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        let _ = panel.update(Message::SourceSelected(Source::Hardware), None, 0);
        assert!(panel.polls_hardware());
        let _ = panel.update(Message::Scan, Some(CoincubeClient::new()), 0);
        assert!(
            matches!(panel.status(), Status::Failed(copy) if copy.contains("Read the account"))
        );

        panel.hardware.ready_for_test(hardware_account());
        let plan = panel.plan().unwrap();
        assert_eq!(plan.branches.len(), 2);
        assert_eq!(plan.branches[0].descriptor.branch(), Branch::External);
        assert_eq!(plan.branches[1].descriptor.branch(), Branch::Internal);
        assert!(plan
            .branches
            .iter()
            .all(|b| b.end_exclusive == DEFAULT_RANGE_END));
        assert!(plan.branches[0].descriptor.canonical().contains("/0/*"));
        assert!(plan.branches[1].descriptor.canonical().contains("/1/*"));

        // Cancel and switching source clear the read account and stop polling.
        panel.cancel();
        assert!(panel.hardware.account().is_none());
        panel.hardware.ready_for_test(hardware_account());
        let _ = panel.update(Message::SourceSelected(Source::Descriptor), None, 0);
        assert!(panel.hardware.account().is_none());
        assert!(!panel.polls_hardware());
    }

    #[test]
    fn split_hardware_change_invalidates_completed_scan_evidence() {
        // cancel() clears the process-wide Split intent slot.
        let _guard = crate::app::session::test_guard();
        let external = ScanDescriptor::parse(Branch::External, FIXED_DESCRIPTOR).unwrap();
        let report = foreign_scan::ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            5,
            BlockHash::from_byte_array([3; 32]),
            Vec::new(),
        );
        let evidence = ScanEvidence::new(report, external, None).unwrap();
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        panel.source = Source::Hardware;
        panel.generation = 5;
        panel.status = Status::Complete(evidence.summary.clone());
        panel.evidence = Some(Arc::new(evidence));

        let _ = panel.update(
            Message::Hardware(HardwareMessage::AccountEdited("1".into())),
            None,
            0,
        );
        assert!(panel.evidence.is_none());
        assert_eq!(panel.status(), &Status::Editing);
        assert_eq!(panel.generation, 6);
    }

    /// #578 review I6: an impossible total is refused, not wrapped or panicked.
    #[test]
    fn overflowing_totals_refuse_the_summary() {
        use coincube_core::miniscript::bitcoin::{
            absolute, transaction, Amount, OutPoint, Transaction, TxIn, TxOut,
        };
        let external = ScanDescriptor::parse(Branch::External, FIXED_DESCRIPTOR).unwrap();
        let previous = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(u64::MAX / 2 + 1),
                script_pubkey: external.script(0).unwrap(),
            }],
        };
        let coins = (0..2)
            .map(|vout| foreign_scan::DiscoveredCoin {
                branch: Branch::External,
                index: 0,
                outpoint: OutPoint::new(previous.compute_txid(), vout),
                output: previous.output[0].clone(),
                previous: previous.clone(),
                confirmed: true,
                block_height: Some(1),
                block_hash: Some(BlockHash::from_byte_array([1; 32])),
            })
            .collect();
        let report = foreign_scan::ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            3,
            BlockHash::from_byte_array([9; 32]),
            coins,
        )
        .with_fork_height(Some(100));
        assert!(ScanEvidence::new(report, external, None).is_err());
    }

    #[test]
    fn a_taproot_only_source_is_not_reviewable_and_cannot_hand_off() {
        let secp = Secp256k1::new();
        let xpub = Xpub::from_priv(
            &secp,
            &Xpriv::new_master(
                coincube_core::miniscript::bitcoin::Network::Bitcoin,
                &[42; 32],
            )
            .unwrap(),
        );
        let tr = ScanDescriptor::parse(Branch::External, &format!("tr({xpub}/0/*)")).unwrap();
        let report = foreign_scan::ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            5,
            BlockHash::from_byte_array([9; 32]),
            Vec::new(),
        );
        let evidence = ScanEvidence::new(report, tr, None).unwrap();
        assert!(!evidence.summary.signable);
        let mut signable = evidence.summary.clone();
        signable.pre_fork = 1;
        signable.coins = 1;
        signable.confirmed = 1;
        assert!(!signable.reviewable());
        signable.signable = true;
        assert!(signable.reviewable());

        let mut client = CoincubeClient::new();
        client.set_token("session-a");
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        panel.generation = 5;
        panel.status = Status::Complete(evidence.summary.clone());
        panel.evidence = Some(Arc::new(evidence));
        assert!(panel
            .take_handoff(ChainId::BitcoinBlake2b, 4, &client)
            .is_none());
    }

    #[test]
    fn cancel_discards_completed_scan_evidence() {
        let external = ScanDescriptor::parse(Branch::External, FIXED_DESCRIPTOR).unwrap();
        let report = foreign_scan::ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            23,
            BlockHash::from_byte_array([10; 32]),
            Vec::new(),
        );
        let evidence = ScanEvidence::new(report, external, None).unwrap();
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        panel.generation = 23;
        panel.status = Status::Complete(evidence.summary.clone());
        panel.evidence = Some(Arc::new(evidence));

        panel.cancel();

        assert!(panel.evidence.is_none());
        assert!(matches!(panel.status, Status::Editing));
        assert_eq!(panel.generation, 24);
    }
}
