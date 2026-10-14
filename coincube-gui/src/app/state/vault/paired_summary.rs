//! Read-only facts from the exact signed ordinary transaction retained by a
//! Claim or Split journal. This never restores live proof or spend authority.

use coincube_core::miniscript::bitcoin::{Address, Network, Transaction};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedOutputSummary {
    pub destination: String,
    pub amount_sats: u64,
}

impl PairedOutputSummary {
    pub fn from_signed(transaction: &Transaction) -> Option<Self> {
        if transaction.output.len() != 1 || transaction.output[0].script_pubkey.is_op_return() {
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
}
