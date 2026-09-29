//! Safe first surface for canonical BTCB2 plan PR 8.
//!
//! This panel deliberately stops after authenticated, bounded discovery on
//! both chains. A scan report and its two-chain inventory are evidence about
//! UTXOs, not permission to sign or spend them. The
//! poison-split actions remain unavailable until the shared Claim ancestry,
//! finalisation and reorg primitives are merged.

use coincube_ui::{
    component::{button, card, text::*},
    theme,
    widget::{Column, Container, Element, Row, RowExt},
};
use iced::{widget::text_input, Alignment, Length, Task};
use std::sync::Arc;
use tokio::sync::watch;

use crate::{
    app::split_intent::SplitIntent,
    chain::ChainId,
    services::{
        coincube::CoincubeClient,
        foreign_scan::{self, Branch, BranchRange, ForkSide, ScanDescriptor, ScanError, ScanPlan},
        foreign_split_inventory::{self, FreshIndex, InventoryError, SplitInventory},
    },
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
    pub inventory: InventorySummary,
}

/// Two-chain categories, display only. See `foreign_split_inventory`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventorySummary {
    pub fork_height: u64,
    pub bitcoin_tip: String,
    pub splittable: usize,
    pub splittable_sats: u64,
    pub spent_on_bitcoin: usize,
    pub spent_on_btcb2: usize,
    pub btcb2_post_fork: usize,
    /// Possible input poisons only; never poison proof.
    pub bitcoin_only_candidates: usize,
    pub pending: usize,
    pub fresh_receive: FreshIndex,
}

impl InventorySummary {
    /// Checked like `ScanEvidence::new`: an impossible total refuses.
    fn of(inventory: &SplitInventory) -> Option<Self> {
        let splittable_sats = inventory
            .splittable()
            .iter()
            .try_fold(0_u64, |sum, coin| sum.checked_add(coin.sats))?;
        Some(Self {
            fork_height: inventory.fork_height(),
            bitcoin_tip: inventory.bitcoin_tip().to_string(),
            splittable: inventory.splittable().len(),
            splittable_sats,
            spent_on_bitcoin: inventory.spent_on_bitcoin().len(),
            spent_on_btcb2: inventory.spent_on_btcb2().len(),
            btcb2_post_fork: inventory.btcb2_post_fork().len(),
            bitcoin_only_candidates: inventory.bitcoin_only_post_fork().len(),
            pending: inventory.pending().len(),
            fresh_receive: inventory.fresh_receive(),
        })
    }
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

#[derive(Debug, Clone)]
pub enum Message {
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
    inventory: SplitInventory,
    external: ScanDescriptor,
    internal: Option<ScanDescriptor>,
    summary: ScanSummary,
}

impl ScanEvidence {
    /// Summarise authenticated evidence. Sums are checked: an overflow means
    /// the evidence is not coherent, so no summary (and no handoff) exists.
    fn new(
        report: foreign_scan::ScanReport,
        inventory: SplitInventory,
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
            inventory: InventorySummary::of(&inventory).ok_or_else(overflow)?,
        };
        Ok(Self {
            report,
            inventory,
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
                        foreign_split_inventory::scan(client, plan, generation, receiver)
                            .await
                            .map_err(inventory_error_copy)
                            .and_then(|two| {
                                ScanEvidence::new(two.btcb2, two.inventory, external, internal)
                            })
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
                    // Evidence from any other generation is stale by definition.
                    Ok(evidence)
                        if evidence.summary.generation != generation
                            || evidence.inventory.generation() != generation =>
                    {
                        self.cancel();
                    }
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

    #[cfg(test)]
    pub(crate) fn targets(&self) -> &[TargetCube] {
        &self.targets
    }
}

fn inventory_error_copy(error: InventoryError) -> String {
    match error {
        InventoryError::Scan(ChainId::Bitcoin, error) => {
            format!("Bitcoin chain: {}", scan_error_copy(error))
        }
        InventoryError::Scan(_, error) => {
            format!("Bitcoin Blake2b chain: {}", scan_error_copy(error))
        }
        InventoryError::Stale => "The scan was cancelled.".to_string(),
        InventoryError::ForkHeightUnknown => "Connect did not report the active Bitcoin Blake2b fork height, so pre-fork coins cannot be identified. No balance conclusion was made.".to_string(),
        InventoryError::PrevoutMismatch(_) | InventoryError::Inconsistent(_) => "The two chains disagree about a pre-fork coin. The inventory was refused; no balance conclusion was made.".to_string(),
        InventoryError::Coverage(..) => "A coin lies beyond the other chain's bounded scan, so its status there is unknown. No balance conclusion was made.".to_string(),
        InventoryError::WrongChain => "This scan request is not supported.".to_string(),
    }
}

fn fresh_index_copy(fresh: FreshIndex) -> String {
    match fresh {
        FreshIndex::Proven(index) => {
            format!("Receive index {index} is unused on both chains.")
        }
        FreshIndex::FixedDescriptor => {
            "This single-address descriptor has no fresh receive address.".to_string()
        }
        FreshIndex::NotProven => "No receive index was proven unused on both chains.".to_string(),
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
        ScanError::Http(_) | ScanError::Unavailable => "The scanner is temporarily unavailable. No balance conclusion was made.".to_string(),
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
            p1_regular("Find BTCB2 held by a Sparrow, Electrum, Coldcard or other non-Cube Bitcoin wallet. This step scans public descriptors on Bitcoin and Bitcoin Blake2b only; it cannot sign or move funds.")
                .style(theme::text::secondary),
        );

    let target = iced::widget::pick_list(
        panel.targets.clone(),
        panel.selected.clone(),
        Message::TargetSelected,
    )
    .placeholder("Choose a Bitcoin Blake2b Cube")
    .width(Length::Fill);

    let form = Column::new()
        .spacing(12)
        .push(p1_bold("Destination Cube"))
        .push(target)
        .push(p1_bold("External / receive descriptor"))
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
        );

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
            .push(p1_regular(format!(
                "Unspent on both chains (splittable): {} UTXO{}, {} sats · Spent on Bitcoin: {} · Spent on Bitcoin Blake2b: {} · Pending or replayed: {}",
                summary.inventory.splittable,
                if summary.inventory.splittable == 1 { "" } else { "s" },
                summary.inventory.splittable_sats,
                summary.inventory.spent_on_bitcoin,
                summary.inventory.spent_on_btcb2,
                summary.inventory.pending,
            )))
            .push(caption(format!(
                "Bitcoin-only coins received after the fork: {} (possible input poison; not yet verified, display only). {}",
                summary.inventory.bitcoin_only_candidates,
                fresh_index_copy(summary.inventory.fresh_receive),
            )).style(theme::text::secondary))
            .push(caption(format!(
                "{} BTCB2 addresses checked at BTCB2 tip {} · Bitcoin tip {} · fork height {} (observed)",
                summary.addresses, summary.tip, summary.inventory.bitcoin_tip, summary.inventory.fork_height
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

    fn walk() -> Vec<foreign_scan::BranchCoverage> {
        vec![foreign_scan::BranchCoverage {
            branch: Branch::External,
            start: 0,
            end_exclusive: 1,
            last_used: Some(0),
        }]
    }

    /// Evidence joined against a Bitcoin report holding `bitcoin_coins`.
    fn evidence(
        report: foreign_scan::ScanReport,
        bitcoin_coins: Vec<foreign_scan::DiscoveredCoin>,
    ) -> ScanEvidence {
        let report = report.with_coverage(walk());
        let bitcoin = foreign_scan::ScanReport::for_test(
            ChainId::Bitcoin,
            report.generation(),
            BlockHash::from_byte_array([8; 32]),
            bitcoin_coins,
        )
        .with_coverage(walk());
        let inventory =
            SplitInventory::join(&report, &bitcoin, report.generation(), false).unwrap();
        let external = ScanDescriptor::parse(Branch::External, FIXED_DESCRIPTOR).unwrap();
        ScanEvidence::new(report, inventory, external, None).unwrap()
    }

    /// A joined inventory against an empty, fully covered Bitcoin report.
    fn empty_bitcoin_inventory(report: &foreign_scan::ScanReport) -> SplitInventory {
        let bitcoin = foreign_scan::ScanReport::for_test(
            ChainId::Bitcoin,
            report.generation(),
            BlockHash::from_byte_array([8; 32]),
            Vec::new(),
        )
        .with_coverage(walk());
        SplitInventory::join(report, &bitcoin, report.generation(), true).unwrap()
    }

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
        let _guard = crate::app::session::test_guard();
        let mut panel = SplitWalletPanel::new();
        panel.external = FIXED_DESCRIPTOR.into();
        let _ = panel.update(Message::Scan, Some(CoincubeClient::new()), 0);
        assert!(
            matches!(panel.status(), Status::Failed(copy) if copy.contains("Create a Bitcoin Blake2b Vault"))
        );
    }

    #[test]
    fn target_refresh_cancels_inflight_and_drops_missing_selection() {
        let _guard = crate::app::session::test_guard();
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
        let _guard = crate::app::session::test_guard();
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
        let _guard = crate::app::session::test_guard();
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
        let _guard = crate::app::session::test_guard();
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
        let _guard = crate::app::session::test_guard();
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
        let _guard = crate::app::session::test_guard();
        let mut client = CoincubeClient::new();
        client.set_token("session-a");
        let report = foreign_scan::ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            17,
            BlockHash::from_byte_array([9; 32]),
            Vec::new(),
        )
        .with_fork_height(Some(100));
        let evidence = evidence(report, Vec::new());
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
        let _guard = crate::app::session::test_guard();
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
        // The pre-fork coin is also unspent on Bitcoin; a Bitcoin coin
        // confirmed after the fork is only a displayed candidate.
        let bitcoin_only = coin(3, Some(150), 8_000);
        let summary = evidence(report.clone(), vec![coins[0].clone(), bitcoin_only]).summary;
        assert_eq!(
            (
                summary.pre_fork,
                summary.pre_fork_sats,
                summary.post_fork,
                summary.unclassified
            ),
            (1, 1_000, 1, 1)
        );
        assert_eq!(
            summary.inventory,
            InventorySummary {
                fork_height: 100,
                bitcoin_tip: BlockHash::from_byte_array([8; 32]).to_string(),
                splittable: 1,
                splittable_sats: 1_000,
                spent_on_bitcoin: 0,
                spent_on_btcb2: 0,
                btcb2_post_fork: 1,
                bitcoin_only_candidates: 1,
                pending: 1,
                fresh_receive: FreshIndex::FixedDescriptor,
            }
        );
        // Absent from the Bitcoin UTXO set: shown as spent on Bitcoin.
        let summary = evidence(report, Vec::new()).summary;
        assert_eq!(
            (
                summary.inventory.splittable,
                summary.inventory.spent_on_bitcoin
            ),
            (0, 1)
        );

        // No observed fork height: the inventory refuses instead of guessing.
        let report = foreign_scan::ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            3,
            BlockHash::from_byte_array([9; 32]),
            coins,
        );
        let bitcoin = foreign_scan::ScanReport::for_test(
            ChainId::Bitcoin,
            3,
            BlockHash::from_byte_array([8; 32]),
            Vec::new(),
        );
        let error = SplitInventory::join(&report, &bitcoin, 3, false).unwrap_err();
        assert_eq!(error, InventoryError::ForkHeightUnknown);
        assert!(inventory_error_copy(error).contains("fork height"));
    }

    #[test]
    fn split_evidence_from_another_generation_is_discarded() {
        let _guard = crate::app::session::test_guard();
        let report = foreign_scan::ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            8,
            BlockHash::from_byte_array([9; 32]),
            Vec::new(),
        )
        .with_fork_height(Some(100));
        let stale = Arc::new(evidence(report, Vec::new()));
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        panel.generation = 9;
        panel.status = Status::Scanning;
        // Delivered under the current generation tag but carrying older evidence.
        let _ = panel.update(Message::Scanned(Ok(stale), 9, 1), None, 1);
        assert!(panel.evidence.is_none());
        assert_eq!(panel.status(), &Status::Editing);
        assert_eq!(panel.generation, 10);
    }

    #[test]
    fn split_incomplete_bitcoin_scan_fails_the_panel_explicitly() {
        let _guard = crate::app::session::test_guard();
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        panel.status = Status::Scanning;
        let generation = panel.generation;
        let copy = inventory_error_copy(InventoryError::Scan(
            ChainId::Bitcoin,
            ScanError::RangeLimit,
        ));
        let _ = panel.update(Message::Scanned(Err(copy), generation, 2), None, 2);
        assert!(
            matches!(panel.status(), Status::Failed(copy) if copy.starts_with("Bitcoin chain:") && copy.contains("No zero-balance conclusion"))
        );
        assert!(panel.evidence.is_none());
    }

    /// #578 review I6: an impossible total is refused, not wrapped or panicked.
    #[test]
    fn overflowing_totals_refuse_the_summary() {
        let _guard = crate::app::session::test_guard();
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
        .with_fork_height(Some(100))
        .with_coverage(walk());
        let inventory = empty_bitcoin_inventory(&report);
        assert!(ScanEvidence::new(report.clone(), inventory, external, None).is_err());

        // The two-chain splittable total is checked the same way.
        let bitcoin = foreign_scan::ScanReport::for_test(
            ChainId::Bitcoin,
            3,
            BlockHash::from_byte_array([8; 32]),
            report.coins().to_vec(),
        )
        .with_coverage(walk());
        let joined = SplitInventory::join(&report, &bitcoin, 3, false).unwrap();
        assert_eq!(joined.splittable().len(), 2);
        assert_eq!(InventorySummary::of(&joined), None);
    }

    #[test]
    fn a_taproot_only_source_is_not_reviewable_and_cannot_hand_off() {
        let _guard = crate::app::session::test_guard();
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
        )
        .with_fork_height(Some(100))
        .with_coverage(walk());
        let inventory = empty_bitcoin_inventory(&report);
        let evidence = ScanEvidence::new(report, inventory, tr, None).unwrap();
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
        let _guard = crate::app::session::test_guard();
        let report = foreign_scan::ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            23,
            BlockHash::from_byte_array([10; 32]),
            Vec::new(),
        )
        .with_fork_height(Some(100));
        let evidence = evidence(report, Vec::new());
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

    #[test]
    fn every_supported_source_remains_discovery_only() {
        use crate::services::foreign_scan::{Capabilities, SigningRoutes};

        let secp = Secp256k1::new();
        let first = Xpub::from_priv(
            &secp,
            &Xpriv::new_master(
                coincube_core::miniscript::bitcoin::Network::Bitcoin,
                &[41; 32],
            )
            .unwrap(),
        );
        let second = Xpub::from_priv(
            &secp,
            &Xpriv::new_master(
                coincube_core::miniscript::bitcoin::Network::Bitcoin,
                &[43; 32],
            )
            .unwrap(),
        );
        // #568 A1: only the PSBT-file route exists, and `tr` has none. No
        // shape grants in-app hardware, unified-seed or Claim authority.
        let expected = |taproot: bool| Capabilities {
            scan: true,
            signing: SigningRoutes {
                psbt_file: !taproot,
                ..SigningRoutes::NONE
            },
            claim_authorization: false,
        };

        // Covers the PR 8 discovery shapes with and without origin metadata.
        // A hardware-exported account xpub is public material here: accepting
        // it for discovery must never imply that a device was connected,
        // accepted the PSBT, or granted Claim authority.
        for descriptor in [
            format!("wpkh({first}/0/*)"),
            format!("wpkh([d34db33f/84h/0h/0h]{first}/0/*)"),
            format!("sh(wpkh({first}/0/*))"),
            format!("pkh({first}/0/*)"),
            format!("wsh(sortedmulti(2,{first}/0/*,{second}/0/*))"),
            format!("tr({first}/0/*)"),
        ] {
            let parsed =
                ScanDescriptor::parse(Branch::External, &descriptor).unwrap_or_else(|_| {
                    panic!("supported discovery descriptor refused: {}", descriptor)
                });
            assert_eq!(
                parsed.capabilities(),
                expected(descriptor.starts_with("tr(")),
                "{}",
                descriptor
            );
        }

        // Private material, ambiguous branches and hardened public derivation
        // fail before a scan can start. They are not silently downgraded to a
        // scan-only or replayable signing route.
        let secret = Xpriv::new_master(
            coincube_core::miniscript::bitcoin::Network::Bitcoin,
            &[44; 32],
        )
        .unwrap();
        for descriptor in [
            format!("wpkh({secret}/0/*)"),
            format!("wpkh({first}/<0;1>/*)"),
            format!("wpkh({first}/0'/*)"),
            format!("wpkh({first}/0/*')"),
            "raw(51)".to_string(),
        ] {
            assert!(
                ScanDescriptor::parse(Branch::External, &descriptor).is_err(),
                "unsafe discovery descriptor accepted: {}",
                descriptor
            );
        }
    }

    #[test]
    fn every_scan_failure_has_explicit_refusal_semantics() {
        let incomplete = [
            ScanError::RangeLimit,
            ScanError::AddressLimit,
            ScanError::Deadline,
            ScanError::BodyLimit,
        ];
        for error in incomplete {
            let copy = scan_error_copy(error);
            assert!(
                copy.contains("bounded")
                    || copy.contains("safe gap")
                    || copy.contains("safety budget"),
                "bounded failure lost its refusal: {:?}: {}",
                error,
                copy
            );
            assert!(!copy.to_ascii_lowercase().contains("zero balance"));
        }

        for error in [
            ScanError::Freshness,
            ScanError::Changed,
            ScanError::Unavailable,
            ScanError::Http(503),
            ScanError::Prevout,
            ScanError::Malformed,
        ] {
            let copy = scan_error_copy(error);
            assert!(
                copy.contains("No balance conclusion") || copy.contains("Scan again"),
                "evidence failure could be mistaken for an empty wallet: {:?}: {}",
                error,
                copy
            );
        }

        assert!(scan_error_copy(ScanError::Http(401)).contains("Sign in to Connect"));
        assert!(scan_error_copy(ScanError::Descriptor).contains("Private keys"));
        assert!(scan_error_copy(ScanError::Cancelled).contains("cancelled"));
        for error in [ScanError::UnsupportedChain, ScanError::InvalidLimits] {
            assert!(scan_error_copy(error).contains("not supported"));
        }
    }

    /// Completed, authenticated (empty) evidence for `generation`.
    fn completed_evidence(generation: u64) -> Arc<ScanEvidence> {
        let report = foreign_scan::ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            generation,
            BlockHash::from_byte_array([9; 32]),
            Vec::new(),
        );
        let external = ScanDescriptor::parse(Branch::External, FIXED_DESCRIPTOR).unwrap();
        Arc::new(ScanEvidence::new(report, external, None).unwrap())
    }

    #[test]
    fn scan_result_is_evidence_only_and_never_a_signing_transition() {
        let _guard = crate::app::session::test_guard();
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        let generation = panel.generation;

        let _ = panel.update(
            Message::Scanned(Ok(completed_evidence(generation)), generation, 9),
            None,
            9,
        );

        assert!(matches!(panel.status(), Status::Complete(_)));
        // The state machine intentionally has no authorize/sign/broadcast
        // state. A completed discovery can only be invalidated back to Editing.
        let _ = panel.update(Message::TargetSelected(target()), None, 9);
        assert_eq!(panel.status(), &Status::Editing);
    }
}
