//! Signature evidence for an ancestry construction; no chain qualification.
use super::*;
use crate::claim_spend::AncestrySelfTransfer;
use bitcoin::OutPoint;

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
