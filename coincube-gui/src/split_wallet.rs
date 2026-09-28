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
use iced::{widget::text_input, Alignment, Length, Task};
use tokio::sync::watch;

use crate::{
    chain::ChainId,
    services::{
        coincube::CoincubeClient,
        foreign_scan::{self, Branch, BranchRange, ScanDescriptor, ScanError, ScanPlan},
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
    pub tip: String,
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
    Scanned(Result<ScanSummary, String>, u64, u64),
    Cancel,
}

pub struct SplitWalletPanel {
    targets: Vec<TargetCube>,
    selected: Option<TargetCube>,
    external: String,
    internal: String,
    status: Status,
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
        self.generation = self.generation.wrapping_add(1);
        let _ = self.cancel.send(self.generation);
        if matches!(self.status, Status::Scanning) {
            self.status = Status::Editing;
        }
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
                self.status = Status::Scanning;
                Task::perform(
                    async move {
                        foreign_scan::scan(client, plan, generation, receiver)
                            .await
                            .map(|report| ScanSummary {
                                generation: report.generation(),
                                addresses: report.addresses_scanned(),
                                coins: report.coins().len(),
                                confirmed: report
                                    .coins()
                                    .iter()
                                    .filter(|coin| coin.confirmed)
                                    .count(),
                                sats: report
                                    .coins()
                                    .iter()
                                    .map(|coin| coin.output.value.to_sat())
                                    .sum(),
                                tip: report.tip().to_string(),
                            })
                            .map_err(scan_error_copy)
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
                self.status = match result {
                    Ok(summary) => Status::Complete(summary),
                    Err(error) => Status::Failed(error),
                };
                Task::none()
            }
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
            p1_regular("Find BTCB2 held by a Sparrow, Electrum, Coldcard or other non-Cube Bitcoin wallet. This step scans public descriptors only; it cannot sign or move funds.")
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
            caption("Supported for discovery: pkh, sh(wpkh), wpkh, wsh(multi/sortedmulti), and tr key-path. Only public mainnet descriptors are accepted. The bounded scan uses a gap of 20 and checks at most 100 addresses per branch.")
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
            .push(caption(format!(
                "{} addresses checked at BTCB2 tip {}",
                summary.addresses, summary.tip
            )).style(theme::text::secondary))
            .push(
                p1_regular("Spending is still locked. The next slice will bind these exact outpoints to the poison-split PSBT and shared Claim confirmation/reorg gates.")
                    .style(theme::text::warning),
            )
            .into(),
    };

    let scanning = matches!(panel.status, Status::Scanning);
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
        secp256k1::Secp256k1,
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

    fn completed_summary(generation: u64) -> ScanSummary {
        ScanSummary {
            generation,
            addresses: 1,
            coins: 1,
            confirmed: 1,
            sats: 1,
            tip: "tip-a".into(),
        }
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
                Message::Scanned(Ok(completed_summary(stale_generation)), stale_generation, 7),
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
    fn every_supported_source_remains_discovery_only() {
        use crate::services::foreign_scan::Capabilities;

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
        let scan_only = Capabilities {
            scan: true,
            unified_signing: false,
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
            assert_eq!(parsed.capabilities(), scan_only, "{}", descriptor);
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

    #[test]
    fn scan_result_is_evidence_only_and_never_a_signing_transition() {
        let mut panel = SplitWalletPanel::new();
        panel.set_targets(vec![target()]);
        let generation = panel.generation;

        let _ = panel.update(
            Message::Scanned(Ok(completed_summary(generation)), generation, 9),
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
