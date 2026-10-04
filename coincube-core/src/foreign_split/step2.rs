//! Split step 2: the BTCB2-side sweep of exactly the original outpoints that
//! step 1 claimed on Bitcoin, into one target script (the target Cube's
//! address). Construction, deterministic reconstruction and finalization only.
//!
//! Step 2 has one output, the target, and no change: every claimed coin moves
//! to the target Cube in one transaction. The fee bounds, dust floor and
//! locktime rule are step 1's, applied to the single output and the observed
//! BTCB2 tip.
//!
//! Signatures must be ordinary ECDSA `SIGHASH_ALL`, either implicit or an
//! explicit `0x01` (#585, owner decision F1). `NONE`, `SINGLE`, every
//! `ANYONECANPAY` variant and the BTCB2 unified `ALL|UNIFIED` (`0x21`) are
//! refused. The digest is the one step 1 verifies, and the per-signature
//! checks (key membership, compressed keys on segwit, the Miniscript
//! finalizer and interpreter replay) are step 1's own code:
//!
//! - Knots' unified sighash is opt-in by the `0x20` hash-type bit
//!   ([`crate::unified_sighash::SIGHASH_UNIFIED`]); a hash type without it
//!   keeps the legacy and BIP 143 digests.
//! - The BLAKE2b regtest node accepts Claim's BTCB2 fork sweep signed with
//!   ordinary BIP 143 `SIGHASH_ALL` P2WSH signatures
//!   (`tests/test_btcb2_claim_consensus.py`), and the Claim fork finalizer
//!   verifies such legacy signatures against the BIP 143 digest.
//!
//! The repository has no node vector for P2PKH, P2WPKH or P2SH-P2WPKH on
//! BTCB2; the two-chain regtest (#568 B6) is the node-acceptance check for
//! those shapes.
//!
//! Step 2 is replayable to Bitcoin only if step 1 is not confirmed there: its
//! inputs are spent on Bitcoin by step 1. This module does NOT check that. It
//! reads no chain, proves nothing unspent, checks no confirmation depth, RDTS
//! state, reorg, reservation or target freshness, runs no preflight and grants
//! no signing or broadcast authority. Callers establish all of that (#568 B2
//! and B3b) before any signature is requested or broadcast.

use super::*;

/// Everything that identifies one step 2 apart from its fee and locktime.
#[derive(Debug, Clone, Copy)]
pub struct SplitStep2Inputs<'a> {
    /// Bitcoin Blake2b mainnet or testnet4.
    pub chain: ChainId,
    pub source: &'a SplitSource,
    /// The claimed coins, freshly authenticated; the same shared pre-fork
    /// history step 1 required on both chains.
    pub coins: &'a [SplitCoin],
    /// From the authenticated BTCB2 network anchor; never a constant.
    pub fork_height: u64,
    /// Step 1's claimed prevouts ([`SplitStep1::claimed_prevouts`]). `coins`
    /// must be exactly these.
    pub claimed: &'a [OutPoint],
    /// The target Cube's address script: P2WSH or P2TR, the only Vault
    /// address types. Its ownership and freshness are the caller's to prove;
    /// this module refuses any other type and the foreign wallet's own
    /// scripts near its claimed coins.
    pub target: &'a bitcoin::Script,
}

/// An unsigned step 2 with no public-field or deserialization bypass. It
/// certifies only the construction checks, not live eligibility.
#[derive(Debug, Clone)]
pub struct SplitStep2 {
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

impl SplitStep2 {
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
    /// The unsigned transaction's txid. As in step 1, P2PKH and P2SH-P2WPKH
    /// scriptSigs change it; track the finalized transaction's own txid.
    pub fn txid(&self) -> Txid {
        self.psbt.unsigned_tx.compute_txid()
    }
    /// The spent outpoints, in input order: step 1's claimed prevouts.
    pub fn claimed_prevouts(&self) -> Vec<OutPoint> {
        self.psbt
            .unsigned_tx
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect()
    }
    /// Worst-case signed size the fee was charged for.
    pub fn maximum_signed_vbytes(&self) -> u64 {
        self.maximum_signed_vbytes
    }
    pub fn fee(&self) -> Amount {
        // Economics were checked at construction; this cannot underflow.
        Amount::from_sat(self.total - self.psbt.unsigned_tx.output[0].value.to_sat())
    }
}

struct Plan2 {
    selection: Selection,
    maximum_signed_vbytes: u64,
}

fn target_output(target: &bitcoin::Script, value: Amount) -> Vec<TxOut> {
    vec![TxOut {
        value,
        script_pubkey: target.to_owned(),
    }]
}

/// How far from each claimed coin's index, on both branches, a target is
/// compared with the foreign wallet's own scripts: 100, the foreign
/// scanner's per-branch range end (`DEFAULT_RANGE_END`), not its gap
/// (`DEFAULT_GAP`, 20). Pinned by
/// `source_window_is_pinned_and_reaches_below_a_high_index_coin` (#614 G2,
/// G3). The cost grows with the union of the windows (about 0.3 ms per
/// derivation), so callers run construction off the UI thread (G1).
const SOURCE_WINDOW: u32 = 100;

/// Whether `target` is one of the foreign wallet's own scripts. Only a `wsh`
/// source has P2WSH scripts, and no supported source shape has P2TR ones, so
/// only a P2WSH target of a `wsh` source is compared. A script hash cannot be inverted, so the check derives
/// both branches within [`SOURCE_WINDOW`] of every claimed coin's index,
/// which covers the spent scripts and the step-1 destination. Beyond that, only
/// the caller's binding of the target to the target Cube's own derivation
/// refuses a foreign script (#568 B3b). Shared with the unified sweep.
pub(super) fn is_source_script(
    source: &SplitSource,
    selected: &[Selected],
    target: &bitcoin::Script,
) -> Result<bool, Error> {
    if !target.is_p2wsh() || !matches!(source.external(), Descriptor::Wsh(_)) {
        return Ok(false);
    }
    let mut indices = BTreeSet::new();
    for input in selected {
        let last = input.index.saturating_add(SOURCE_WINDOW).min((1 << 31) - 1);
        indices.extend(input.index.saturating_sub(SOURCE_WINDOW)..=last);
    }
    let mut branches = vec![SplitBranch::External];
    if source.internal().is_some() {
        branches.push(SplitBranch::Internal);
    }
    for branch in branches {
        for &index in &indices {
            if source.derive(branch, index)?.script_pubkey().as_script() == target {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn plan(inputs: &SplitStep2Inputs<'_>) -> Result<Plan2, Error> {
    if !inputs.chain.is_blake2b() {
        return Err(Error::NotBitcoinBlake2b(inputs.chain));
    }
    let selection = select(inputs.source, inputs.coins, inputs.fork_height)?;
    let claimed: BTreeSet<_> = inputs.claimed.iter().copied().collect();
    if claimed.len() != inputs.claimed.len() || claimed != selection.outpoints {
        return Err(Error::ClaimedMismatch);
    }
    let target = inputs.target;
    if !(target.is_p2wsh() || target.is_p2tr())
        || is_source_script(inputs.source, &selection.selected, target)?
    {
        return Err(Error::InvalidTarget);
    }
    let maximum_signed_vbytes =
        maximum_signed_vbytes(&selection.selected, target_output(target, Amount::ZERO))?;
    Ok(Plan2 {
        selection,
        maximum_signed_vbytes,
    })
}

fn build(
    inputs: &SplitStep2Inputs<'_>,
    plan: Plan2,
    value: Amount,
    locktime: LockTime,
) -> Result<SplitStep2, Error> {
    let selected = &plan.selection.selected;
    let tx = unsigned_transaction(selected, target_output(inputs.target, value), locktime);
    let psbt = psbt_for(tx, selected)?;
    Ok(SplitStep2 {
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

/// Build the unsigned BTCB2 step 2 spending exactly step 1's claimed coins to
/// the target, with no change. The fee is `feerate_vb` times the worst-case
/// signed size; inputs are ordered by outpoint and the result is
/// deterministic. `locktime` must be a block height no greater than
/// `btcb2_tip_height`, the observed BTCB2 tip.
pub fn create_split_step2(
    inputs: &SplitStep2Inputs<'_>,
    feerate_vb: u64,
    locktime: LockTime,
    btcb2_tip_height: u32,
) -> Result<SplitStep2, Error> {
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

/// Rebuild an exact recorded step 2 from freshly authenticated coins. Only the
/// target amount and the locktime are read from the record; everything else
/// is rebuilt and the whole transaction must then match. Economics and the
/// locktime (against `btcb2_tip_height`, the current BTCB2 tip) are checked
/// again. This reserves nothing and authorizes no submission.
pub fn reconstruct_split_step2(
    inputs: &SplitStep2Inputs<'_>,
    recorded: &Transaction,
    btcb2_tip_height: u32,
) -> Result<SplitStep2, Error> {
    if recorded.output.len() != 1 {
        return Err(Error::Recorded("Recorded step 2 must have one output"));
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
            "Recorded transaction differs from the owned step-2 construction",
        ));
    }
    Ok(rebuilt)
}

/// A finalized step 2 with verified witnesses. Cryptographic evidence only: it
/// does not prove step 1's confirmation depth, replay safety, relay
/// acceptance, target ownership or freshness, and it grants no broadcast
/// authority.
#[derive(Debug)]
pub struct VerifiedSplitStep2 {
    transaction: Transaction,
    chain: ChainId,
    construction_txid: Txid,
    fee: Amount,
    signatures_per_input: Vec<usize>,
}

impl VerifiedSplitStep2 {
    pub fn transaction(&self) -> &Transaction {
        &self.transaction
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
        self.transaction.vsize()
    }
    pub fn signatures_per_input(&self) -> &[usize] {
        &self.signatures_per_input
    }
}

/// The supplied coins and source must be the construction's: the same source,
/// and, in input order, the same outpoint, derivation and previous
/// transaction for every input, each still shared pre-fork history.
fn check_bound(
    construction: &SplitStep2,
    coins: &[SplitCoin],
    source: &SplitSource,
) -> Result<(), FinalizeError> {
    let mut coins: Vec<_> = coins.iter().collect();
    coins.sort_by_key(|coin| coin.outpoint);
    let psbt = &construction.psbt;
    let bound = construction.source == *source
        && coins.len() == construction.inputs.len()
        && coins.iter().enumerate().all(|(index, coin)| {
            coin.outpoint == psbt.unsigned_tx.input[index].previous_output
                && (coin.branch, coin.index) == construction.inputs[index]
                && psbt.inputs[index].non_witness_utxo.as_ref() == Some(&coin.previous)
                && coin.splittable(construction.fork_height).is_ok()
        });
    if bound {
        Ok(())
    } else {
        Err(FinalizeError::CoinsChanged)
    }
}

/// Accept only an unfinalized partial-signature PSBT of the exact opaque step-2
/// construction, with the same rules as [`finalize_split_step1`]: everything
/// except `partial_sigs` and an absent or explicit `SIGHASH_ALL` request must
/// be identical, and every supplied signature is verified. `coins` and
/// `source` are the caller's current authenticated evidence and must be the
/// construction's own.
pub fn finalize_split_step2<C: secp256k1::Verification>(
    construction: &SplitStep2,
    coins: &[SplitCoin],
    source: &SplitSource,
    signed: &Psbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedSplitStep2, FinalizeError> {
    check_bound(construction, coins, source)?;
    let original = &construction.psbt;
    check_signed_construction(original, signed)?;
    let prevouts =
        verify_partial_signatures(&construction.source, &construction.inputs, signed, secp)?;
    let total = prevouts
        .iter()
        .try_fold(0u64, |sum, output| sum.checked_add(output.value.to_sat()))
        .ok_or(FinalizeError::Economics)?;
    let value = signed.unsigned_tx.output[0].value.to_sat();
    // Defence in depth mirrored from step 1: the exact-construction check
    // above already pins the transaction whose economics create checked.
    check_economics(
        total,
        value,
        &signed.unsigned_tx.output[0].script_pubkey,
        construction.maximum_signed_vbytes,
    )
    .map_err(|_| FinalizeError::Economics)?;
    let (transaction, signatures_per_input) =
        finalize_and_replay(original, signed, &prevouts, secp)?;
    Ok(VerifiedSplitStep2 {
        construction_txid: original.unsigned_tx.compute_txid(),
        transaction,
        chain: construction.chain,
        fee: Amount::from_sat(total - value),
        signatures_per_input,
    })
}

/// Verify a recorded signed step 2 (a journal's bytes) against the exact
/// construction rebuilt from authenticated coins, the step-2 twin of
/// [`verify_split_step1_transaction`]: each input's previous output is
/// authenticated by its txid and the construction's own derivation, the
/// transaction with every scriptSig and witness removed must be the
/// construction, every retained scriptSig and witness is replayed by
/// Miniscript's interpreter (`SIGHASH_ALL` only, keys the input commits to),
/// and economics are checked again. Certifies what [`finalize_split_step2`]
/// does, nothing about the chain.
pub fn verify_split_step2_transaction<C: secp256k1::Verification>(
    construction: &SplitStep2,
    transaction: &Transaction,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedSplitStep2, FinalizeError> {
    let original = &construction.psbt;
    if construction.inputs.len() != original.inputs.len()
        || original.unsigned_tx.input.len() != original.inputs.len()
        || original.unsigned_tx.output.len() != 1
    {
        return Err(FinalizeError::ConstructionChanged);
    }
    let mut prevouts = Vec::with_capacity(original.inputs.len());
    for (index, input) in original.inputs.iter().enumerate() {
        let output = spend::authenticate_previous_output(
            &original.unsigned_tx.input[index].previous_output,
            input.non_witness_utxo.as_ref(),
            input.witness_utxo.as_ref(),
        )
        .map_err(|_| FinalizeError::InputAuthentication)?;
        let (branch, derivation) = construction.inputs[index];
        let definite = construction
            .source
            .derive(branch, derivation)
            .map_err(|_| FinalizeError::InputAuthentication)?;
        if definite.script_pubkey() != output.script_pubkey {
            return Err(FinalizeError::InputAuthentication);
        }
        prevouts.push(output);
    }
    let signatures_per_input = verify_retained_witness(transaction, original, &prevouts, secp)?;
    let total = prevouts
        .iter()
        .try_fold(0u64, |sum, output| sum.checked_add(output.value.to_sat()))
        .ok_or(FinalizeError::Economics)?;
    let target = &original.unsigned_tx.output[0];
    let value = target.value.to_sat();
    check_economics(
        total,
        value,
        &target.script_pubkey,
        construction.maximum_signed_vbytes,
    )
    .map_err(|_| FinalizeError::Economics)?;
    Ok(VerifiedSplitStep2 {
        construction_txid: original.unsigned_tx.compute_txid(),
        transaction: transaction.clone(),
        chain: construction.chain,
        fee: Amount::from_sat(total - value),
        signatures_per_input,
    })
}

#[cfg(test)]
mod tests;
