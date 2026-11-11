//! Shared verification of retained ordinary SIGHASH_ALL witnesses.
//! Callers independently bind the transaction to their construction first.
use miniscript::{
    bitcoin::{
        self,
        psbt::Psbt,
        secp256k1,
        sighash::{EcdsaSighashType, Prevouts},
        Transaction,
    },
    interpreter::{Interpreter, KeySigPair, SatisfiedConstraint},
};

#[derive(Debug)]
pub(crate) struct InvalidWitness;

pub(crate) fn verify_all<C: secp256k1::Verification>(
    transaction: &Transaction,
    original: &Psbt,
    prevouts: &[bitcoin::TxOut],
    secp: &secp256k1::Secp256k1<C>,
) -> Result<Vec<usize>, InvalidWitness> {
    let mut counts = Vec::with_capacity(transaction.input.len());
    for (index, input) in transaction.input.iter().enumerate() {
        counts.push(verify_retained_input(
            transaction,
            original,
            prevouts,
            secp,
            index,
            input,
        )?);
    }
    Ok(counts)
}

fn verify_retained_input<C: secp256k1::Verification>(
    transaction: &Transaction,
    original: &Psbt,
    prevouts: &[bitcoin::TxOut],
    secp: &secp256k1::Secp256k1<C>,
    index: usize,
    input: &bitcoin::TxIn,
) -> Result<usize, InvalidWitness> {
    let interpreter = Interpreter::from_txdata(
        &prevouts[index].script_pubkey,
        &input.script_sig,
        &input.witness,
        input.sequence,
        transaction.lock_time,
    )
    .map_err(|_| InvalidWitness)?;
    let mut signatures = 0;
    for constraint in interpreter.iter(secp, transaction, index, &Prevouts::All(prevouts)) {
        let pair = match constraint.map_err(|_| InvalidWitness)? {
            SatisfiedConstraint::PublicKey { key_sig }
            | SatisfiedConstraint::PublicKeyHash { key_sig, .. } => Some(key_sig),
            _ => None,
        };
        if let Some(pair) = pair {
            match pair {
                KeySigPair::Ecdsa(key, sig)
                    if sig.sighash_type == EcdsaSighashType::All
                        && original.inputs[index]
                            .bip32_derivation
                            .contains_key(&key.inner) =>
                {
                    signatures += 1
                }
                _ => return Err(InvalidWitness),
            }
        }
    }
    if signatures == 0 {
        return Err(InvalidWitness);
    }
    Ok(signatures)
}
