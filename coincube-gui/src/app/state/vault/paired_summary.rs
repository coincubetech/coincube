//! Read-only facts from the exact signed ordinary transaction retained by a
//! Claim or Split journal. This never restores live proof or spend authority.

use coincube_core::miniscript::bitcoin::{Address, Amount, Network, Transaction};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedOutputSummary {
    pub destination: String,
    pub amount_sats: u64,
}

impl PairedOutputSummary {
    pub fn from_signed(transaction: &Transaction) -> Option<Self> {
        if transaction.output.len() != 1
            || transaction.output[0].script_pubkey.is_op_return()
            || transaction.output[0].value == Amount::ZERO
        {
            return None;
        }
        let output = &transaction.output[0];
        let destination = Address::from_script(&output.script_pubkey, Network::Bitcoin)
            .ok()?
            .to_string();
        Some(Self {
            destination,
            amount_sats: output.value.to_sat(),
        })
    }

    /// A broadcast review must display both exact destinations before it
    /// offers either chain's transaction for submission.
    pub fn from_signed_pair(bitcoin: &Transaction, fork: &Transaction) -> Option<(Self, Self)> {
        Some((Self::from_signed(bitcoin)?, Self::from_signed(fork)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::{
        absolute::LockTime, hashes::Hash, transaction::Version, Amount, PubkeyHash, ScriptBuf,
        TxOut,
    };

    fn signed_with_output(script_pubkey: ScriptBuf) -> Transaction {
        Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![],
            output: vec![TxOut {
                value: Amount::from_sat(10_000),
                script_pubkey,
            }],
        }
    }

    #[test]
    fn paired_broadcast_review_requires_both_destinations() {
        let owned = signed_with_output(ScriptBuf::new_p2pkh(&PubkeyHash::all_zeros()));
        let unrenderable = signed_with_output(ScriptBuf::new());
        let mut zero_value = owned.clone();
        zero_value.output[0].value = Amount::ZERO;
        assert!(PairedOutputSummary::from_signed_pair(&owned, &owned).is_some());
        assert!(PairedOutputSummary::from_signed_pair(&owned, &unrenderable).is_none());
        assert!(PairedOutputSummary::from_signed_pair(&unrenderable, &owned).is_none());
        assert!(PairedOutputSummary::from_signed_pair(&owned, &zero_value).is_none());
    }
}
