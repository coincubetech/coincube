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
use iced::{Alignment, Length, Subscription, Task};
use std::sync::Arc;
use tokio::sync::watch;

use crate::{
    app::split_intent::SplitIntent,
    chain::ChainId,
    dir::CoincubeDirectory,
    imported_claim::ImportedClaimSource,
    services::{
        coincube::CoincubeClient,
        foreign_scan::{self, Branch, BranchRange, ForkSide, ScanDescriptor, ScanError, ScanPlan},
        foreign_split_inventory::{self, FreshIndex, InventoryError, SplitInventory, TwoChainScan},
    },
    split_hardware::HardwareMessage,
    wallet_source_form::{self, SourceFormCopy, WalletSourceForm},
};

#[cfg(test)]
use crate::split_hardware::{self, HardwareSource};

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
    /// Seen on one chain within both scans' coverage; never ancestry proof.
    pub bitcoin_only_candidates: usize,
    pub btcb2_only_candidates: usize,
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
            btcb2_only_candidates: inventory.btcb2_only_post_fork().len(),
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

pub use wallet_source_form::Source;

impl From<wallet_source_form::Message> for Message {
    fn from(message: wallet_source_form::Message) -> Self {
        match message {
            wallet_source_form::Message::SourceSelected(source) => Self::SourceSelected(source),
            wallet_source_form::Message::Hardware(message) => Self::Hardware(message),
            wallet_source_form::Message::ExternalEdited(value) => Self::ExternalEdited(value),
            wallet_source_form::Message::InternalEdited(value) => Self::InternalEdited(value),
        }
    }
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
    bitcoin: foreign_scan::ScanReport,
    inventory: SplitInventory,
    external: ScanDescriptor,
    internal: Option<ScanDescriptor>,
    summary: ScanSummary,
}

impl ScanEvidence {
    /// Summarise authenticated evidence. Sums are checked: an overflow means
    /// the evidence is not coherent, so no summary (and no handoff) exists.
    fn new(
        scan: TwoChainScan,
        external: ScanDescriptor,
        internal: Option<ScanDescriptor>,
    ) -> Result<Self, String> {
        let TwoChainScan {
            btcb2: report,
            bitcoin,
            inventory,
        } = scan;
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
            bitcoin,
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
    source_form: WalletSourceForm,
    imported_source: Option<ImportedClaimSource>,
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
            source_form: WalletSourceForm::default(),
            imported_source: None,
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

    /// Bind this panel to the public source saved with an imported Bitcoin
    /// wallet. The caller must revalidate that binding against the active
    /// wallet before presenting the panel or accepting a handoff.
    pub fn set_imported_source(&mut self, source: ImportedClaimSource) -> Result<(), String> {
        let external =
            ScanDescriptor::parse(Branch::External, &source.external).map_err(scan_error_copy)?;
        let internal =
            ScanDescriptor::parse(Branch::Internal, &source.internal).map_err(scan_error_copy)?;
        let plan = ScanPlan {
            chain: ChainId::BitcoinBlake2b,
            branches: vec![external, internal]
                .into_iter()
                .map(|descriptor| BranchRange {
                    descriptor,
                    start: 0,
                    end_exclusive: source.address_window,
                })
                .collect(),
            gap: DEFAULT_GAP,
        };
        foreign_scan::validate_imported_plan(&plan).map_err(scan_error_copy)?;
        self.cancel();
        self.source_form.select_source(Source::Descriptor);
        self.source_form.edit_external(source.external.clone());
        self.source_form.edit_internal(source.internal.clone());
        self.imported_source = Some(source);
        Ok(())
    }

    pub fn imported_source(&self) -> Option<&ImportedClaimSource> {
        self.imported_source.as_ref()
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
        self.source_form.clear_transient();
    }

    #[cfg(test)]
    pub(crate) fn hardware_mut(&mut self) -> &mut HardwareSource {
        self.source_form.hardware_mut()
    }

    /// Session-only root for the hardware device list; nothing is written.
    pub fn set_hardware_root(&mut self, datadir: CoincubeDirectory) {
        self.source_form.set_hardware_root(datadir);
    }

    /// Device polling runs only while the hardware source is chosen.
    pub fn polls_hardware(&self) -> bool {
        self.imported_source.is_none() && self.source_form.polls_hardware()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        if self.imported_source.is_some() {
            Subscription::none()
        } else {
            self.source_form.subscription().map(Message::from)
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
            || !self.imported_evidence_matches(&evidence)
        {
            self.cancel();
            return None;
        }
        let ScanEvidence {
            report,
            bitcoin,
            inventory,
            external,
            internal,
            ..
        } = Arc::try_unwrap(evidence).unwrap_or_else(|shared| (*shared).clone());
        let intent = SplitIntent::new(
            target.id.clone(),
            target_source,
            account_session_generation,
            client,
            TwoChainScan {
                btcb2: report,
                bitcoin,
                inventory,
            },
            external,
            internal,
        );
        let intent = match self.imported_source.clone() {
            Some(source) => intent.and_then(|intent| intent.bind_imported_source(source, client)),
            None => intent,
        };
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
                if self.imported_source.is_some() {
                    return Task::none();
                }
                if source != self.source_form.source() {
                    self.cancel();
                    self.source_form.select_source(source);
                }
                Task::none()
            }
            Message::Hardware(message) => {
                if self.imported_source.is_some() {
                    return Task::none();
                }
                let (task, invalidated) = self.source_form.update_hardware(message);
                if invalidated {
                    // Scan evidence belongs to the account it was built from.
                    self.generation = self.generation.wrapping_add(1);
                    let _ = self.cancel.send(self.generation);
                    self.evidence = None;
                    self.status = Status::Editing;
                }
                task.map(Message::from)
            }
            Message::ExternalEdited(value) => {
                if self.imported_source.is_some() {
                    return Task::none();
                }
                self.cancel();
                self.source_form.edit_external(value);
                self.status = Status::Editing;
                Task::none()
            }
            Message::InternalEdited(value) => {
                if self.imported_source.is_some() {
                    return Task::none();
                }
                self.cancel();
                self.source_form.edit_internal(value);
                self.status = Status::Editing;
                Task::none()
            }
            Message::TargetSelected(target) => {
                if self.imported_source.is_some() {
                    return Task::none();
                }
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
                if self.source_form.source() == Source::Hardware
                    && self.source_form.hardware_account().is_none()
                {
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
                let imported = self.imported_source.is_some();
                Task::perform(
                    async move {
                        let result = if imported {
                            foreign_split_inventory::scan_imported(
                                client, plan, generation, receiver,
                            )
                            .await
                        } else {
                            foreign_split_inventory::scan(client, plan, generation, receiver).await
                        };
                        result
                            .map_err(inventory_error_copy)
                            .and_then(|two| ScanEvidence::new(two, external, internal))
                            .map(Arc::new)
                    },
                    move |result| Message::Scanned(result, generation, session_generation),
                )
            }
            Message::Scanned(result, generation, fired_session) => {
                if self.imported_source.is_some() && !matches!(self.status, Status::Scanning) {
                    return Task::none();
                }
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
                    Ok(evidence) if !self.imported_evidence_matches(&evidence) => {
                        self.evidence = None;
                        self.status = Status::Failed(
                            "The scan did not cover every saved receive and change address on both chains. No balance conclusion was made. Scan again."
                                .to_string(),
                        );
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
        if let Some(source) = &self.imported_source {
            let external = ScanDescriptor::parse(Branch::External, &source.external)?;
            let internal = ScanDescriptor::parse(Branch::Internal, &source.internal)?;
            let plan = ScanPlan {
                chain: ChainId::BitcoinBlake2b,
                branches: vec![external, internal]
                    .into_iter()
                    .map(|descriptor| BranchRange {
                        descriptor,
                        start: 0,
                        end_exclusive: source.address_window,
                    })
                    .collect(),
                gap: DEFAULT_GAP,
            };
            foreign_scan::validate_imported_plan(&plan)?;
            return Ok(plan);
        }
        if self.source_form.source() == Source::Hardware {
            let account = self
                .source_form
                .hardware_account()
                .ok_or(ScanError::Descriptor)?;
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
        let external = ScanDescriptor::parse(Branch::External, self.source_form.external().trim())?;
        let external_end = external.end_exclusive(DEFAULT_RANGE_END);
        let mut branches = vec![BranchRange {
            descriptor: external,
            start: 0,
            end_exclusive: external_end,
        }];
        if !self.source_form.internal().trim().is_empty() {
            let internal =
                ScanDescriptor::parse(Branch::Internal, self.source_form.internal().trim())?;
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

    fn imported_evidence_matches(&self, evidence: &ScanEvidence) -> bool {
        let Some(source) = &self.imported_source else {
            return true;
        };
        let Ok(plan) = self.plan() else {
            return false;
        };
        if evidence.report.chain() != ChainId::BitcoinBlake2b
            || evidence.bitcoin.chain() != ChainId::Bitcoin
            || evidence.external.canonical() != plan.branches[0].descriptor.canonical()
            || evidence.internal.as_ref().map(ScanDescriptor::canonical)
                != Some(plan.branches[1].descriptor.canonical())
        {
            return false;
        }
        [Branch::External, Branch::Internal]
            .iter()
            .copied()
            .all(|branch| {
                [
                    evidence.report.coverage(branch),
                    evidence.bitcoin.coverage(branch),
                ]
                .iter()
                .copied()
                .all(|coverage| {
                    coverage.is_some_and(|walk| {
                        walk.start == 0 && walk.end_exclusive >= source.address_window
                    })
                })
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
        InventoryError::PrevoutMismatch(_) | InventoryError::Inconsistent(_) => "The two chains disagree about a pre-fork coin. The inventory was refused. No balance conclusion was made.".to_string(),
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
        ScanError::RateLimited(delay) => format!("The indexer is rate limited. Try again after its cooldown (about {} minutes).", delay.as_secs().div_ceil(60)),
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
    let imported = panel.imported_source.as_ref();
    let header = if imported.is_some() {
        Column::new()
            .spacing(6)
            .push(h3("Claim BTCB2"))
            .push(
                p1_regular("Scan your wallet on both chains, then review any BTCB2 found. Your paired Bitcoin Blake2b wallet uses the same descriptors and original signers. Its Cube has no Spark wallet.")
                    .style(theme::text::secondary),
            )
    } else {
        Column::new()
            .spacing(6)
            .push(h3("Split a Bitcoin wallet"))
            .push(
                p1_regular("Find BTCB2 held by a Sparrow, Electrum, Coldcard or other non-Cube Bitcoin wallet. This step scans public keys on Bitcoin and Bitcoin Blake2b only; it cannot sign or move funds.")
                    .style(theme::text::secondary),
            )
    };

    let target = iced::widget::pick_list(
        panel.targets.clone(),
        panel.selected.clone(),
        Message::TargetSelected,
    )
    .placeholder("Choose a Bitcoin Blake2b Cube")
    .width(Length::Fill);

    let mut form = if imported.is_some() {
        Column::new().spacing(12)
            .push(p1_bold("Paired Bitcoin Blake2b Cube"))
            .push(p1_regular(panel.selected.as_ref().map(|target| target.name.as_str()).unwrap_or("Preparing paired Cube…")))
            .push(caption("Same wallet descriptors and signers. Bitcoin and Bitcoin Blake2b history are stored separately.").style(theme::text::secondary))
    } else {
        Column::new()
            .spacing(12)
            .push(p1_bold("Destination Cube"))
            .push(target)
    };
    form = if let Some(source) = imported {
        form.push(p1_bold("Original Bitcoin wallet"))
            .push(p1_regular(&source.name))
            .push(caption(format!(
                "Selected wallet · receive and change addresses 0–{} will be checked on both chains",
                source.address_window.saturating_sub(1)
            )).style(theme::text::secondary))
    } else {
        form.push(
            wallet_source_form::view(&panel.source_form, &SourceFormCopy::split())
                .map(Message::from),
        )
    };

    let status: Element<Message> = match &panel.status {
        Status::Editing => caption(if imported.is_some() {
            "We’ll check both chains before showing what may be available to claim."
        } else {
            "Ready to scan. An incomplete scan is always an error, never a zero balance."
        })
        .style(theme::text::secondary)
        .into(),
        Status::Scanning => p1_regular(if imported.is_some() {
            "Checking Bitcoin and Bitcoin Blake2b history…"
        } else {
            "Scanning with fresh, authenticated chain observations…"
        })
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
                "Post-fork funding candidates: {} seen only on Bitcoin · {} seen only on Bitcoin Blake2b. These still need verification before a split. {}",
                summary.inventory.bitcoin_only_candidates,
                summary.inventory.btcb2_only_candidates,
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
                } else if imported.is_some() && summary.inventory.splittable == 0 {
                    "No confirmed pre-fork coins remain unspent on both chains for this claim."
                } else {
                    "Continue to unlock the destination Cube and review bounded sweep economics. Spending remains locked."
                })
                    .style(theme::text::warning),
            )
            .into(),
    };

    let scanning = matches!(panel.status, Status::Scanning);
    let reviewable = matches!(&panel.status, Status::Complete(summary) if summary.reviewable() && (imported.is_none() || summary.inventory.splittable > 0));
    let actions = Row::new()
        .spacing(10)
        .align_y(Alignment::Center)
        .push(
            button::primary(
                None,
                if scanning {
                    "Scanning…"
                } else {
                    if imported.is_some() {
                        "Scan selected wallet"
                    } else {
                        "Scan wallet"
                    }
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
        ScanEvidence::new(
            TwoChainScan {
                btcb2: report,
                bitcoin,
                inventory,
            },
            external,
            None,
        )
        .unwrap()
    }

    /// Both reports joined against an empty, fully covered Bitcoin report.
    fn empty_bitcoin_scan(report: foreign_scan::ScanReport) -> TwoChainScan {
        let bitcoin = foreign_scan::ScanReport::for_test(
            ChainId::Bitcoin,
            report.generation(),
            BlockHash::from_byte_array([8; 32]),
            Vec::new(),
        )
        .with_coverage(walk());
        let inventory = SplitInventory::join(&report, &bitcoin, report.generation(), true).unwrap();
        TwoChainScan {
            btcb2: report,
            bitcoin,
            inventory,
        }
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

    fn imported_source(window: u32) -> ImportedClaimSource {
        ImportedClaimSource {
            cube_id: "bitcoin-wallet-1".into(),
            name: "My Bitcoin wallet".into(),
            external: ranged_descriptor(0),
            internal: ranged_descriptor(1),
            address_window: window,
        }
    }

    #[test]
    fn imported_source_is_prefilled_and_cannot_be_edited_by_messages() {
        let _guard = crate::app::session::test_guard();
        let mut panel = SplitWalletPanel::new();
        let source = imported_source(240);
        let paired_target = target();
        let other_target = TargetCube {
            id: "unrelated-cube".into(),
            name: "Unrelated BTCB2 Cube".into(),
        };
        panel.set_targets(vec![paired_target.clone(), other_target.clone()]);
        panel.set_imported_source(source.clone()).unwrap();
        assert_eq!(panel.imported_source().unwrap().cube_id, source.cube_id);
        assert_eq!(panel.source_form.external(), source.external);
        assert_eq!(panel.source_form.internal(), source.internal);
        for message in [
            Message::ExternalEdited("other".into()),
            Message::InternalEdited("other".into()),
            Message::SourceSelected(Source::Hardware),
            Message::TargetSelected(other_target),
            Message::Hardware(HardwareMessage::AccountEdited("1".into())),
        ] {
            let generation = panel.generation;
            let _ = panel.update(message, None, 0);
            assert_eq!(panel.generation, generation);
            assert_eq!(panel.source_form.external(), source.external);
            assert_eq!(panel.source_form.internal(), source.internal);
            assert_eq!(panel.source_form.source(), Source::Descriptor);
            assert_eq!(panel.imported_source().unwrap().cube_id, source.cube_id);
            assert_eq!(panel.selected_target_id(), Some(paired_target.id.as_str()));
        }
        assert!(!panel.polls_hardware());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn imported_view_names_the_source_and_action_without_edit_controls() {
        use iced::advanced::{
            layout,
            renderer::Headless,
            widget::{Id, Operation, Tree},
            Layout,
        };
        #[derive(Default)]
        struct Labels(Vec<String>);
        impl Operation for Labels {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn text(&mut self, _: Option<&Id>, _: iced::Rectangle, text: &str) {
                self.0.push(text.to_owned());
            }
        }

        let renderer = <iced::Renderer as Headless>::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        )
        .await
        .expect("software renderer for the claim view");
        let _guard = crate::app::session::test_guard();
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        panel.set_imported_source(imported_source(240)).unwrap();
        let mut element = view(&panel);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(iced::Size::ZERO, iced::Size::new(760.0, 1400.0)),
        );
        let mut labels = Labels::default();
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
        let labels = labels.0.join(" | ");
        for visible in [
            "Claim BTCB2",
            "Original Bitcoin wallet",
            "My Bitcoin wallet",
            "receive and change addresses 0–239",
            "Paired Bitcoin Blake2b Cube",
            "Scan selected wallet",
        ] {
            assert!(labels.contains(visible), "missing {}: {}", visible, labels);
        }
        for hidden in [
            "Public descriptor",
            "Hardware wallet",
            "External / receive descriptor",
            "Internal / change descriptor",
        ] {
            assert!(
                !labels.contains(hidden),
                "unexpected {}: {}",
                hidden,
                labels
            );
        }
    }

    #[test]
    fn imported_source_replacement_invalidates_scan_and_scans_full_window() {
        let _guard = crate::app::session::test_guard();
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        panel.set_imported_source(imported_source(240)).unwrap();
        let plan = panel.plan().unwrap();
        assert_eq!(plan.branches.len(), 2);
        assert_eq!(plan.branches[0].descriptor.branch(), Branch::External);
        assert_eq!(plan.branches[1].descriptor.branch(), Branch::Internal);
        assert!(plan
            .branches
            .iter()
            .all(|range| range.start == 0 && range.end_exclusive == 240));
        panel.status = Status::Scanning;
        let stale_generation = panel.generation;
        panel.set_imported_source(imported_source(300)).unwrap();
        assert_eq!(panel.generation, stale_generation + 1);
        assert!(panel.evidence.is_none());
        assert_eq!(panel.status(), &Status::Editing);
        let _ = panel.update(
            Message::Scanned(Err("stale".into()), stale_generation, 0),
            None,
            0,
        );
        assert_eq!(panel.status(), &Status::Editing);
        assert_eq!(panel.plan().unwrap().branches[0].end_exclusive, 300);
        assert!(panel.set_imported_source(imported_source(0)).is_err());
        assert_eq!(panel.plan().unwrap().branches[0].end_exclusive, 300);
        assert!(panel.set_imported_source(imported_source(10_001)).is_err());
    }

    #[test]
    fn imported_evidence_requires_both_branches_full_coverage_on_both_chains() {
        let _guard = crate::app::session::test_guard();
        let source = imported_source(240);
        let mut panel = SplitWalletPanel::new();
        panel.set_imported_source(source.clone()).unwrap();
        let generation = panel.generation;
        let coverage = |internal_end| {
            vec![
                foreign_scan::BranchCoverage {
                    branch: Branch::External,
                    start: 0,
                    end_exclusive: 240,
                    last_used: None,
                },
                foreign_scan::BranchCoverage {
                    branch: Branch::Internal,
                    start: 0,
                    end_exclusive: internal_end,
                    last_used: None,
                },
            ]
        };
        let report = foreign_scan::ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            generation,
            BlockHash::from_byte_array([9; 32]),
            Vec::new(),
        )
        .with_fork_height(Some(100))
        .with_coverage(coverage(240));
        let bitcoin = foreign_scan::ScanReport::for_test(
            ChainId::Bitcoin,
            generation,
            BlockHash::from_byte_array([8; 32]),
            Vec::new(),
        )
        .with_coverage(coverage(239));
        let inventory = SplitInventory::join(&report, &bitcoin, generation, true).unwrap();
        let external = ScanDescriptor::parse(Branch::External, &source.external).unwrap();
        let internal = ScanDescriptor::parse(Branch::Internal, &source.internal).unwrap();
        let short = Arc::new(
            ScanEvidence::new(
                TwoChainScan {
                    btcb2: report,
                    bitcoin,
                    inventory,
                },
                external,
                Some(internal),
            )
            .unwrap(),
        );
        assert!(!panel.imported_evidence_matches(&short));
        let short_for_handoff = short.clone();
        panel.status = Status::Scanning;
        let _ = panel.update(Message::Scanned(Ok(short), generation, 0), None, 0);
        assert!(
            matches!(panel.status(), Status::Failed(copy) if copy.contains("did not cover every saved receive and change address"))
        );
        assert!(panel.evidence.is_none());
        panel.set_targets(vec![target()]);
        panel.generation = generation;
        panel.status = Status::Complete(short_for_handoff.summary.clone());
        panel.evidence = Some(short_for_handoff);
        assert!(panel
            .take_handoff(ChainId::BitcoinBlake2b, 0, &CoincubeClient::new())
            .is_none());
    }

    #[test]
    fn refuses_scan_without_destination_cube() {
        let _guard = crate::app::session::test_guard();
        let mut panel = SplitWalletPanel::new();
        panel.source_form.edit_external(FIXED_DESCRIPTOR.into());
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
            Message::SourceSelected(Source::Hardware),
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
    fn reselecting_source_keeps_current_scan_evidence() {
        let _guard = crate::app::session::test_guard();
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        let generation = panel.generation;
        let evidence = completed_evidence(generation);
        panel.status = Status::Complete(evidence.summary.clone());
        panel.evidence = Some(evidence);

        let _ = panel.update(Message::SourceSelected(Source::Descriptor), None, 7);
        assert_eq!(panel.generation, generation);
        assert!(matches!(panel.status(), Status::Complete(_)));
        assert!(panel.evidence.is_some());
    }

    #[test]
    fn plan_uses_single_index_for_fixed_descriptors_and_bound_for_wildcards() {
        let _guard = crate::app::session::test_guard();
        let mut panel = SplitWalletPanel::new();
        panel.source_form.edit_external(FIXED_DESCRIPTOR.into());
        panel.source_form.edit_internal(FIXED_DESCRIPTOR.into());
        let fixed = panel.plan().unwrap();
        assert_eq!(fixed.branches[0].end_exclusive, 1);
        assert_eq!(fixed.branches[1].end_exclusive, 1);

        panel.source_form.edit_external(ranged_descriptor(0));
        panel.source_form.edit_internal(ranged_descriptor(1));
        let wildcard = panel.plan().unwrap();
        assert_eq!(wildcard.branches[0].end_exclusive, DEFAULT_RANGE_END);
        assert_eq!(wildcard.branches[1].end_exclusive, DEFAULT_RANGE_END);
    }

    #[test]
    fn parse_failure_is_explicit_and_does_not_start_scan() {
        let _guard = crate::app::session::test_guard();
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        panel.source_form.edit_external("not a descriptor".into());
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
        // #568 B1a: the Bitcoin scan and the joined inventory travel too.
        assert_eq!(intent.bitcoin_report.chain(), ChainId::Bitcoin);
        assert_eq!(intent.bitcoin_report.generation(), 17);
        assert_eq!(
            intent.bitcoin_report.tip(),
            BlockHash::from_byte_array([8; 32])
        );
        assert_eq!(intent.inventory.generation(), 17);
        assert_eq!(
            intent.inventory.bitcoin_tip(),
            BlockHash::from_byte_array([8; 32])
        );
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
            let output = TxOut {
                value: Amount::from_sat(sats),
                script_pubkey: external.script(0).unwrap(),
            };
            let previous = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![TxIn::default()],
                output: vec![output.clone(); vout as usize + 1],
            };
            foreign_scan::DiscoveredCoin {
                branch: Branch::External,
                index: 0,
                outpoint: OutPoint::new(previous.compute_txid(), vout),
                output,
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
                btcb2_only_candidates: 1,
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

        panel
            .source_form
            .hardware_mut()
            .ready_for_test(hardware_account());
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
        // A2: the Scan path mirrors the hardware plan onto Bitcoin unchanged,
        // so both chains are scanned for the same account.
        let mirror = foreign_split_inventory::bitcoin_plan(&plan).unwrap();
        assert_eq!(mirror.chain, ChainId::Bitcoin);
        assert_eq!(mirror.gap, plan.gap);
        assert_eq!(mirror.branches.len(), 2);
        for (a, b) in plan.branches.iter().zip(&mirror.branches) {
            assert_eq!(a.descriptor.canonical(), b.descriptor.canonical());
            assert_eq!(a.descriptor.branch(), b.descriptor.branch());
            assert_eq!((a.start, a.end_exclusive), (b.start, b.end_exclusive));
        }

        // Cancel and switching source clear the read account and stop polling.
        panel.cancel();
        assert!(panel.source_form.hardware_account().is_none());
        panel
            .source_form
            .hardware_mut()
            .ready_for_test(hardware_account());
        let _ = panel.update(Message::SourceSelected(Source::Descriptor), None, 0);
        assert!(panel.source_form.hardware_account().is_none());
        assert!(!panel.polls_hardware());
    }

    #[test]
    fn split_hardware_change_invalidates_completed_scan_evidence() {
        // cancel() clears the process-wide Split intent slot.
        let _guard = crate::app::session::test_guard();
        let report = foreign_scan::ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            5,
            BlockHash::from_byte_array([3; 32]),
            Vec::new(),
        )
        .with_fork_height(Some(100));
        let evidence = evidence(report, Vec::new());
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        panel.source_form.select_source(Source::Hardware);
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
        assert!(ScanEvidence::new(empty_bitcoin_scan(report.clone()), external, None).is_err());

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
        let evidence = ScanEvidence::new(empty_bitcoin_scan(report), tr, None).unwrap();
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

    /// #568 U6 (replaces the A1 "discovery only" pin): the PSBT-file route
    /// exists for every non-`tr` shape; the in-app routes (a connected device,
    /// the unified seed) only when every key carries its origin, so a signer
    /// can be matched to it. `tr` has no route, and no shape grants Claim
    /// authority.
    #[test]
    fn signing_routes_need_origins() {
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
        let expected = |taproot: bool, origins: bool| Capabilities {
            scan: true,
            signing: SigningRoutes {
                psbt_file: !taproot,
                in_app_hardware: !taproot && origins,
                seed_unified: !taproot && origins,
            },
            claim_authorization: false,
        };

        // The PR 8 discovery shapes with and without origin metadata. A
        // hardware-exported account xpub is public material here: the route
        // says a device could be matched, never that one was connected,
        // accepted the PSBT, or granted Claim authority.
        let a = "[d34db33f/84h/0h/0h]";
        let m = "[d34db33f/48h/0h/0h/2h]";
        let n = "[0badc0de/48h/0h/0h/2h]";
        for (descriptor, origins) in [
            (format!("wpkh({first}/0/*)"), false),
            (format!("wpkh({a}{first}/0/*)"), true),
            (format!("sh(wpkh({first}/0/*))"), false),
            (format!("sh(wpkh([d34db33f/49h/0h/0h]{first}/0/*))"), true),
            (format!("pkh({first}/0/*)"), false),
            (format!("pkh([d34db33f/44h/0h/0h]{first}/1/*)"), true),
            (
                format!("wsh(sortedmulti(2,{first}/0/*,{second}/0/*))"),
                false,
            ),
            // One multisig key without its origin is enough to refuse.
            (
                format!("wsh(sortedmulti(2,{m}{first}/0/*,{second}/0/*))"),
                false,
            ),
            (
                format!("wsh(sortedmulti(2,{m}{first}/0/*,{n}{second}/0/*))"),
                true,
            ),
            (
                format!("wsh(multi(2,{m}{first}/0/*,{n}{second}/0/*))"),
                true,
            ),
            (format!("tr({first}/0/*)"), false),
            (format!("tr([d34db33f/86h/0h/0h]{first}/0/*)"), true),
            // #653 F2: a fixed (non-ranged) xpub keeps no in-app route even
            // with its origin (P7).
            (format!("wpkh({a}{first}/0/5)"), false),
            (
                format!("wsh(multi(2,{m}{first}/0/5,{n}{second}/0/*))"),
                false,
            ),
        ] {
            let parsed =
                ScanDescriptor::parse(Branch::External, &descriptor).unwrap_or_else(|_| {
                    panic!("supported discovery descriptor refused: {}", descriptor)
                });
            assert_eq!(
                parsed.capabilities(),
                expected(descriptor.starts_with("tr("), origins),
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

        // A2: every two-chain inventory refusal keeps the same semantics and
        // names the chain whose scan was incomplete.
        let outpoint = coincube_core::miniscript::bitcoin::OutPoint::null();
        for error in [
            InventoryError::ForkHeightUnknown,
            InventoryError::PrevoutMismatch(outpoint),
            InventoryError::Inconsistent(outpoint),
            InventoryError::Coverage(ChainId::Bitcoin, outpoint),
        ] {
            let copy = inventory_error_copy(error);
            assert!(
                copy.contains("No balance conclusion"),
                "{:?}: {}",
                error,
                copy
            );
        }
        for (chain, prefix) in [
            (ChainId::Bitcoin, "Bitcoin chain:"),
            (ChainId::BitcoinBlake2b, "Bitcoin Blake2b chain:"),
        ] {
            let copy = inventory_error_copy(InventoryError::Scan(chain, ScanError::Deadline));
            assert!(
                copy.starts_with(prefix) && copy.contains("bounded"),
                "{}",
                copy
            );
        }
    }

    /// Completed, authenticated (empty) evidence for `generation`.
    fn completed_evidence(generation: u64) -> Arc<ScanEvidence> {
        let report = foreign_scan::ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            generation,
            BlockHash::from_byte_array([9; 32]),
            Vec::new(),
        )
        .with_fork_height(Some(100));
        Arc::new(evidence(report, Vec::new()))
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
