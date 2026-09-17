//! Replay-protection model for a Bitcoin Blake2b Vault spend (`#276` I3/I4,
//! desktop plan PR 6 with the launch-GA corrections).
//!
//! Everything here is keyed on [`ChainId::is_blake2b`]. On a Bitcoin-family
//! Cube nothing in this module runs: signing, merging and readiness are the
//! prior code paths byte for byte (pinned by `bitcoin_paths_are_unchanged`).
//!
//! The replay statement a spend shows is derived from **one** source: the
//! core finaliser's per-input report — verified signatures that actually make
//! up the witness — never from which signers were asked, what the PSBT claims,
//! or a capability flag (`#276` correction 1). The four states:
//!
//! - *Replay protected*: every input's witness carries at least one verified
//!   unified signature. Legacy signatures alongside are fine.
//! - *Replayable*: at least one input would be finalised from legacy
//!   signatures alone. Amber; broadcasting needs the explicit acknowledgement
//!   [`REPLAYABLE_ACKNOWLEDGEMENT`] — **unless** one of those inputs spends a
//!   deposit Connect has positively confirmed also exists on Bitcoin (`#276`
//!   I13). Such an input *requires* a replay-capable signature (or, once Lane
//!   B1.5 exists, a poison split first); no acknowledgement clears it
//!   ([`blocked_entangled_inputs`]). An input whose entanglement is *Unknown*
//!   is amber and acknowledgeable, never blocked — a lookup that has not
//!   happened is not evidence either way.
//! - *Split — cannot replay*: positive poison-split evidence for every input.
//!   Wired here, unreachable by construction until Lane B1.5 defines the
//!   evidence ([`SplitEvidence`] has no values yet).
//! - *Unknown / not yet checked*: no signatures, not enough of them, or the
//!   verifier refused the PSBT.

use std::collections::BTreeSet;

use coincube_core::{
    descriptors::{CoincubeDescError, CoincubeDescriptor, PartialSpendInfo},
    miniscript::bitcoin::{ecdsa, psbt::Psbt, secp256k1, sighash::EcdsaSighashType, Txid},
    psbt_unified::{unified_signatures, UnifiedPsbt},
    unified_finalize::{finalize_p2wsh_all_unified, UnifiedFinalizeError},
};

use crate::{chain::ChainId, services::entangled::Entanglement};

/// The acknowledgement a user gives before broadcasting a replayable spend
/// (`#276` I4). Verbatim from the lane brief.
pub const REPLAYABLE_ACKNOWLEDGEMENT: &str = "I understand this can also spend my Bitcoin";

/// Why a spend's replay status is not known yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnknownReason {
    /// No signature of any kind has been collected.
    NotYetChecked,
    /// Signatures exist but at least one input cannot be satisfied yet.
    Incomplete,
    /// The unified verifier or finaliser refused the PSBT; the text is the
    /// typed error's `Display`.
    Refused(String),
}

/// Positive poison-split evidence for every input of a spend. Lane B1.5
/// records it (step 1 confirmed with depth, `split_completed_at_height` on
/// both Cubes); until then this type has no values, so
/// [`ReplayStatus::Split`] cannot be produced by any code path — see
/// `split_is_unreachable_by_construction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitEvidence {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayStatus {
    Unknown(UnknownReason),
    Protected,
    /// Inputs whose witness would hold legacy signatures only.
    Replayable {
        inputs: Vec<usize>,
    },
    Split,
}

impl ReplayStatus {
    /// The only constructor of [`Self::Split`]. Uninhabited argument: no
    /// caller can reach this until B1.5 gives [`SplitEvidence`] a value.
    pub fn from_split_evidence(evidence: SplitEvidence) -> Self {
        match evidence {}
    }

    /// Whether the finaliser would produce a transaction from the current
    /// signatures — the BTCB2 notion of "ready to broadcast".
    pub fn is_finalisable(&self) -> bool {
        matches!(
            self,
            Self::Protected | Self::Replayable { .. } | Self::Split
        )
    }

    /// Whether broadcasting needs the user's replay acknowledgement.
    pub fn needs_acknowledgement(&self) -> bool {
        matches!(self, Self::Replayable { .. })
    }
}

/// Derive the replay status of a Bitcoin Blake2b spend from its signatures.
///
/// `split` is the poison-split evidence, which wins over signatures when
/// present (a split step-2 sweep is legacy-signed and *cannot* replay because
/// its inputs are already spent on Bitcoin). It is always `None` today.
///
/// Callers must gate on the chain: this is meaningless — and never shown —
/// for a Bitcoin-family Cube.
pub fn replay_status(
    psbt: &Psbt,
    secp: &secp256k1::Secp256k1<impl secp256k1::Verification>,
    split: Option<SplitEvidence>,
) -> ReplayStatus {
    if let Some(evidence) = split {
        return ReplayStatus::from_split_evidence(evidence);
    }
    let unified = match UnifiedPsbt::from_psbt(psbt.clone()) {
        Ok(unified) => unified,
        Err(e) => return ReplayStatus::Unknown(UnknownReason::Refused(e.to_string())),
    };
    let has_any_signature = unified.psbt().inputs.iter().any(|input| {
        !input.partial_sigs.is_empty()
            || !input.tap_script_sigs.is_empty()
            || input.tap_key_sig.is_some()
    }) || unified_signatures(&unified)
        .map(|records| !records.is_empty())
        .unwrap_or(false);
    if !has_any_signature {
        return ReplayStatus::Unknown(UnknownReason::NotYetChecked);
    }
    match finalize_p2wsh_all_unified(&unified, secp) {
        Ok(finalized) => {
            let inputs: Vec<usize> = finalized
                .inputs
                .iter()
                .enumerate()
                .filter(|(_, report)| !report.replay_protected())
                .map(|(index, _)| index)
                .collect();
            if inputs.is_empty() {
                ReplayStatus::Protected
            } else {
                ReplayStatus::Replayable { inputs }
            }
        }
        Err(UnifiedFinalizeError::Unsatisfiable { .. }) => {
            ReplayStatus::Unknown(UnknownReason::Incomplete)
        }
        Err(e) => ReplayStatus::Unknown(UnknownReason::Refused(e.to_string())),
    }
}

/// Why a Bitcoin Blake2b PSBT must not be handed to any signer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchRefused {
    /// An input asks for, or already carries, an `ANYONECANPAY` sighash.
    /// `ANYONECANPAY` never occurs in a Vault flow, and on Bitcoin Blake2b it
    /// is refused outright rather than warned about (a unified `ANYONECANPAY`
    /// would commit to fewer inputs than the replay model reasons over).
    AnyoneCanPay { input: usize, sighash: u32 },
    /// A reserved unified record (`coincube`/0 proprietary entry) the adapter
    /// rejects — wrong trailing sighash byte (`0xa1` included), non-strict
    /// DER, a key that also has a legacy signature. Such a PSBT would be
    /// refused by every merge and by the finaliser anyway; refusing it here
    /// keeps "before any signer is dispatched" true, so no device or phone is
    /// prompted for a signature that would be thrown away.
    MalformedUnifiedRecord(String),
    /// An input carries Taproot signature data (`tap_key_sig` or
    /// `tap_script_sigs`). A Bitcoin Blake2b Vault is native P2WSH, so this
    /// is not a PSBT for it; and Taproot signatures carry their own sighash
    /// byte, which the ECDSA walk below cannot vet.
    TaprootSignatureData { input: usize },
}

impl std::fmt::Display for DispatchRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AnyoneCanPay { input, sighash } => write!(
                f,
                "input {} asks for sighash 0x{:02x} (ANYONECANPAY), which a Bitcoin Blake2b \
                 Vault spend never uses; refusing to sign",
                input, sighash
            ),
            Self::MalformedUnifiedRecord(reason) => write!(
                f,
                "the PSBT carries a unified signature record no signer could add to: {}; \
                 refusing to sign",
                reason
            ),
            Self::TaprootSignatureData { input } => write!(
                f,
                "input {} carries Taproot signature data, which a Bitcoin Blake2b Vault spend \
                 never has; refusing to sign",
                input
            ),
        }
    }
}

/// Refuse, before any signer — local, device or Keychain — is dispatched on
/// Bitcoin Blake2b, a PSBT that asks for or carries an `ANYONECANPAY`
/// sighash on any input, that carries a reserved unified record the adapter
/// rejects, or that carries Taproot signature data at all (a Blake2b Vault
/// is P2WSH; Taproot signatures bring their own sighash byte).
pub fn refuse_before_dispatch(psbt: &Psbt) -> Result<(), DispatchRefused> {
    // The specific refusals first, so the user is told "ANYONECANPAY" or
    // "Taproot" rather than the adapter's generic wording; then the adapter's
    // own validation — the same check every merge and the finaliser apply —
    // which also covers the reserved records' sighash byte and the sighash
    // request rule the walk below does not restate.
    const ANYONECANPAY: u32 = 0x80;
    for (index, input) in psbt.inputs.iter().enumerate() {
        if input.tap_key_sig.is_some() || !input.tap_script_sigs.is_empty() {
            return Err(DispatchRefused::TaprootSignatureData { input: index });
        }
        if let Some(sighash) = input.sighash_type {
            let raw = sighash.to_u32();
            if raw & ANYONECANPAY != 0 {
                return Err(DispatchRefused::AnyoneCanPay {
                    input: index,
                    sighash: raw,
                });
            }
        }
        for signature in input.partial_sigs.values() {
            let raw = signature.sighash_type.to_u32();
            if raw & ANYONECANPAY != 0 {
                return Err(DispatchRefused::AnyoneCanPay {
                    input: index,
                    sighash: raw,
                });
            }
        }
    }
    UnifiedPsbt::from_psbt(psbt.clone())
        .map_err(|e| DispatchRefused::MalformedUnifiedRecord(e.to_string()))?;
    Ok(())
}

/// `CoincubeDescriptor::partial_spend_info` keyed on the chain.
///
/// The descriptor analysis reads `partial_sigs` only, so on Bitcoin Blake2b a
/// unified signature — stored in the proprietary records — would not count
/// toward the path threshold and the picker would never show its signer as
/// done. For BTCB2 the analysis runs over a **counting projection**: a clone
/// of the PSBT where each unified record is mirrored into `partial_sigs` as
/// its DER part with `SIGHASH_ALL`. The projection exists for this call only;
/// it is never persisted, exported or signed (a key with both encodings is
/// exactly what the adapter refuses). On every other chain this is the
/// unchanged analysis of the unchanged PSBT.
pub fn spend_info_for_chain(
    chain: ChainId,
    descriptor: &CoincubeDescriptor,
    psbt: &Psbt,
) -> Result<PartialSpendInfo, CoincubeDescError> {
    if !chain.is_blake2b() {
        return descriptor.partial_spend_info(psbt);
    }
    descriptor.partial_spend_info(&counting_projection(psbt))
}

/// See [`spend_info_for_chain`]. Falls back to the PSBT as-is when the
/// proprietary records do not parse (the analysis then simply does not count
/// them, which errs toward "not signed yet").
pub(crate) fn counting_projection(psbt: &Psbt) -> Psbt {
    let mut projected = psbt.clone();
    let Ok(unified) = UnifiedPsbt::from_psbt(psbt.clone()) else {
        return projected;
    };
    let Ok(records) = unified_signatures(&unified) else {
        return projected;
    };
    for record in records {
        let Some((_, der)) = record.signature.split_last() else {
            continue;
        };
        let Ok(signature) = secp256k1::ecdsa::Signature::from_der(der) else {
            continue;
        };
        if let Some(input) = projected.inputs.get_mut(record.input_index) {
            input
                .partial_sigs
                .entry(record.public_key)
                .or_insert(ecdsa::Signature {
                    signature,
                    sighash_type: EcdsaSighashType::All,
                });
        }
    }
    projected
}

/// What the spend screen shows and gates on for a Bitcoin Blake2b spend.
/// `None` on every other chain — the screen then renders exactly as before.
///
/// Holds the (verified, therefore not free) status; entanglement of the
/// inputs is a cache lookup and is resolved at render time with
/// [`entangled_inputs`], so a lookup landing after a signature still shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayReview {
    pub status: ReplayStatus,
    /// SHA-256 of the serialised PSBT the status was derived from: the exact
    /// signatures the user is looking at.
    psbt_digest: [u8; 32],
    /// The PSBT digest the user ticked [`REPLAYABLE_ACKNOWLEDGEMENT`] for, if
    /// any. Keyed to **content**, not to the status: a new signature can leave
    /// `Replayable { inputs: [0] }` unchanged while the PSBT changed, and an
    /// acknowledgement must never carry across a signature the user did not
    /// see. Keying to content also means a recompute with no new signature
    /// (every Keychain stream event emits one) keeps the tick.
    acknowledged_for: Option<[u8; 32]>,
}

impl ReplayReview {
    pub fn new(psbt: &Psbt, secp: &secp256k1::Secp256k1<impl secp256k1::Verification>) -> Self {
        Self {
            status: replay_status(psbt, secp, None),
            psbt_digest: psbt_digest(psbt),
            acknowledged_for: None,
        }
    }

    /// Recompute for the current PSBT, carrying an acknowledgement over
    /// **only** if the PSBT is byte-identical to the one it was given for.
    pub fn refreshed(
        &self,
        psbt: &Psbt,
        secp: &secp256k1::Secp256k1<impl secp256k1::Verification>,
    ) -> Self {
        let mut next = Self::new(psbt, secp);
        if self.acknowledged_for == Some(next.psbt_digest) {
            next.acknowledged_for = self.acknowledged_for;
        }
        next
    }

    /// Whether the user has acknowledged *these* signatures.
    pub fn acknowledged(&self) -> bool {
        self.acknowledged_for == Some(self.psbt_digest)
    }

    /// Record or clear the acknowledgement for the PSBT this review is of.
    pub fn set_acknowledged(&mut self, acknowledged: bool) {
        self.acknowledged_for = acknowledged.then_some(self.psbt_digest);
    }

    /// The digest of the PSBT this review describes.
    pub fn psbt_digest(&self) -> [u8; 32] {
        self.psbt_digest
    }

    /// Whether the signatures collected are enough: the finaliser would
    /// produce a transaction **and** no input the user is required to protect
    /// ([`blocked_entangled_inputs`]) is left replayable. This is what closes
    /// the signing picker on BTCB2 — a blocked input keeps it open so the
    /// replay-capable signature can be added. `entangled` is
    /// [`entangled_inputs`] resolved from the cache at the time of the check.
    pub fn signatures_complete(&self, entangled: &[(usize, Entanglement)]) -> bool {
        self.status.is_finalisable() && blocked_entangled_inputs(&self.status, entangled).is_empty()
    }

    /// Whether the spend may be broadcast: [`Self::signatures_complete`], and
    /// acknowledged when it is replayable. The acknowledgement never
    /// substitutes for a required signature on a known-entangled input.
    pub fn broadcast_ready(&self, entangled: &[(usize, Entanglement)]) -> bool {
        self.signatures_complete(entangled)
            && (!self.status.needs_acknowledgement() || self.acknowledged())
    }
}

/// SHA-256 of the serialised PSBT — the identity of a set of signatures.
pub fn psbt_digest(psbt: &Psbt) -> [u8; 32] {
    use coincube_core::miniscript::bitcoin::hashes::{sha256, Hash};
    sha256::Hash::hash(&psbt.serialize()).to_byte_array()
}

/// Inputs (by index) that are replayable **and** spend a deposit Connect has
/// positively confirmed on the twin chain ([`Entanglement::Entangled`]). These
/// require a replay-capable signature in the witness (`#276` I13); the
/// acknowledgement does not apply to them. `Unknown` inputs are never in this
/// set: gating on an unanswered lookup would block every BTCB2 spend until a
/// sync completed, and "never reads as not entangled" asks for the honest
/// amber, not a hard stop.
pub fn blocked_entangled_inputs(
    status: &ReplayStatus,
    entangled: &[(usize, Entanglement)],
) -> Vec<usize> {
    let ReplayStatus::Replayable { inputs } = status else {
        return Vec::new();
    };
    let confirmed: BTreeSet<usize> = entangled
        .iter()
        .filter(|(_, status)| matches!(status, Entanglement::Entangled))
        .map(|(index, _)| *index)
        .collect();
    inputs
        .iter()
        .copied()
        .filter(|index| confirmed.contains(index))
        .collect()
}

/// Inputs (by index) that spend an entangled deposit, or one whose
/// entanglement is not known — `lookup` answers from the cache and returns
/// [`Entanglement::Unknown`] for a txid it has no answer for. These are the
/// inputs the replayable copy names as also existing on Bitcoin; an input
/// known *not* to be entangled is left out. Unknown is deliberately kept: a
/// lookup that has not happened is not evidence of safety.
pub fn entangled_inputs(
    psbt: &Psbt,
    lookup: impl Fn(&Txid) -> Entanglement,
) -> Vec<(usize, Entanglement)> {
    psbt.unsigned_tx
        .input
        .iter()
        .enumerate()
        .filter_map(|(index, txin)| match lookup(&txin.previous_output.txid) {
            Entanglement::NotEntangled => None,
            other => Some((index, other)),
        })
        .collect()
}

/// Whether the Broadcast action is available. One definition for the state
/// (broadcast handler) and the view (button), so they cannot disagree: on a
/// Bitcoin-family Cube it is the path threshold as before (`entangled` is
/// ignored — it is always empty there); on Bitcoin Blake2b it is the
/// finaliser's verdict, the I13 requirement on known-entangled inputs, and
/// the acknowledgement. `entangled` must be resolved from the cache at the
/// point of the check, so a lookup that lands after the last signature
/// tightens the gate instead of being missed by it.
pub fn broadcast_ready(
    path_ready: bool,
    review: Option<&ReplayReview>,
    entangled: &[(usize, Entanglement)],
) -> bool {
    match review {
        None => path_ready,
        Some(review) => review.broadcast_ready(entangled),
    }
}

/// Shown while the spend screen's entanglement re-check is in flight.
pub const CHECKING_COPY: &str =
    "Checking whether these coins also exist on Bitcoin before this can be sent…";

/// Why a spend that looked ready is not — for the toast shown when the
/// Broadcast dialog is closed under the user because the gate moved (a
/// lookup landed, a re-check started, a signature changed). Composed only
/// from the copy the spend screen already shows.
pub fn not_ready_reason(
    review: &ReplayReview,
    entangled: &[(usize, Entanglement)],
    checking: bool,
) -> String {
    if checking {
        return CHECKING_COPY.to_string();
    }
    let blocked = blocked_entangled_inputs(&review.status, entangled);
    if let Some(required) = blocked_entangled_copy(&blocked) {
        return required;
    }
    let (label, _) = pill_copy(&review.status, entangled);
    if review.status.needs_acknowledgement() && !review.acknowledged() {
        return format!("{label}. Tick \"{REPLAYABLE_ACKNOWLEDGEMENT}\" to send.");
    }
    label
}

/// The remedy line shown under the pill when [`blocked_entangled_inputs`] is
/// non-empty: what is true in this build (a replay-capable signature — the
/// Cube key or a Border Wallet key), and that splitting first is not yet
/// available rather than offering a tool that does not exist (Lane B1.5).
pub fn blocked_entangled_copy(blocked: &[usize]) -> Option<String> {
    if blocked.is_empty() {
        return None;
    }
    let list = blocked
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let (noun, verb, pronoun) = if blocked.len() == 1 {
        ("Input", "exists", "it")
    } else {
        ("Inputs", "exist", "them")
    };
    Some(format!(
        "{noun} {list} also {verb} on Bitcoin, so this cannot be sent without a replay-capable \
         signature on {pronoun} (the Cube key or a Border Wallet key). Splitting the coins first \
         is not available in this version."
    ))
}

/// Visual weight of the status pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillTone {
    Success,
    Warning,
    Neutral,
}

/// The pill's label and tone. Pure so the copy is testable without a widget
/// tree; the view only lays it out. `entangled` is [`entangled_inputs`] for
/// the same PSBT.
pub fn pill_copy(status: &ReplayStatus, entangled: &[(usize, Entanglement)]) -> (String, PillTone) {
    match status {
        ReplayStatus::Protected => ("Replay protected".to_string(), PillTone::Success),
        ReplayStatus::Split => ("Split — cannot replay".to_string(), PillTone::Success),
        ReplayStatus::Replayable { inputs } => {
            let blocked: BTreeSet<usize> = blocked_entangled_inputs(status, entangled)
                .into_iter()
                .collect();
            let entangled: BTreeSet<usize> = entangled.iter().map(|(index, _)| *index).collect();
            let list = inputs
                .iter()
                .map(|index| {
                    if blocked.contains(index) {
                        format!("{index} (also exists on Bitcoin — signature required)")
                    } else if entangled.contains(index) {
                        format!("{index} (not yet checked against Bitcoin)")
                    } else {
                        index.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            let noun = if inputs.len() == 1 { "input" } else { "inputs" };
            (
                format!("Replayable — no replay-capable signature on {noun} {list}"),
                PillTone::Warning,
            )
        }
        ReplayStatus::Unknown(UnknownReason::NotYetChecked) => {
            ("Not yet checked".to_string(), PillTone::Neutral)
        }
        ReplayStatus::Unknown(UnknownReason::Incomplete) => (
            "Unknown — not enough signatures to check yet".to_string(),
            PillTone::Neutral,
        ),
        ReplayStatus::Unknown(UnknownReason::Refused(reason)) => {
            (format!("Unknown — {reason}"), PillTone::Neutral)
        }
    }
}

/// Copy for the Bitcoin Cube's recovery and inheritance screens (`#276` I8):
/// coins received before the fork also exist on Bitcoin Blake2b until a BTCB2
/// Cube sweeps them. Copy only, no behaviour: shown when a Bitcoin Cube
/// holds pre-fork coins that no BTCB2 Cube has swept — a fact Lane B1.5
/// records (`split_completed_at_height`). Until it does, nothing supplies
/// `Some(true)`, so the line is never rendered.
pub const BITCOIN_CUBE_UNSWEPT_NOTICE: &str =
    "These coins also exist on Bitcoin Blake2b until swept there.";

/// The I8 line for a Cube, or `None` when it does not apply: only a
/// Bitcoin (mainnet) Cube with known-unswept pre-fork coins shows it.
pub fn bitcoin_cube_unswept_notice(chain: ChainId, unswept: Option<bool>) -> Option<&'static str> {
    match (chain, unswept) {
        (ChainId::Bitcoin, Some(true)) => Some(BITCOIN_CUBE_UNSWEPT_NOTICE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::vault::test_support::unified::{fixture, legacy, unified};
    use coincube_core::miniscript::bitcoin::{ecdsa, sighash::EcdsaSighashType};

    fn secp() -> secp256k1::Secp256k1<secp256k1::VerifyOnly> {
        secp256k1::Secp256k1::verification_only()
    }

    #[test]
    fn split_is_unreachable_by_construction() {
        // `SplitEvidence` has no values: an `Option` of it can only be `None`
        // (its size is that of the unit `None`), so `replay_status` can never
        // take the `Split` branch. The state itself is wired — it renders —
        // and Lane B1.5 gives the type a value when it records poison-split
        // evidence. `Option<SplitEvidence>` being zero-sized is the proof: a
        // type with even one value would need a discriminant.
        assert_eq!(std::mem::size_of::<Option<SplitEvidence>>(), 0);
        let f = fixture();
        let signed = legacy(&legacy(&f.psbt, &f.signers[0]), &f.signers[1]);
        assert!(!matches!(
            replay_status(&signed, &secp(), None),
            ReplayStatus::Split
        ));
        assert_eq!(
            pill_copy(&ReplayStatus::Split, &[]),
            ("Split — cannot replay".to_string(), PillTone::Success)
        );
        assert!(ReplayStatus::Split.is_finalisable());
        assert!(!ReplayStatus::Split.needs_acknowledgement());
    }

    #[test]
    fn status_matrix_from_verified_signatures() {
        let f = fixture();
        let secp = secp();

        assert_eq!(
            replay_status(&f.psbt, &secp, None),
            ReplayStatus::Unknown(UnknownReason::NotYetChecked)
        );
        let one = unified(&f.psbt, &f.signers[0]);
        assert_eq!(
            replay_status(&one, &secp, None),
            ReplayStatus::Unknown(UnknownReason::Incomplete)
        );
        let two = unified(&one, &f.signers[1]);
        assert_eq!(replay_status(&two, &secp, None), ReplayStatus::Protected);

        // Legacy signatures alone finalise but are replayable.
        let legacy_only = legacy(&legacy(&f.psbt, &f.signers[0]), &f.signers[1]);
        assert_eq!(
            replay_status(&legacy_only, &secp, None),
            ReplayStatus::Replayable { inputs: vec![0] }
        );
        // One unified + one legacy (different keys): protected — a legacy
        // signature alongside is fine.
        let mixed = legacy(&one, &f.signers[1]);
        assert_eq!(replay_status(&mixed, &secp, None), ReplayStatus::Protected);
        // Unified on the *third* key with two legacy ones: still protected
        // (the finaliser keeps the unified signature whichever key holds it).
        let third = legacy(
            &legacy(&unified(&f.psbt, &f.signers[2]), &f.signers[0]),
            &f.signers[1],
        );
        assert_eq!(replay_status(&third, &secp, None), ReplayStatus::Protected);
    }

    #[test]
    fn a_refused_psbt_is_unknown_with_the_reason_never_protected() {
        let f = fixture();
        let secp = secp();
        // Tamper with a unified record: the verifier refuses the whole PSBT.
        let mut tampered = unified(&unified(&f.psbt, &f.signers[0]), &f.signers[1]);
        let key = tampered.inputs[0]
            .proprietary
            .keys()
            .next()
            .cloned()
            .unwrap();
        let value = tampered.inputs[0].proprietary.get_mut(&key).unwrap();
        value[10] ^= 0x01;
        match replay_status(&tampered, &secp, None) {
            ReplayStatus::Unknown(UnknownReason::Refused(reason)) => {
                assert!(reason.contains("unified"), "{}", reason)
            }
            other => panic!("expected Refused, got {:?}", other),
        }
    }

    #[test]
    fn anyonecanpay_is_refused_before_signing_and_in_status() {
        let f = fixture();
        assert_eq!(refuse_before_dispatch(&f.psbt), Ok(()));

        // Requested on the input.
        let mut asked = f.psbt.clone();
        asked.inputs[0].sighash_type = Some(EcdsaSighashType::AllPlusAnyoneCanPay.into());
        assert_eq!(
            refuse_before_dispatch(&asked),
            Err(DispatchRefused::AnyoneCanPay {
                input: 0,
                sighash: 0x81
            })
        );
        assert!(refuse_before_dispatch(&asked)
            .unwrap_err()
            .to_string()
            .contains("ANYONECANPAY"));

        // Already carried by a signature: refused before signing, and the
        // status never reads it as protected or replayable — the finaliser
        // refuses it too.
        let mut carried = legacy(&legacy(&f.psbt, &f.signers[0]), &f.signers[1]);
        let (pk, sig) = carried.inputs[0]
            .partial_sigs
            .iter()
            .map(|(pk, sig)| (*pk, *sig))
            .next()
            .unwrap();
        carried.inputs[0].partial_sigs.insert(
            pk,
            ecdsa::Signature {
                signature: sig.signature,
                sighash_type: EcdsaSighashType::AllPlusAnyoneCanPay,
            },
        );
        assert!(matches!(
            refuse_before_dispatch(&carried),
            Err(DispatchRefused::AnyoneCanPay { input: 0, .. })
        ));
        assert!(matches!(
            replay_status(&carried, &secp(), None),
            ReplayStatus::Unknown(UnknownReason::Refused(_))
        ));
    }

    /// Taproot signature data never belongs to a Blake2b Vault PSBT, and its
    /// own sighash byte is not something the ECDSA walk vets: refused at the
    /// dispatch boundary whatever its sighash says.
    #[test]
    fn taproot_signature_data_is_refused_before_dispatch() {
        use coincube_core::miniscript::bitcoin::{
            key::Secp256k1, secp256k1::SecretKey, taproot, TapSighashType, XOnlyPublicKey,
        };
        let f = fixture();
        let secp = Secp256k1::new();
        let secret = SecretKey::from_slice(&[5u8; 32]).unwrap();
        let keypair = secp256k1::Keypair::from_secret_key(&secp, &secret);
        let (xonly, _) = XOnlyPublicKey::from_keypair(&keypair);
        let signature =
            secp.sign_schnorr_no_aux_rand(&secp256k1::Message::from_digest([4u8; 32]), &keypair);
        for sighash in [TapSighashType::All, TapSighashType::AllPlusAnyoneCanPay] {
            let mut with_key_sig = f.psbt.clone();
            with_key_sig.inputs[0].tap_key_sig = Some(taproot::Signature {
                signature,
                sighash_type: sighash,
            });
            assert_eq!(
                refuse_before_dispatch(&with_key_sig),
                Err(DispatchRefused::TaprootSignatureData { input: 0 }),
                "{:?}",
                sighash
            );
            let mut with_script_sig = f.psbt.clone();
            with_script_sig.inputs[0].tap_script_sigs.insert(
                (
                    xonly,
                    coincube_core::miniscript::bitcoin::TapLeafHash::from_script(
                        &coincube_core::miniscript::bitcoin::ScriptBuf::new(),
                        coincube_core::miniscript::bitcoin::taproot::LeafVersion::TapScript,
                    ),
                ),
                taproot::Signature {
                    signature,
                    sighash_type: sighash,
                },
            );
            assert_eq!(
                refuse_before_dispatch(&with_script_sig),
                Err(DispatchRefused::TaprootSignatureData { input: 0 }),
                "{:?}",
                sighash
            );
        }
        assert_eq!(refuse_before_dispatch(&f.psbt), Ok(()));
    }

    /// A reserved unified record with an `ANYONECANPAY` sighash byte (`0xa1`)
    /// is not a `partial_sigs` entry and not the input's sighash field, so the
    /// plain walk would let it through to a device; adapter validation at the
    /// dispatch boundary refuses it. Not a funds risk — every merge and the
    /// finaliser reject such a record too — but a signer must not be prompted
    /// for a signature that will be thrown away.
    #[test]
    fn a_malformed_reserved_record_is_refused_before_dispatch() {
        let f = fixture();
        let signed = unified(&f.psbt, &f.signers[0]);
        assert_eq!(refuse_before_dispatch(&signed), Ok(()));
        let mut malformed = signed.clone();
        let key = malformed.inputs[0]
            .proprietary
            .keys()
            .next()
            .cloned()
            .unwrap();
        let record = malformed.inputs[0].proprietary.get_mut(&key).unwrap();
        *record.last_mut().unwrap() = 0xa1;
        match refuse_before_dispatch(&malformed) {
            Err(DispatchRefused::MalformedUnifiedRecord(reason)) => {
                assert!(
                    reason.contains("a1") || reason.contains("sighash"),
                    "{}",
                    reason
                )
            }
            other => panic!(
                "expected the reserved record to be refused, got {:?}",
                other
            ),
        }
        assert!(refuse_before_dispatch(&malformed)
            .unwrap_err()
            .to_string()
            .contains("refusing to sign"));
    }

    #[test]
    fn spend_info_counts_unified_records_on_blake2b_only() {
        let f = fixture();
        let two = unified(&unified(&f.psbt, &f.signers[0]), &f.signers[1]);
        assert!(two.inputs[0].partial_sigs.is_empty());

        let blake2b = spend_info_for_chain(ChainId::BitcoinBlake2b, &f.descriptor, &two).unwrap();
        assert_eq!(blake2b.primary_path().sigs_count, 2);
        assert_eq!(blake2b.primary_path().signed_pubkeys.len(), 2);

        // Bitcoin: the unchanged analysis of the unchanged PSBT.
        let bitcoin = spend_info_for_chain(ChainId::Bitcoin, &f.descriptor, &two).unwrap();
        assert_eq!(bitcoin.primary_path().sigs_count, 0);
        assert_eq!(bitcoin, f.descriptor.partial_spend_info(&two).unwrap());

        // The projection is a scratch copy: the PSBT handed in is untouched
        // and still carries no `partial_sigs`.
        let projected = counting_projection(&two);
        assert_eq!(projected.inputs[0].partial_sigs.len(), 2);
        assert!(two.inputs[0].partial_sigs.is_empty());
        assert_eq!(projected.inputs[0].proprietary, two.inputs[0].proprietary);
    }

    #[test]
    fn broadcast_ready_needs_the_acknowledgement_only_when_replayable() {
        let f = fixture();
        let secp = secp();
        let none: &[(usize, Entanglement)] = &[];
        // Bitcoin family: the path threshold, whatever it says — and whatever
        // the entangled set says.
        assert!(broadcast_ready(true, None, none));
        assert!(!broadcast_ready(false, None, none));
        assert!(broadcast_ready(true, None, &[(0, Entanglement::Entangled)]));

        let protected = ReplayReview::new(
            &unified(&unified(&f.psbt, &f.signers[0]), &f.signers[1]),
            &secp,
        );
        assert_eq!(protected.status, ReplayStatus::Protected);
        assert!(protected.broadcast_ready(none));
        // The path count is irrelevant on BTCB2: verified witness decides.
        assert!(broadcast_ready(false, Some(&protected), none));

        let mut replayable = ReplayReview::new(
            &legacy(&legacy(&f.psbt, &f.signers[0]), &f.signers[1]),
            &secp,
        );
        assert!(replayable.status.needs_acknowledgement());
        assert!(!replayable.broadcast_ready(none));
        replayable.set_acknowledged(true);
        assert!(replayable.broadcast_ready(none));

        let unknown = ReplayReview::new(&f.psbt, &secp);
        assert!(!unknown.broadcast_ready(none));
        assert!(!broadcast_ready(true, Some(&unknown), none));
    }

    /// `#276` I13: a replayable input that Connect has confirmed also exists
    /// on Bitcoin *requires* a replay-capable signature. The acknowledgement
    /// never clears it; an unanswered lookup never imposes it.
    #[test]
    fn a_known_entangled_replayable_input_cannot_be_acknowledged_away() {
        let f = fixture();
        let secp = secp();
        let legacy_only = legacy(&legacy(&f.psbt, &f.signers[0]), &f.signers[1]);
        let mut review = ReplayReview::new(&legacy_only, &secp);
        assert_eq!(review.status, ReplayStatus::Replayable { inputs: vec![0] });
        let entangled = [(0, Entanglement::Entangled)];

        // 1. Known entangled + legacy-only: not ready, and still not ready
        //    after the tick — the check that distinguishes a requirement
        //    from a warning.
        assert_eq!(
            blocked_entangled_inputs(&review.status, &entangled),
            vec![0]
        );
        assert!(!review.signatures_complete(&entangled));
        assert!(!review.broadcast_ready(&entangled));
        review.set_acknowledged(true);
        assert!(!review.broadcast_ready(&entangled));
        assert!(!broadcast_ready(true, Some(&review), &entangled));

        // 2. The same input with a verified unified signature in the witness
        //    is ready: the requirement is satisfiable in this build.
        let with_unified = legacy(&unified(&f.psbt, &f.signers[2]), &f.signers[0]);
        let with_unified = legacy(&with_unified, &f.signers[1]);
        let protected = ReplayReview::new(&with_unified, &secp);
        assert_eq!(protected.status, ReplayStatus::Protected);
        assert!(blocked_entangled_inputs(&protected.status, &entangled).is_empty());
        assert!(protected.signatures_complete(&entangled));
        assert!(protected.broadcast_ready(&entangled));

        // 3. Unknown + replayable: unchanged amber, the tick still works —
        //    the gate must not over-block a spend nobody has looked up yet.
        let unknown = [(0, Entanglement::Unknown)];
        let mut review = ReplayReview::new(&legacy_only, &secp);
        assert!(blocked_entangled_inputs(&review.status, &unknown).is_empty());
        assert!(review.signatures_complete(&unknown));
        assert!(!review.broadcast_ready(&unknown));
        review.set_acknowledged(true);
        assert!(review.broadcast_ready(&unknown));

        // 4. Not entangled + replayable: the unchanged acknowledgement path.
        let mut review = ReplayReview::new(&legacy_only, &secp);
        assert!(!review.broadcast_ready(&[]));
        review.set_acknowledged(true);
        assert!(review.broadcast_ready(&[]));
        // (`entangled_inputs` never yields NotEntangled entries, but the gate
        // ignores them if handed one.)
        assert!(review.broadcast_ready(&[(0, Entanglement::NotEntangled)]));

        // The requirement is on the witness, not on the coin: an entangled
        // input that is not replayable, or is not an input at all, is not
        // blocked.
        assert!(blocked_entangled_inputs(
            &ReplayStatus::Replayable { inputs: vec![1] },
            &[(0, Entanglement::Entangled)]
        )
        .is_empty());
        assert!(blocked_entangled_inputs(
            &ReplayStatus::Unknown(UnknownReason::Incomplete),
            &[(0, Entanglement::Entangled)]
        )
        .is_empty());
    }

    #[test]
    fn blocked_copy_names_the_input_and_the_honest_remedy() {
        assert_eq!(blocked_entangled_copy(&[]), None);
        let one = blocked_entangled_copy(&[0]).unwrap();
        assert!(one.starts_with("Input 0 also exists on Bitcoin"), "{}", one);
        assert!(one.contains("replay-capable signature on it"), "{}", one);
        assert!(one.contains("Cube key or a Border Wallet key"), "{}", one);
        assert!(
            one.contains("Splitting the coins first is not available in this version"),
            "{}",
            one
        );
        let two = blocked_entangled_copy(&[0, 2]).unwrap();
        assert!(
            two.starts_with("Inputs 0, 2 also exist on Bitcoin"),
            "{}",
            two
        );
        assert!(two.contains("signature on them"), "{}", two);
    }

    /// The acknowledgement is keyed to the PSBT's bytes: a recompute over the
    /// same bytes (a Keychain stream event emits one for every event) keeps
    /// it; any content change — even one that leaves the status enum equal —
    /// drops it; a brand-new review never has one.
    #[test]
    fn the_acknowledgement_follows_the_psbt_bytes_not_the_status() {
        let f = fixture();
        let secp = secp();
        let legacy_only = legacy(&legacy(&f.psbt, &f.signers[0]), &f.signers[1]);
        let mut review = ReplayReview::new(&legacy_only, &secp);
        assert!(!review.acknowledged());
        review.set_acknowledged(true);
        assert!(review.acknowledged());

        // Same bytes: kept.
        let same = review.refreshed(&legacy_only, &secp);
        assert!(same.acknowledged());
        assert_eq!(same.status, review.status);

        // A third legacy signature: the status is still `Replayable { [0] }`
        // but the content changed, so the tick is gone.
        let three = legacy(&legacy_only, &f.signers[2]);
        let changed = review.refreshed(&three, &secp);
        assert_eq!(changed.status, review.status);
        assert!(!changed.acknowledged());
        assert_ne!(changed.psbt_digest(), review.psbt_digest());

        // Clearing works, and a fresh review starts unacknowledged.
        review.set_acknowledged(false);
        assert!(!review.acknowledged());
        assert!(!ReplayReview::new(&legacy_only, &secp).acknowledged());
        assert_eq!(psbt_digest(&legacy_only), review.psbt_digest());
    }

    #[test]
    fn pill_copy_names_replayable_inputs_and_entangled_ones() {
        assert_eq!(
            pill_copy(&ReplayStatus::Protected, &[]),
            ("Replay protected".to_string(), PillTone::Success)
        );
        assert_eq!(
            pill_copy(&ReplayStatus::Replayable { inputs: vec![1] }, &[]),
            (
                "Replayable — no replay-capable signature on input 1".to_string(),
                PillTone::Warning
            )
        );
        assert_eq!(
            pill_copy(
                &ReplayStatus::Replayable { inputs: vec![0, 2] },
                &[(0, Entanglement::Entangled), (2, Entanglement::Unknown)]
            ),
            (
                "Replayable — no replay-capable signature on inputs \
                 0 (also exists on Bitcoin — signature required), \
                 2 (not yet checked against Bitcoin)"
                    .to_string(),
                PillTone::Warning
            )
        );
        // An entangled input that already carries a unified signature is not
        // named: the pill is about the witness, not the coin.
        assert_eq!(
            pill_copy(&ReplayStatus::Protected, &[(0, Entanglement::Entangled)]),
            ("Replay protected".to_string(), PillTone::Success)
        );
        assert_eq!(
            pill_copy(&ReplayStatus::Unknown(UnknownReason::NotYetChecked), &[]),
            ("Not yet checked".to_string(), PillTone::Neutral)
        );
        assert_eq!(
            pill_copy(&ReplayStatus::Unknown(UnknownReason::Incomplete), &[]).1,
            PillTone::Neutral
        );
        assert_eq!(
            pill_copy(
                &ReplayStatus::Unknown(UnknownReason::Refused("bad".into())),
                &[]
            )
            .0,
            "Unknown — bad"
        );
        assert_eq!(
            REPLAYABLE_ACKNOWLEDGEMENT,
            "I understand this can also spend my Bitcoin"
        );
    }

    #[test]
    fn entangled_inputs_keep_unknown_and_drop_only_definite_no() {
        let f = fixture();
        let txid = f.psbt.unsigned_tx.input[0].previous_output.txid;
        assert_eq!(
            entangled_inputs(&f.psbt, |_| Entanglement::NotEntangled),
            vec![]
        );
        assert_eq!(
            entangled_inputs(&f.psbt, |_| Entanglement::Unknown),
            vec![(0, Entanglement::Unknown)]
        );
        assert_eq!(
            entangled_inputs(&f.psbt, |t| {
                assert_eq!(*t, txid);
                Entanglement::Entangled
            }),
            vec![(0, Entanglement::Entangled)]
        );
    }

    #[test]
    fn i8_notice_is_copy_only_and_gated_to_a_bitcoin_cube_with_unswept_coins() {
        assert_eq!(
            bitcoin_cube_unswept_notice(ChainId::Bitcoin, Some(true)),
            Some(BITCOIN_CUBE_UNSWEPT_NOTICE)
        );
        assert_eq!(
            BITCOIN_CUBE_UNSWEPT_NOTICE,
            "These coins also exist on Bitcoin Blake2b until swept there."
        );
        for (chain, unswept) in [
            (ChainId::Bitcoin, None),
            (ChainId::Bitcoin, Some(false)),
            (ChainId::BitcoinBlake2b, Some(true)),
            (ChainId::Testnet4, Some(true)),
            (ChainId::Signet, Some(true)),
        ] {
            assert_eq!(
                bitcoin_cube_unswept_notice(chain, unswept),
                None,
                "{:?}",
                chain
            );
        }
    }

    #[test]
    fn a_malformed_proprietary_record_is_unknown_not_protected() {
        use coincube_core::miniscript::bitcoin::psbt::raw::ProprietaryKey;
        let f = fixture();
        let mut bad = unified(&unified(&f.psbt, &f.signers[0]), &f.signers[1]);
        bad.inputs[0].proprietary.insert(
            ProprietaryKey {
                prefix: b"coincube".to_vec(),
                subtype: 0,
                key: vec![0x02; 33],
            },
            vec![0x30, 0x00],
        );
        assert!(matches!(
            replay_status(&bad, &secp(), None),
            ReplayStatus::Unknown(UnknownReason::Refused(_))
        ));
    }
}
