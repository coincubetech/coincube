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
//!   [`REPLAYABLE_ACKNOWLEDGEMENT`].
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

/// An input that must not be signed: `ANYONECANPAY` never occurs in a Vault
/// flow, and on Bitcoin Blake2b it is refused outright rather than warned
/// about (a unified `ANYONECANPAY` would commit to fewer inputs than the
/// replay model reasons over).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnyoneCanPayRefused {
    pub input: usize,
    pub sighash: u32,
}

impl std::fmt::Display for AnyoneCanPayRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "input {} asks for sighash 0x{:02x} (ANYONECANPAY), which a Bitcoin Blake2b \
             Vault spend never uses; refusing to sign",
            self.input, self.sighash
        )
    }
}

/// Refuse a PSBT that asks for, or already carries, an `ANYONECANPAY` sighash
/// on any input. Checked before any signer is dispatched on Bitcoin Blake2b.
pub fn refuse_anyonecanpay(psbt: &Psbt) -> Result<(), AnyoneCanPayRefused> {
    const ANYONECANPAY: u32 = 0x80;
    for (index, input) in psbt.inputs.iter().enumerate() {
        if let Some(sighash) = input.sighash_type {
            let raw = sighash.to_u32();
            if raw & ANYONECANPAY != 0 {
                return Err(AnyoneCanPayRefused {
                    input: index,
                    sighash: raw,
                });
            }
        }
        for signature in input.partial_sigs.values() {
            let raw = signature.sighash_type.to_u32();
            if raw & ANYONECANPAY != 0 {
                return Err(AnyoneCanPayRefused {
                    input: index,
                    sighash: raw,
                });
            }
        }
    }
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
    /// The user ticked [`REPLAYABLE_ACKNOWLEDGEMENT`]. Reset whenever the
    /// status is recomputed, so a new signature never inherits an old
    /// acknowledgement.
    pub acknowledged: bool,
}

impl ReplayReview {
    pub fn new(psbt: &Psbt, secp: &secp256k1::Secp256k1<impl secp256k1::Verification>) -> Self {
        Self {
            status: replay_status(psbt, secp, None),
            acknowledged: false,
        }
    }

    /// Whether the spend may be broadcast: finalisable, and acknowledged when
    /// it is replayable.
    pub fn broadcast_ready(&self) -> bool {
        self.status.is_finalisable() && (!self.status.needs_acknowledgement() || self.acknowledged)
    }
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
/// (picker close, broadcast handler) and the view (button), so they cannot
/// disagree: on a Bitcoin-family Cube it is the path threshold as before; on
/// Bitcoin Blake2b it is the finaliser's verdict plus the acknowledgement.
pub fn broadcast_ready(path_ready: bool, review: Option<&ReplayReview>) -> bool {
    match review {
        None => path_ready,
        Some(review) => review.broadcast_ready(),
    }
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
            let entangled: BTreeSet<usize> = entangled.iter().map(|(index, _)| *index).collect();
            let list = inputs
                .iter()
                .map(|index| {
                    if entangled.contains(index) {
                        format!("{index} (also exists on Bitcoin)")
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
        assert_eq!(refuse_anyonecanpay(&f.psbt), Ok(()));

        // Requested on the input.
        let mut asked = f.psbt.clone();
        asked.inputs[0].sighash_type = Some(EcdsaSighashType::AllPlusAnyoneCanPay.into());
        assert_eq!(
            refuse_anyonecanpay(&asked),
            Err(AnyoneCanPayRefused {
                input: 0,
                sighash: 0x81
            })
        );
        assert!(refuse_anyonecanpay(&asked)
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
            refuse_anyonecanpay(&carried),
            Err(AnyoneCanPayRefused { input: 0, .. })
        ));
        assert!(matches!(
            replay_status(&carried, &secp(), None),
            ReplayStatus::Unknown(UnknownReason::Refused(_))
        ));
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
        // Bitcoin family: the path threshold, whatever it says.
        assert!(broadcast_ready(true, None));
        assert!(!broadcast_ready(false, None));

        let protected = ReplayReview::new(
            &unified(&unified(&f.psbt, &f.signers[0]), &f.signers[1]),
            &secp,
        );
        assert_eq!(protected.status, ReplayStatus::Protected);
        assert!(protected.broadcast_ready());
        // The path count is irrelevant on BTCB2: verified witness decides.
        assert!(broadcast_ready(false, Some(&protected)));

        let mut replayable = ReplayReview::new(
            &legacy(&legacy(&f.psbt, &f.signers[0]), &f.signers[1]),
            &secp,
        );
        assert!(replayable.status.needs_acknowledgement());
        assert!(!replayable.broadcast_ready());
        replayable.acknowledged = true;
        assert!(replayable.broadcast_ready());

        let unknown = ReplayReview::new(&f.psbt, &secp);
        assert!(!unknown.broadcast_ready());
        assert!(!broadcast_ready(true, Some(&unknown)));
    }

    #[test]
    fn a_recomputed_review_drops_the_acknowledgement() {
        let f = fixture();
        let legacy_only = legacy(&legacy(&f.psbt, &f.signers[0]), &f.signers[1]);
        let mut review = ReplayReview::new(&legacy_only, &secp());
        review.acknowledged = true;
        let fresh = ReplayReview::new(&legacy_only, &secp());
        assert!(!fresh.acknowledged);
        assert_eq!(fresh.status, review.status);
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
                &[(0, Entanglement::Entangled), (1, Entanglement::Unknown)]
            ),
            (
                "Replayable — no replay-capable signature on inputs 0 (also exists on Bitcoin), 2"
                    .to_string(),
                PillTone::Warning
            )
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
