//! Ordinary Bitcoin split step 1: shared pre-fork inputs plus one post-fork
//! Bitcoin funding input, paid to a fresh source-wallet address. This module
//! checks construction and signatures, not live Bitcoin-only ancestry,
//! current UTXO status, confirmations, reservations or broadcast readiness.

use super::*;
use crate::claim_ancestry::CoinbaseDependency;

mod step2;
pub use step2::{
    create_ancestry_split_step2, finalize_ancestry_split_step2, reconstruct_ancestry_split_step2,
    verify_ancestry_split_step2_transaction, AncestrySplitStep2, AncestrySplitStep2Inputs,
    VerifiedAncestrySplitStep2,
};

/// Inputs of an ancestry-based Bitcoin step 1. `dependency` establishes a
/// structural path from `funding` to a coinbase only. A caller must separately
/// verify canonical and differing coinbases on both chains, ownership, UTXO
/// status and reservations before presenting this PSBT to a signer.
#[derive(Debug, Clone, Copy)]
pub struct AncestrySplitInputs<'a> {
    pub chain: ChainId,
    pub source: &'a SplitSource,
    pub shared: &'a [SplitCoin],
    pub funding: &'a SplitCoin,
    pub dependency: &'a CoinbaseDependency,
    pub fork_height: u64,
    pub destination: u32,
}

/// An opaque, structurally owned `S + B` construction. It never conveys
/// proof that `B` is absent on BTCB2 or permission to sign or broadcast.
#[derive(Debug, Clone)]
pub struct AncestrySplitStep1 {
    psbt: Psbt,
    chain: ChainId,
    source: SplitSource,
    inputs: Vec<(SplitBranch, u32)>,
    shared: Vec<OutPoint>,
    funding: OutPoint,
    destination: u32,
    fork_height: u64,
    maximum_signed_vbytes: u64,
    feerate_vb: u64,
    total: u64,
}

impl AncestrySplitStep1 {
    pub fn psbt(&self) -> &Psbt {
        &self.psbt
    }
    pub fn chain(&self) -> ChainId {
        self.chain
    }
    pub fn source(&self) -> &SplitSource {
        &self.source
    }
    pub fn destination(&self) -> u32 {
        self.destination
    }
    pub fn fork_height(&self) -> u64 {
        self.fork_height
    }
    pub fn funding_prevout(&self) -> OutPoint {
        self.funding
    }
    pub fn funding_derivation(&self) -> Option<(SplitBranch, u32)> {
        self.psbt
            .unsigned_tx
            .input
            .iter()
            .zip(&self.inputs)
            .find(|(input, _)| input.previous_output == self.funding)
            .map(|(_, derivation)| *derivation)
    }
    /// Exactly the shared pre-fork inputs that BTCB2 step 2 must also spend.
    pub fn claimed_prevouts(&self) -> &[OutPoint] {
        &self.shared
    }
    pub fn txid(&self) -> Txid {
        self.psbt.unsigned_tx.compute_txid()
    }
    pub fn maximum_signed_vbytes(&self) -> u64 {
        self.maximum_signed_vbytes
    }
    pub fn fee(&self) -> Amount {
        Amount::from_sat(self.total - self.psbt.unsigned_tx.output[0].value.to_sat())
    }
}

/// Estimate the exact worst-case signed size of a future `S + B` or `S + F`
/// one-output transaction before its funding coin exists. This authenticates
/// the shared coins and derives the reserved funding script, but does not
/// establish that a later deposit is chain-specific, confirmed or unspent.
/// Recalculate the actual fee from the constructed PSBT at review.
pub fn estimate_ancestry_signed_vbytes(
    source: &SplitSource,
    shared: &[SplitCoin],
    fork_height: u64,
    funding_index: u32,
    destination: &bitcoin::Script,
) -> Result<u64, Error> {
    let mut selected = select(source, shared, fork_height)?.selected;
    let definite = source.derive(SplitBranch::External, funding_index)?;
    let funding_script = definite.script_pubkey();
    if funding_script == *destination {
        return Err(Error::DestinationNotFresh);
    }
    selected.push(Selected {
        // This outpoint is only a fixed-width placeholder for size
        // estimation. No transaction can be constructed from this entry.
        outpoint: OutPoint::null(),
        branch: SplitBranch::External,
        index: funding_index,
        previous: Transaction {
            version: transaction::Version::TWO,
            lock_time: LockTime::ZERO,
            input: Vec::new(),
            output: Vec::new(),
        },
        output: TxOut {
            value: Amount::ZERO,
            script_pubkey: funding_script,
        },
        definite,
    });
    maximum_signed_vbytes(
        &selected,
        vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: destination.to_owned(),
        }],
    )
}

struct Plan {
    selected: Vec<Selected>,
    shared: Vec<OutPoint>,
    destination: Descriptor<DefiniteDescriptorKey>,
    total: u64,
    funding_amount: u64,
    maximum_signed_vbytes: u64,
}

fn plan(inputs: &AncestrySplitInputs<'_>) -> Result<Plan, Error> {
    if !matches!(inputs.chain, ChainId::Bitcoin | ChainId::Testnet4) {
        return Err(Error::UnsupportedChain(inputs.chain));
    }
    let Selection {
        mut selected,
        outpoints,
        total,
    } = select(inputs.source, inputs.shared, inputs.fork_height)?;
    let funding = inputs.funding;
    if outpoints.contains(&funding.outpoint) {
        return Err(Error::DuplicateInput(funding.outpoint));
    }
    if funding.outpoint != inputs.dependency.selected() {
        return Err(Error::InvalidFunding(
            "dependency does not name the funding input",
        ));
    }
    if funding
        .bitcoin_block
        .is_none_or(|block| block.height < inputs.fork_height)
        || funding.btcb2_block.is_some()
    {
        return Err(Error::InvalidFunding(
            "funding input must be post-fork Bitcoin history",
        ));
    }
    let definite = inputs.source.derive(funding.branch, funding.index)?;
    let output =
        spend::authenticate_previous_output(&funding.outpoint, Some(&funding.previous), None)
            .map_err(|reason| Error::InputAuthentication {
                outpoint: funding.outpoint,
                reason,
            })?;
    if definite.script_pubkey() != output.script_pubkey {
        return Err(Error::ScriptMismatch(funding.outpoint));
    }
    let funding_amount = output.value.to_sat();
    let total = total
        .checked_add(funding_amount)
        .filter(|sum| *sum <= Amount::MAX_MONEY.to_sat())
        .ok_or(Error::Economics)?;
    selected.push(Selected {
        outpoint: funding.outpoint,
        branch: funding.branch,
        index: funding.index,
        previous: funding.previous.clone(),
        output,
        definite,
    });
    selected.sort_by_key(|input| input.outpoint);
    let destination = inputs
        .source
        .derive(SplitBranch::External, inputs.destination)?;
    let script = destination.script_pubkey();
    if selected
        .iter()
        .any(|input| input.output.script_pubkey == script)
    {
        return Err(Error::DestinationNotFresh);
    }
    let maximum_signed_vbytes = maximum_signed_vbytes(
        &selected,
        vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: script,
        }],
    )?;
    Ok(Plan {
        selected,
        shared: outpoints.into_iter().collect(),
        destination,
        total,
        funding_amount,
        maximum_signed_vbytes,
    })
}

fn build(
    inputs: &AncestrySplitInputs<'_>,
    plan: Plan,
    value: Amount,
    locktime: LockTime,
    feerate_vb: u64,
) -> Result<AncestrySplitStep1, Error> {
    let transaction = unsigned_transaction(
        &plan.selected,
        vec![TxOut {
            value,
            script_pubkey: plan.destination.script_pubkey(),
        }],
        locktime,
    );
    let mut psbt = psbt_for(transaction, &plan.selected)?;
    psbt.update_output_with_descriptor(0, &plan.destination)
        .map_err(|_| Error::UnsupportedDescriptor)?;
    Ok(AncestrySplitStep1 {
        psbt,
        chain: inputs.chain,
        source: inputs.source.clone(),
        inputs: plan
            .selected
            .iter()
            .map(|input| (input.branch, input.index))
            .collect(),
        shared: plan.shared,
        funding: inputs.funding.outpoint,
        destination: inputs.destination,
        fork_height: inputs.fork_height,
        maximum_signed_vbytes: plan.maximum_signed_vbytes,
        feerate_vb,
        total: plan.total,
    })
}

/// Build the exact `S + B` transaction with one owned output and no data
/// output. The fee is calculated from worst-case signed virtual size.
pub fn create_ancestry_split_step1(
    inputs: &AncestrySplitInputs<'_>,
    feerate_vb: u64,
    locktime: LockTime,
    bitcoin_tip_height: u32,
) -> Result<AncestrySplitStep1, Error> {
    check_locktime(locktime, bitcoin_tip_height)?;
    let plan = plan(inputs)?;
    if !(1..=spend::MAX_FEERATE).contains(&feerate_vb) {
        return Err(Error::Economics);
    }
    let fee = plan
        .maximum_signed_vbytes
        .checked_mul(feerate_vb)
        .ok_or(Error::Economics)?;
    if plan.funding_amount < fee {
        return Err(Error::FundingBelowFee(ChainId::Bitcoin));
    }
    let value = plan.total.checked_sub(fee).ok_or(Error::Economics)?;
    check_economics(
        plan.total,
        value,
        &plan.destination.script_pubkey(),
        plan.maximum_signed_vbytes,
    )?;
    build(inputs, plan, Amount::from_sat(value), locktime, feerate_vb)
}

/// Reconstruct all inputs, outputs and PSBT metadata from fresh observations.
/// Only the recorded locktime is reused. The persisted fee rate is required
/// to rebuild the exact output amount, and the complete unsigned transaction
/// must match. No cached spend authority survives reconstruction.
pub fn reconstruct_ancestry_split_step1(
    inputs: &AncestrySplitInputs<'_>,
    recorded: &Transaction,
    feerate_vb: u64,
    bitcoin_tip_height: u32,
) -> Result<AncestrySplitStep1, Error> {
    if recorded.output.len() != 1 {
        return Err(Error::Recorded("Ancestry step 1 must have one output"));
    }
    let rebuilt =
        create_ancestry_split_step1(inputs, feerate_vb, recorded.lock_time, bitcoin_tip_height)?;
    if rebuilt.psbt.unsigned_tx != *recorded {
        return Err(Error::Recorded(
            "Recorded transaction differs from the owned ancestry step-1 construction",
        ));
    }
    Ok(rebuilt)
}

/// Verify a signed PSBT against the exact construction and replay all final
/// witnesses. Structural signature evidence only; it grants no chain action.
pub fn finalize_ancestry_split_step1<C: secp256k1::Verification>(
    construction: &AncestrySplitStep1,
    inputs: &AncestrySplitInputs<'_>,
    signed: &Psbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedSplitStep1, FinalizeError> {
    check_bound(construction, inputs)?;
    let original = &construction.psbt;
    check_signed_construction(original, signed)?;
    let prevouts =
        verify_partial_signatures(&construction.source, &construction.inputs, signed, secp)?;
    let total = prevouts
        .iter()
        .try_fold(0u64, |sum, output| sum.checked_add(output.value.to_sat()))
        .ok_or(FinalizeError::Economics)?;
    let destination = &signed.unsigned_tx.output[0];
    check_economics(
        total,
        destination.value.to_sat(),
        &destination.script_pubkey,
        construction.maximum_signed_vbytes,
    )
    .map_err(|_| FinalizeError::Economics)?;
    let (transaction, signatures_per_input) =
        finalize_and_replay(original, signed, &prevouts, secp)?;
    Ok(VerifiedSplitStep1 {
        construction_txid: original.unsigned_tx.compute_txid(),
        transaction,
        chain: construction.chain,
        fee: Amount::from_sat(total - destination.value.to_sat()),
        signatures_per_input,
    })
}

/// Verify finalized recorded bytes against a freshly reconstructed step 1.
pub fn verify_ancestry_split_step1_transaction<C: secp256k1::Verification>(
    construction: &AncestrySplitStep1,
    inputs: &AncestrySplitInputs<'_>,
    transaction: &Transaction,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedSplitStep1, FinalizeError> {
    check_bound(construction, inputs)?;
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
    let destination = &original.unsigned_tx.output[0];
    check_economics(
        total,
        destination.value.to_sat(),
        &destination.script_pubkey,
        construction.maximum_signed_vbytes,
    )
    .map_err(|_| FinalizeError::Economics)?;
    Ok(VerifiedSplitStep1 {
        construction_txid: original.unsigned_tx.compute_txid(),
        transaction: transaction.clone(),
        chain: construction.chain,
        fee: Amount::from_sat(total - destination.value.to_sat()),
        signatures_per_input,
    })
}

/// Cryptographically finalized ordinary Split transactions, without current
/// two-chain proof, reservations, or permission to broadcast either side.
#[derive(Debug)]
pub struct VerifiedAncestrySplitPair {
    bitcoin: VerifiedSplitStep1,
    fork: VerifiedAncestrySplitStep2,
}
impl VerifiedAncestrySplitPair {
    pub fn bitcoin(&self) -> &VerifiedSplitStep1 {
        &self.bitcoin
    }
    pub fn fork(&self) -> &VerifiedAncestrySplitStep2 {
        &self.fork
    }
}

/// Finalize both signed Split PSBTs as one exact `S+B` / `S+F` pair. A
/// mismatched source, shared set, or funding input is refused before either
/// artifact can be returned. Existing per-chain finalizers authenticate every
/// signature and all PSBT metadata; this function does no chain reads.
#[allow(clippy::too_many_arguments)]
pub fn finalize_ancestry_split_pair<C: secp256k1::Verification>(
    bitcoin: &AncestrySplitStep1,
    bitcoin_inputs: &AncestrySplitInputs<'_>,
    bitcoin_signed: &Psbt,
    fork: &AncestrySplitStep2,
    fork_inputs: &AncestrySplitStep2Inputs<'_>,
    fork_signed: &Psbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedAncestrySplitPair, FinalizeError> {
    validate_ancestry_split_pair_signing(
        bitcoin,
        bitcoin_inputs,
        bitcoin_signed,
        fork,
        fork_inputs,
        fork_signed,
    )?;
    let bitcoin = finalize_ancestry_split_step1(bitcoin, bitcoin_inputs, bitcoin_signed, secp)?;
    let fork = finalize_ancestry_split_step2(fork, fork_inputs, fork_signed, secp)?;
    Ok(VerifiedAncestrySplitPair { bitcoin, fork })
}

/// Check a partially signed Split pair against the exact two ordinary
/// constructions and current source inputs. Incomplete signatures are
/// allowed; live ancestry, wallet reservations and signer admission remain
/// separate requirements.
#[allow(clippy::too_many_arguments)]
pub fn validate_ancestry_split_pair_signing(
    bitcoin: &AncestrySplitStep1,
    bitcoin_inputs: &AncestrySplitInputs<'_>,
    bitcoin_signed: &Psbt,
    fork: &AncestrySplitStep2,
    fork_inputs: &AncestrySplitStep2Inputs<'_>,
    fork_signed: &Psbt,
) -> Result<(), FinalizeError> {
    let shared: BTreeSet<_> = bitcoin.claimed_prevouts().iter().copied().collect();
    let fork_shared: BTreeSet<_> = fork.claimed_prevouts().iter().copied().collect();
    if !matches!(
        (bitcoin.chain(), fork.chain()),
        (ChainId::Bitcoin, ChainId::BitcoinBlake2b)
            | (ChainId::Testnet4, ChainId::BitcoinBlake2bTestnet4)
    ) || bitcoin.source() != fork.source()
        || bitcoin.fork_height() != fork.fork_height()
        || shared.is_empty()
        || shared.len() != bitcoin.claimed_prevouts().len()
        || fork_shared.len() != fork.claimed_prevouts().len()
        || shared != fork_shared
        || shared.contains(&bitcoin.funding_prevout())
        || shared.contains(&fork.funding_prevout())
        || bitcoin.funding_prevout() == fork.funding_prevout()
        || bitcoin.psbt().unsigned_tx.output.len() != 1
        || fork.psbt().unsigned_tx.output.len() != 1
        || bitcoin.psbt().unsigned_tx.output[0]
            .script_pubkey
            .is_op_return()
        || fork.psbt().unsigned_tx.output[0]
            .script_pubkey
            .is_op_return()
    {
        return Err(FinalizeError::ConstructionChanged);
    }
    check_bound(bitcoin, bitcoin_inputs)?;
    step2::check_bound(fork, fork_inputs)?;
    check_signed_construction(bitcoin.psbt(), bitcoin_signed)?;
    check_signed_construction(fork.psbt(), fork_signed)?;
    Ok(())
}

/// Merge independently returned ordinary Split signatures without accepting
/// altered PSBT metadata, conflicting signatures or a one-sided threshold as
/// completion. Live ancestry and signer admission are separate checks.
#[derive(Debug)]
pub struct CombinedAncestrySplitPair {
    pub bitcoin: Psbt,
    pub fork: Psbt,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombineAncestrySplitError {
    TooManyReturns,
    UnsignedReturn,
    ConflictingSignature { chain: ChainId, input: usize },
    Invalid(FinalizeError),
}

#[allow(clippy::too_many_arguments)]
pub fn combine_ancestry_split_pair<C: secp256k1::Verification>(
    bitcoin: &AncestrySplitStep1,
    bitcoin_inputs: &AncestrySplitInputs<'_>,
    bitcoin_returns: &[Psbt],
    fork: &AncestrySplitStep2,
    fork_inputs: &AncestrySplitStep2Inputs<'_>,
    fork_returns: &[Psbt],
    secp: &secp256k1::Secp256k1<C>,
) -> Result<CombinedAncestrySplitPair, CombineAncestrySplitError> {
    const MAX_RETURNS: usize = 16;
    if bitcoin_returns.len() > MAX_RETURNS || fork_returns.len() > MAX_RETURNS {
        return Err(CombineAncestrySplitError::TooManyReturns);
    }
    let mut bitcoin_signed = bitcoin.psbt().clone();
    let mut fork_signed = fork.psbt().clone();
    for (chain, returns, aggregate) in [
        (ChainId::Bitcoin, bitcoin_returns, &mut bitcoin_signed),
        (ChainId::BitcoinBlake2b, fork_returns, &mut fork_signed),
    ] {
        for returned in returns {
            if returned
                .inputs
                .iter()
                .all(|input| input.partial_sigs.is_empty())
            {
                return Err(CombineAncestrySplitError::UnsignedReturn);
            }
            let checked = match chain {
                ChainId::Bitcoin => {
                    finalize_ancestry_split_step1(bitcoin, bitcoin_inputs, returned, secp)
                        .map(|_| ())
                }
                ChainId::BitcoinBlake2b => {
                    finalize_ancestry_split_step2(fork, fork_inputs, returned, secp).map(|_| ())
                }
                _ => unreachable!(),
            };
            if !matches!(checked, Ok(()) | Err(FinalizeError::Unsatisfied)) {
                return Err(CombineAncestrySplitError::Invalid(checked.unwrap_err()));
            }
            for (index, (input, supplied)) in aggregate
                .inputs
                .iter_mut()
                .zip(&returned.inputs)
                .enumerate()
            {
                for (key, signature) in &supplied.partial_sigs {
                    if input
                        .partial_sigs
                        .get(key)
                        .is_some_and(|old| old != signature)
                    {
                        return Err(CombineAncestrySplitError::ConflictingSignature {
                            chain,
                            input: index,
                        });
                    }
                    input.partial_sigs.insert(*key, *signature);
                }
            }
        }
    }
    validate_ancestry_split_pair_signing(
        bitcoin,
        bitcoin_inputs,
        &bitcoin_signed,
        fork,
        fork_inputs,
        &fork_signed,
    )
    .map_err(CombineAncestrySplitError::Invalid)?;
    let bitcoin_complete =
        match finalize_ancestry_split_step1(bitcoin, bitcoin_inputs, &bitcoin_signed, secp) {
            Ok(_) => true,
            Err(FinalizeError::Unsatisfied) => false,
            Err(error) => return Err(CombineAncestrySplitError::Invalid(error)),
        };
    let fork_complete = match finalize_ancestry_split_step2(fork, fork_inputs, &fork_signed, secp) {
        Ok(_) => true,
        Err(FinalizeError::Unsatisfied) => false,
        Err(error) => return Err(CombineAncestrySplitError::Invalid(error)),
    };
    Ok(CombinedAncestrySplitPair {
        bitcoin: bitcoin_signed,
        fork: fork_signed,
        complete: bitcoin_complete && fork_complete,
    })
}

fn check_bound(
    construction: &AncestrySplitStep1,
    inputs: &AncestrySplitInputs<'_>,
) -> Result<(), FinalizeError> {
    let rebuilt = create_ancestry_split_step1(
        inputs,
        construction.feerate_vb,
        construction.psbt.unsigned_tx.lock_time,
        u32::MAX,
    )
    .map_err(|_| FinalizeError::CoinsChanged)?;
    if rebuilt.psbt == construction.psbt
        && rebuilt.chain == construction.chain
        && rebuilt.source == construction.source
        && rebuilt.shared == construction.shared
        && rebuilt.funding == construction.funding
        && rebuilt.fork_height == construction.fork_height
    {
        Ok(())
    } else {
        Err(FinalizeError::CoinsChanged)
    }
}
