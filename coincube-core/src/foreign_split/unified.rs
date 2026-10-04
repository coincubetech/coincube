//! Split unified fallback (#568 B4b): one BTCB2 sweep of a foreign wallet's
//! shared pre-fork coins into the target Cube, signed `ALL|UNIFIED` (`0x21`)
//! so that the transaction is invalid on Bitcoin by construction.
//! Construction, deterministic reconstruction, finalization and
//! retained-witness verification only.
//!
//! The two-step split (step 1 on Bitcoin, step 2 on BTCB2) separates the
//! chains with an OP_RETURN poison and ordinary `SIGHASH_ALL` signatures.
//! This single-step route separates them with the signature itself: a `0x21`
//! signature verifies on BTCB2 only ([`crate::unified_foreign`]), so a sweep
//! whose every input carries a verified unified signature cannot be replayed
//! on Bitcoin. That is also its limit: nothing moves on Bitcoin, the coins
//! stay spendable there, and only the witness bytes keep the chains apart.
//!
//! The construction is step 2's: exactly the selected coins, ordered by
//! outpoint, one target output, no change, the same fee bounds, dust floor
//! and locktime rule, and the same refusal of a target that is one of the
//! foreign wallet's own scripts. There is no step 1 and no claimed-prevout
//! list; every coin must still be shared pre-fork history on both chains
//! (owner decision D10).
//!
//! Signatures must be unified `ALL|UNIFIED`, and every input must request
//! exactly that (`PSBT_IN_SIGHASH_TYPE` = `0x21`). Under owner decision P1
//! the single-step route has no legacy state: a `partial_sigs` entry
//! anywhere, or a `0x01` signature in a retained witness, is refused as
//! [`UnifiedSweepFinalizeError::LegacySignature`] and is never classified
//! Protected. Devices sign `0x01` only, so this route is seed-only; hardware
//! users take the two-step route.
//!
//! What this module does NOT do: it reads no chain, proves no coin unspent
//! and no target fresh, checks no anchor, runs no preflight, reserves
//! nothing, persists nothing and grants no signing or broadcast authority.
//! The repository has no node vector for `0x21` on P2PKH, P2WPKH or
//! P2SH-P2WPKH; the two-chain regtest (#568 B6) is the node-acceptance check
//! for every shape.

use super::{step2::is_source_script, *};
use crate::{
    psbt_unified::{is_reserved_key, proprietary_key, UnifiedPsbt},
    unified_finalize::{FinalizedSpend, InputWitnessReport},
    unified_foreign::{finalize_foreign_unified, ForeignUnifiedError},
    unified_sighash::{UnifiedSighashCache, SCRIPT_TYPE_BASE, SCRIPT_TYPE_WITNESS_V0},
};
use miniscript::bitcoin::{psbt::PsbtSighashType, script::Instruction, PublicKey};

/// `SIGHASH_ALL | SIGHASH_UNIFIED`, the only sighash this route signs,
/// requests or retains.
const UNIFIED_SIGHASH_ALL: u8 = 0x21;
/// Plain `SIGHASH_ALL`: a legacy signature byte, refused wherever it appears.
const LEGACY_SIGHASH_ALL: u8 = 0x01;

/// Everything that identifies one unified sweep apart from its fee and
/// locktime. The caller records these to reconstruct the sweep later.
#[derive(Debug, Clone, Copy)]
pub struct UnifiedInputs<'a> {
    /// Bitcoin Blake2b mainnet or testnet4.
    pub chain: ChainId,
    pub source: &'a SplitSource,
    /// The coins to sweep, freshly authenticated: shared pre-fork history on
    /// both chains, as both split steps require.
    pub coins: &'a [SplitCoin],
    /// From the authenticated BTCB2 network anchor; never a constant.
    pub fork_height: u64,
    /// The target Cube's address script: P2WSH or P2TR, the only Vault
    /// address types. Its ownership and freshness are the caller's to prove;
    /// this module refuses any other type and the foreign wallet's own
    /// scripts near its coins.
    pub target: &'a bitcoin::Script,
}

/// An unsigned unified sweep with no public-field or deserialization bypass.
/// It certifies only the construction checks, not live eligibility.
#[derive(Debug, Clone)]
pub struct UnifiedSweep {
    psbt: Psbt,
    chain: ChainId,
    source: SplitSource,
    /// Branch and index of each transaction input, in input order.
    inputs: Vec<(SplitBranch, u32)>,
    fork_height: u64,
    maximum_signed_vbytes: u64,
    /// Sum of the authenticated spent outputs.
    total: u64,
}

impl UnifiedSweep {
    pub fn psbt(&self) -> &Psbt {
        &self.psbt
    }
    pub fn chain(&self) -> ChainId {
        self.chain
    }
    pub fn source(&self) -> &SplitSource {
        &self.source
    }
    pub fn target(&self) -> &bitcoin::Script {
        &self.psbt.unsigned_tx.output[0].script_pubkey
    }
    /// The fork height every spent coin was checked to precede
    /// (`UnifiedInputs::fork_height`). Not in the transaction, so a journal
    /// must bind it separately.
    pub fn fork_height(&self) -> u64 {
        self.fork_height
    }
    /// The unsigned transaction's txid. As in both split steps, P2PKH and
    /// P2SH-P2WPKH scriptSigs change it; track the finalized transaction's
    /// own txid.
    pub fn txid(&self) -> Txid {
        self.psbt.unsigned_tx.compute_txid()
    }
    /// The spent outpoints, in input order.
    pub fn spent_outpoints(&self) -> Vec<OutPoint> {
        self.psbt
            .unsigned_tx
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect()
    }
    /// Worst-case signed size the fee was charged for. A unified witness has
    /// the same shape as a legacy one (one trailing sighash byte either way),
    /// so step 2's estimate applies unchanged.
    pub fn maximum_signed_vbytes(&self) -> u64 {
        self.maximum_signed_vbytes
    }
    pub fn fee(&self) -> Amount {
        // Economics were checked at construction; this cannot underflow.
        Amount::from_sat(self.total - self.psbt.unsigned_tx.output[0].value.to_sat())
    }
}

struct Plan {
    selection: Selection,
    maximum_signed_vbytes: u64,
}

fn target_output(target: &bitcoin::Script, value: Amount) -> Vec<TxOut> {
    vec![TxOut {
        value,
        script_pubkey: target.to_owned(),
    }]
}

fn plan(inputs: &UnifiedInputs<'_>) -> Result<Plan, Error> {
    if !inputs.chain.is_blake2b() {
        return Err(Error::NotBitcoinBlake2b(inputs.chain));
    }
    let selection = select(inputs.source, inputs.coins, inputs.fork_height)?;
    let target = inputs.target;
    if !(target.is_p2wsh() || target.is_p2tr())
        || is_source_script(inputs.source, &selection.selected, target)?
    {
        return Err(Error::InvalidTarget);
    }
    let maximum_signed_vbytes =
        maximum_signed_vbytes(&selection.selected, target_output(target, Amount::ZERO))?;
    Ok(Plan {
        selection,
        maximum_signed_vbytes,
    })
}

fn build(
    inputs: &UnifiedInputs<'_>,
    plan: Plan,
    value: Amount,
    locktime: LockTime,
) -> Result<UnifiedSweep, Error> {
    let selected = &plan.selection.selected;
    let tx = unsigned_transaction(selected, target_output(inputs.target, value), locktime);
    let psbt = psbt_for(tx, selected)?;
    Ok(UnifiedSweep {
        psbt,
        chain: inputs.chain,
        source: inputs.source.clone(),
        inputs: selected
            .iter()
            .map(|input| (input.branch, input.index))
            .collect(),
        fork_height: inputs.fork_height,
        maximum_signed_vbytes: plan.maximum_signed_vbytes,
        total: plan.selection.total,
    })
}

/// Build the unsigned BTCB2 unified sweep spending exactly `inputs.coins` to
/// the target, with no change. The fee is `feerate_vb` times the worst-case
/// signed size; inputs are ordered by outpoint and the result is
/// deterministic. `locktime` must be a block height no greater than
/// `btcb2_tip_height`, the observed BTCB2 tip.
pub fn create_unified_sweep(
    inputs: &UnifiedInputs<'_>,
    feerate_vb: u64,
    locktime: LockTime,
    btcb2_tip_height: u32,
) -> Result<UnifiedSweep, Error> {
    check_locktime(locktime, btcb2_tip_height)?;
    let plan = plan(inputs)?;
    if !(1..=spend::MAX_FEERATE).contains(&feerate_vb) {
        return Err(Error::Economics);
    }
    let fee = plan
        .maximum_signed_vbytes
        .checked_mul(feerate_vb)
        .ok_or(Error::Economics)?;
    let value = plan
        .selection
        .total
        .checked_sub(fee)
        .ok_or(Error::Economics)?;
    check_economics(
        plan.selection.total,
        value,
        inputs.target,
        plan.maximum_signed_vbytes,
    )?;
    build(inputs, plan, Amount::from_sat(value), locktime)
}

/// Rebuild an exact recorded unified sweep from freshly authenticated coins.
/// Only the target amount and the locktime are read from the record;
/// everything else is rebuilt and the whole transaction must then match.
/// Economics and the locktime (against `btcb2_tip_height`, the current BTCB2
/// tip) are checked again. This reserves nothing and authorizes no
/// submission.
pub fn reconstruct_unified_sweep(
    inputs: &UnifiedInputs<'_>,
    recorded: &Transaction,
    btcb2_tip_height: u32,
) -> Result<UnifiedSweep, Error> {
    if recorded.output.len() != 1 {
        return Err(Error::Recorded(
            "Recorded unified sweep must have one output",
        ));
    }
    let plan = plan(inputs)?;
    let value = recorded.output[0].value;
    check_locktime(recorded.lock_time, btcb2_tip_height)?;
    check_economics(
        plan.selection.total,
        value.to_sat(),
        inputs.target,
        plan.maximum_signed_vbytes,
    )?;
    let rebuilt = build(inputs, plan, value, recorded.lock_time)?;
    if rebuilt.psbt.unsigned_tx != *recorded {
        return Err(Error::Recorded(
            "Recorded transaction differs from the owned unified sweep construction",
        ));
    }
    Ok(rebuilt)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifiedSweepFinalizeError {
    /// The signed PSBT is not the exact construction plus unified records.
    ConstructionChanged,
    /// An input requests a sighash other than `ALL|UNIFIED` (`0x21`), or
    /// none at all.
    UnsupportedSighash {
        input: usize,
    },
    /// A legacy signature: a `partial_sigs` entry, or a `0x01` signature in
    /// a retained witness. Refused under owner decision P1, never Protected.
    LegacySignature {
        input: usize,
    },
    /// A previous output is not authenticated by its transaction, or is not
    /// the construction's own derivation.
    InputAuthentication,
    /// The unified verifier or finalizer refused the records.
    Foreign(ForeignUnifiedError),
    /// An input's final witness holds no verified unified signature.
    /// [`finalize_foreign_unified`] refuses rather than degrades, so this is
    /// not reachable through it; it is checked here so the Protected verdict
    /// never rests on another module's contract alone.
    NotProtected {
        input: usize,
    },
    Economics,
    /// A retained witness is not one this finalizer produces.
    InvalidWitness,
}

impl fmt::Display for UnifiedSweepFinalizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Foreign(err) => write!(f, "Unified sweep finalization refused: {err}"),
            other => write!(f, "Unified sweep finalization refused: {other:?}"),
        }
    }
}
impl std::error::Error for UnifiedSweepFinalizeError {}

/// The replay classification of a verified unified sweep. There is one
/// value: every input's witness holds at least one verified `0x21`
/// signature, so the transaction is invalid on Bitcoin. A sweep that could
/// be anything else is refused by [`finalize_unified_sweep`] rather than
/// classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnifiedReplayStatus {
    Protected,
}

/// A finalized unified sweep with verified witnesses. Cryptographic evidence
/// only: it does not prove the coins unspent, the target fresh, relay
/// acceptance or inclusion, and it grants no broadcast authority. It does
/// certify that every input's witness is invalid on Bitcoin.
#[derive(Debug)]
pub struct VerifiedUnifiedSweep {
    finalized: FinalizedSpend,
    chain: ChainId,
    construction_txid: Txid,
    fee: Amount,
}

impl VerifiedUnifiedSweep {
    pub fn transaction(&self) -> &Transaction {
        &self.finalized.transaction
    }
    pub fn chain(&self) -> ChainId {
        self.chain
    }
    /// The unsigned construction's txid; track `transaction().compute_txid()`.
    pub fn construction_txid(&self) -> Txid {
        self.construction_txid
    }
    pub fn fee(&self) -> Amount {
        self.fee
    }
    pub fn vsize(&self) -> usize {
        self.finalized.transaction.vsize()
    }
    /// Per input, how many verified unified and legacy signatures the final
    /// witness holds. Legacy is always zero here.
    pub fn inputs(&self) -> &[InputWitnessReport] {
        &self.finalized.inputs
    }
    /// Protected, by construction: the finalizer refused anything else.
    pub fn replay_status(&self) -> UnifiedReplayStatus {
        debug_assert!(self
            .finalized
            .inputs
            .iter()
            .all(InputWitnessReport::replay_protected));
        UnifiedReplayStatus::Protected
    }
}

/// Accept only an unfinalized PSBT of the exact opaque construction plus
/// unified records: everything except the reserved `coincube`/0 records must
/// be identical (another proprietary record is a change, #647 O4), every input must request `ALL|UNIFIED`, and no input may carry
/// a `partial_sigs` entry (P1). The unified finalizer then verifies every
/// record against the authenticated previous outputs and builds the witness
/// from verified unified signatures only; every input of the result must be
/// `replay_protected()`. Economics are checked again.
pub fn finalize_unified_sweep<C: secp256k1::Verification>(
    construction: &UnifiedSweep,
    signed: &UnifiedPsbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedUnifiedSweep, UnifiedSweepFinalizeError> {
    let original = &construction.psbt;
    check_signed_unified_construction(original, signed.psbt())?;
    let authenticated = authenticate_inputs(construction)?;
    let finalized =
        finalize_foreign_unified(signed, secp).map_err(UnifiedSweepFinalizeError::Foreign)?;
    if finalized.inputs.len() != original.inputs.len()
        || finalized.transaction.input.len() != original.inputs.len()
    {
        return Err(UnifiedSweepFinalizeError::InvalidWitness);
    }
    if let Some(input) = finalized
        .inputs
        .iter()
        .position(|report| !report.replay_protected() || report.legacy_used != 0)
    {
        return Err(UnifiedSweepFinalizeError::NotProtected { input });
    }
    // Defence in depth (#647 O5): `finalize_foreign_unified` only adds
    // scriptSigs and witnesses to the checked construction's transaction, so
    // no test reaches this refusal; it pins that the bytes returned are that
    // construction and nothing else, whatever the finalizer does later.
    if strip(&finalized.transaction) != original.unsigned_tx {
        return Err(UnifiedSweepFinalizeError::InvalidWitness);
    }
    let fee = economics(construction, &authenticated)?;
    Ok(VerifiedUnifiedSweep {
        finalized,
        chain: construction.chain,
        construction_txid: original.unsigned_tx.compute_txid(),
        fee,
    })
}

/// Verify a recorded signed unified sweep (a journal's bytes) against the
/// exact construction rebuilt from authenticated coins, the unified twin of
/// [`verify_split_step2_transaction`]: the transaction with every scriptSig
/// and witness removed must be the construction; each retained signature
/// item must be a `0x21` signature by a key the input commits to, over the
/// unified digest of this transaction (a `0x01` item is a legacy signature
/// and refused); the recovered records are then finalized by
/// [`finalize_unified_sweep`], whose output must equal the recorded bytes,
/// including every witness item. Covers the base (P2PKH scriptSig), P2SH
/// (P2SH-P2WPKH) and native segwit script types. Stored bytes and a matching
/// txid are not signature evidence, and this grants no permission to resend.
pub fn verify_unified_sweep_transaction<C: secp256k1::Verification>(
    construction: &UnifiedSweep,
    transaction: &Transaction,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedUnifiedSweep, UnifiedSweepFinalizeError> {
    use UnifiedSweepFinalizeError::{InvalidWitness, LegacySignature};
    let original = &construction.psbt;
    // The recorded bytes are matched to the construction before anything
    // else is read from them; the re-finalization below must then return
    // exactly these bytes (#647 O5).
    if strip(transaction) != original.unsigned_tx {
        return Err(UnifiedSweepFinalizeError::ConstructionChanged);
    }
    let authenticated = authenticate_inputs(construction)?;
    let prevouts: Vec<TxOut> = authenticated
        .iter()
        .map(|(output, _)| output.clone())
        .collect();
    let cache =
        UnifiedSighashCache::new(&original.unsigned_tx, &prevouts).map_err(|_| InvalidWitness)?;
    let mut recovered = UnifiedPsbt::from_psbt(original.clone()).map_err(|_| InvalidWitness)?;
    for (index, (_, definite)) in authenticated.iter().enumerate() {
        // The same scriptCode and script type the unified signer uses for
        // each shape (`unified_foreign`'s table): P2PKH signs its
        // scriptPubKey as type 0; P2WPKH and P2SH-P2WPKH the implied P2PKH
        // script as type 1; P2WSH its witness script as type 1. A wrong
        // choice here cannot admit a witness: the recovered records are
        // verified again by the finalizer.
        let script_code = definite.script_code().map_err(|_| InvalidWitness)?;
        let script_type = if is_segwit(definite) {
            SCRIPT_TYPE_WITNESS_V0
        } else {
            SCRIPT_TYPE_BASE
        };
        let digest = cache
            .signature_hash(index, UNIFIED_SIGHASH_ALL, script_type, &script_code)
            .map_err(|_| InvalidWitness)?;
        let message = secp256k1::Message::from_digest(digest);
        let mut found = 0;
        for item in retained_items(definite, &transaction.input[index])? {
            let Some((&tag, der)) = item.split_last() else {
                continue;
            };
            let Ok(signature) = secp256k1::ecdsa::Signature::from_der(der) else {
                continue;
            };
            match tag {
                UNIFIED_SIGHASH_ALL => {}
                LEGACY_SIGHASH_ALL => return Err(LegacySignature { input: index }),
                _ => return Err(InvalidWitness),
            }
            let key = original.inputs[index]
                .bip32_derivation
                .keys()
                .find(|key| secp.verify_ecdsa(&message, &signature, key).is_ok())
                .ok_or(InvalidWitness)?;
            recovered.psbt_mut().inputs[index]
                .proprietary
                .insert(proprietary_key(&PublicKey::new(*key)), item);
            found += 1;
        }
        if found == 0 {
            return Err(InvalidWitness);
        }
        recovered.psbt_mut().inputs[index].sighash_type =
            Some(PsbtSighashType::from_u32(u32::from(UNIFIED_SIGHASH_ALL)));
    }
    let verified = finalize_unified_sweep(construction, &recovered, secp)?;
    if verified.transaction() != transaction {
        return Err(InvalidWitness);
    }
    Ok(verified)
}

/// The signed PSBT must be the exact construction plus unified signature
/// records in the reserved `coincube`/0 namespace, with every input
/// requesting `ALL|UNIFIED` and none carrying a legacy `partial_sigs` entry.
fn check_signed_unified_construction(
    original: &Psbt,
    signed: &Psbt,
) -> Result<(), UnifiedSweepFinalizeError> {
    if signed.unsigned_tx != original.unsigned_tx
        || signed.inputs.len() != original.inputs.len()
        || signed.outputs.len() != original.outputs.len()
    {
        return Err(UnifiedSweepFinalizeError::ConstructionChanged);
    }
    let mut normalized = signed.clone();
    for (index, input) in signed.inputs.iter().enumerate() {
        if !input.partial_sigs.is_empty() {
            return Err(UnifiedSweepFinalizeError::LegacySignature { input: index });
        }
        if input.sighash_type.map(|s| s.to_u32()) != Some(u32::from(UNIFIED_SIGHASH_ALL)) {
            return Err(UnifiedSweepFinalizeError::UnsupportedSighash { input: index });
        }
        // Only this crate's own unified records are signing output (#647
        // O4); any other proprietary record is a change to the construction,
        // as step 2's check treats everything but `partial_sigs`.
        normalized.inputs[index]
            .proprietary
            .retain(|key, _| !is_reserved_key(key));
        normalized.inputs[index].sighash_type = original.inputs[index].sighash_type;
    }
    if normalized != *original {
        return Err(UnifiedSweepFinalizeError::ConstructionChanged);
    }
    Ok(())
}

/// Authenticate each input's previous output by its transaction and against
/// the construction's own derivation. Returns the outputs with their
/// definite descriptors, in input order.
#[allow(clippy::type_complexity)]
fn authenticate_inputs(
    construction: &UnifiedSweep,
) -> Result<Vec<(TxOut, Descriptor<DefiniteDescriptorKey>)>, UnifiedSweepFinalizeError> {
    let original = &construction.psbt;
    if construction.inputs.len() != original.inputs.len()
        || original.unsigned_tx.input.len() != original.inputs.len()
        || original.unsigned_tx.output.len() != 1
    {
        return Err(UnifiedSweepFinalizeError::ConstructionChanged);
    }
    let mut authenticated = Vec::with_capacity(original.inputs.len());
    for (index, input) in original.inputs.iter().enumerate() {
        let output = spend::authenticate_previous_output(
            &original.unsigned_tx.input[index].previous_output,
            input.non_witness_utxo.as_ref(),
            input.witness_utxo.as_ref(),
        )
        .map_err(|_| UnifiedSweepFinalizeError::InputAuthentication)?;
        let (branch, derivation) = construction.inputs[index];
        let definite = construction
            .source
            .derive(branch, derivation)
            .map_err(|_| UnifiedSweepFinalizeError::InputAuthentication)?;
        if definite.script_pubkey() != output.script_pubkey {
            return Err(UnifiedSweepFinalizeError::InputAuthentication);
        }
        authenticated.push((output, definite));
    }
    Ok(authenticated)
}

/// Step 2's fee bounds and dust floor, from the authenticated previous
/// outputs rather than the construction's own total. Returns the fee.
fn economics(
    construction: &UnifiedSweep,
    authenticated: &[(TxOut, Descriptor<DefiniteDescriptorKey>)],
) -> Result<Amount, UnifiedSweepFinalizeError> {
    let total = authenticated
        .iter()
        .try_fold(0u64, |sum, (output, _)| {
            sum.checked_add(output.value.to_sat())
        })
        .ok_or(UnifiedSweepFinalizeError::Economics)?;
    let target = &construction.psbt.unsigned_tx.output[0];
    let value = target.value.to_sat();
    check_economics(
        total,
        value,
        &target.script_pubkey,
        construction.maximum_signed_vbytes,
    )
    .map_err(|_| UnifiedSweepFinalizeError::Economics)?;
    Ok(Amount::from_sat(total - value))
}

fn strip(transaction: &Transaction) -> Transaction {
    let mut unsigned = transaction.clone();
    for input in &mut unsigned.input {
        input.script_sig = ScriptBuf::new();
        input.witness.clear();
    }
    unsigned
}

/// The items of an input's retained witness that may be signatures: the
/// scriptSig pushes of a P2PKH spend, the witness items of a P2WPKH or
/// P2SH-P2WPKH spend, and the witness items before the witness script of a
/// P2WSH spend. Keys, the empty CHECKMULTISIG dummy and the redeem script
/// push are skipped later because they do not parse as DER.
fn retained_items(
    definite: &Descriptor<DefiniteDescriptorKey>,
    input: &TxIn,
) -> Result<Vec<Vec<u8>>, UnifiedSweepFinalizeError> {
    match definite {
        Descriptor::Pkh(_) => input
            .script_sig
            .instructions()
            .map(|instruction| match instruction {
                Ok(Instruction::PushBytes(bytes)) => Ok(bytes.as_bytes().to_vec()),
                _ => Err(UnifiedSweepFinalizeError::InvalidWitness),
            })
            .collect(),
        Descriptor::Wpkh(_) | Descriptor::Sh(_) => {
            Ok(input.witness.iter().map(<[u8]>::to_vec).collect())
        }
        Descriptor::Wsh(_) => Ok(input
            .witness
            .iter()
            .take(input.witness.len().saturating_sub(1))
            .map(<[u8]>::to_vec)
            .collect()),
        Descriptor::Bare(_) | Descriptor::Tr(_) => Err(UnifiedSweepFinalizeError::InvalidWitness),
    }
}

#[cfg(test)]
mod tests;
