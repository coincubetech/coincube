//! Signature evidence for an ancestry construction; no chain qualification.
use super::*;
use crate::claim_spend::AncestrySelfTransfer;
use bitcoin::{psbt::Psbt, OutPoint};
use std::collections::BTreeSet;

/// Kept distinct from OP_RETURN evidence so callers must explicitly integrate
/// fresh ancestry qualification before using it in a submission workflow.
#[derive(Debug)]
pub struct VerifiedAncestryTransfer {
    verified: VerifiedPoisonTransfer,
    poison_input: OutPoint,
    claimed_prevouts: Vec<OutPoint>,
}
impl VerifiedAncestryTransfer {
    pub fn transaction(&self) -> &Transaction {
        self.verified.transaction()
    }
    pub fn chain(&self) -> ChainId {
        self.verified.chain()
    }
    pub fn descriptor(&self) -> &CoincubeDescriptor {
        self.verified.descriptor()
    }
    pub fn construction_txid(&self) -> Txid {
        self.verified.construction_txid()
    }
    pub fn fee(&self) -> Amount {
        self.verified.fee()
    }
    pub fn vsize(&self) -> usize {
        self.verified.vsize()
    }
    pub fn signatures_per_input(&self) -> &[usize] {
        self.verified.signatures_per_input()
    }
    pub fn poison_input(&self) -> OutPoint {
        self.poison_input
    }
    pub fn claimed_prevouts(&self) -> &[OutPoint] {
        &self.claimed_prevouts
    }
}
fn bind(
    construction: &AncestrySelfTransfer,
    verified: VerifiedPoisonTransfer,
) -> VerifiedAncestryTransfer {
    VerifiedAncestryTransfer {
        verified,
        poison_input: construction.poison_input(),
        claimed_prevouts: construction.claimed_prevouts().to_vec(),
    }
}
/// Check an in-progress Bitcoin ancestry PSBT before another signer handles
/// it. Incomplete signatures are allowed, but the owned transaction and all
/// non-signature metadata must still match the original construction.
pub fn validate_ancestry_transfer_signing(
    construction: &AncestrySelfTransfer,
    signed: &Psbt,
) -> Result<(), FinalizeError> {
    validate_transfer_signing(construction.psbt(), construction.descriptor(), signed)
}

/// Authenticate every supplied signature and interpret the retained witness,
/// requiring exact owned metadata and SIGHASH_ALL. Does not prove exclusivity,
/// maturity, unspentness, deployment activity, or permission to broadcast.
pub fn finalize_ancestry_transfer<C: secp256k1::Verification>(
    construction: &AncestrySelfTransfer,
    signed: &Psbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedAncestryTransfer, FinalizeError> {
    finalize_transfer(
        construction.psbt(),
        construction.descriptor(),
        construction.chain(),
        signed,
        secp,
    )
    .map(|verified| bind(construction, verified))
}
/// Re-interpret recovered public transaction bytes against newly reconstructed
/// owned metadata. Neither a journal nor matching txid is signature evidence.
pub fn verify_ancestry_transaction<C: secp256k1::Verification>(
    construction: &AncestrySelfTransfer,
    transaction: &Transaction,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedAncestryTransfer, FinalizeError> {
    verify_transfer(
        construction.psbt(),
        construction.descriptor(),
        construction.chain(),
        transaction,
        secp,
    )
    .map(|verified| bind(construction, verified))
}

/// Both ordinary Claim transactions after cryptographic finalization. This
/// does not include a current two-chain ancestry proof or submission consent.
#[derive(Debug)]
pub struct VerifiedAncestryClaimPair {
    bitcoin: VerifiedAncestryTransfer,
    fork: VerifiedClaimForkSweep,
}
impl VerifiedAncestryClaimPair {
    pub fn bitcoin(&self) -> &VerifiedAncestryTransfer {
        &self.bitcoin
    }
    pub fn fork(&self) -> &VerifiedClaimForkSweep {
        &self.fork
    }
}

#[derive(Debug)]
pub enum ClaimPairFinalizeError {
    ConstructionChanged,
    Bitcoin(FinalizeError),
    Fork(ClaimForkFinalizeError),
}
impl std::fmt::Display for ClaimPairFinalizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Ancestry Claim pair finalization refused: {self:?}")
    }
}
impl std::error::Error for ClaimPairFinalizeError {}

/// Finalize the exact `S+B` and `S+F` Claim pair as one unit. In particular,
/// the BTCB2 sweep must have the same shared inputs as the Bitcoin transfer,
/// one *different* funding input, and its saved Bitcoin step-one txid. This
/// verifies signatures but deliberately grants no live chain or signer access.
pub fn finalize_ancestry_claim_pair<C: secp256k1::Verification>(
    bitcoin: &AncestrySelfTransfer,
    fork: &crate::claim_spend::ClaimForkSweep,
    bitcoin_signed: &Psbt,
    fork_signed: &crate::psbt_unified::UnifiedPsbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedAncestryClaimPair, ClaimPairFinalizeError> {
    validate_ancestry_claim_pair_signing(bitcoin, fork, bitcoin_signed, fork_signed)?;
    let bitcoin = finalize_ancestry_transfer(bitcoin, bitcoin_signed, secp)
        .map_err(ClaimPairFinalizeError::Bitcoin)?;
    let fork =
        finalize_claim_fork_sweep(fork, fork_signed, secp).map_err(ClaimPairFinalizeError::Fork)?;
    Ok(VerifiedAncestryClaimPair { bitcoin, fork })
}

/// Validate the exact paired construction after each signer round trip.
/// Incomplete signatures are allowed. This is not live ancestry evidence or
/// permission to offer either transaction to a signer or network.
pub fn validate_ancestry_claim_pair_signing(
    bitcoin: &AncestrySelfTransfer,
    fork: &crate::claim_spend::ClaimForkSweep,
    bitcoin_signed: &Psbt,
    fork_signed: &crate::psbt_unified::UnifiedPsbt,
) -> Result<(), ClaimPairFinalizeError> {
    let shared: BTreeSet<_> = bitcoin.claimed_prevouts().iter().copied().collect();
    let fork_inputs: BTreeSet<_> = fork
        .psbt()
        .unsigned_tx
        .input
        .iter()
        .map(|input| input.previous_output)
        .collect();
    let marker = fork
        .fork_marker()
        .ok_or(ClaimPairFinalizeError::ConstructionChanged)?;
    if !matches!(
        (bitcoin.chain(), fork.chain()),
        (ChainId::Bitcoin, ChainId::BitcoinBlake2b)
            | (ChainId::Testnet4, ChainId::BitcoinBlake2bTestnet4)
    ) || bitcoin.descriptor() != fork.descriptor()
        || fork.bitcoin_step1() != bitcoin.psbt().unsigned_tx.compute_txid()
        || marker == bitcoin.poison_input()
        || shared.is_empty()
        || shared.len() != bitcoin.claimed_prevouts().len()
        || shared.contains(&marker)
        || fork_inputs.len() != shared.len() + 1
        || !shared.iter().all(|outpoint| fork_inputs.contains(outpoint))
        || !fork_inputs.contains(&marker)
        || bitcoin.psbt().unsigned_tx.output.len() != 1
        || fork.psbt().unsigned_tx.output.len() != 1
        || bitcoin.psbt().unsigned_tx.output[0]
            .script_pubkey
            .is_op_return()
        || fork.psbt().unsigned_tx.output[0]
            .script_pubkey
            .is_op_return()
    {
        return Err(ClaimPairFinalizeError::ConstructionChanged);
    }
    validate_ancestry_transfer_signing(bitcoin, bitcoin_signed)
        .map_err(ClaimPairFinalizeError::Bitcoin)?;
    validate_claim_fork_signing(fork, fork_signed).map_err(ClaimPairFinalizeError::Fork)?;
    Ok(())
}
