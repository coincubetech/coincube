//! One-use authorization for a single signer dispatch of an exact ordinary
//! ancestry PSBT. Only a fresh pair requalification may issue this permit.
//! It does not authorize finalization, journal writes or network submission.

use coincube_core::{
    chain::ChainId,
    miniscript::bitcoin::{
        hashes::{sha256, Hash},
        psbt::Psbt,
    },
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use tokio::sync::watch;

#[allow(dead_code)] // Production construction is staged with the paired signer panel.
const DISPATCH_LIFETIME: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub struct PairedSigningPermit {
    chain: ChainId,
    psbt: sha256::Hash,
    generation: watch::Receiver<u64>,
    expected: u64,
    expires: Instant,
    consumed: AtomicBool,
}

impl PairedSigningPermit {
    #[allow(dead_code)] // Production construction is staged with the paired signer panel.
    pub(super) fn new(
        chain: ChainId,
        psbt: &Psbt,
        generation: watch::Receiver<u64>,
        expected: u64,
    ) -> Self {
        Self {
            chain,
            psbt: sha256::Hash::hash(&psbt.serialize()),
            generation,
            expected,
            expires: Instant::now() + DISPATCH_LIFETIME,
            consumed: AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        chain: ChainId,
        psbt: &Psbt,
        generation: watch::Receiver<u64>,
        expected: u64,
    ) -> Self {
        Self::new(chain, psbt, generation, expected)
    }

    pub(crate) fn matches(&self, chain: ChainId, psbt: &Psbt) -> bool {
        self.chain == chain
            && sha256::Hash::hash(&psbt.serialize()) == self.psbt
            && *self.generation.borrow() == self.expected
            && matches!(self.generation.has_changed(), Ok(false))
            && Instant::now() < self.expires
    }

    pub(crate) fn consume(&self, chain: ChainId, psbt: &Psbt) -> bool {
        self.matches(chain, psbt)
            && self
                .consumed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::{
        absolute, transaction, Amount, ScriptBuf, Transaction, TxIn, TxOut,
    };

    fn psbt() -> Psbt {
        Psbt::from_unsigned_tx(Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(1_000),
                script_pubkey: ScriptBuf::new(),
            }],
        })
        .unwrap()
    }

    #[test]
    fn permit_is_one_use_exact_and_revocable() {
        let (sender, receiver) = watch::channel(7);
        let exact = psbt();
        let permit = PairedSigningPermit::new(ChainId::Bitcoin, &exact, receiver.clone(), 7);
        let mut changed = exact.clone();
        changed.unknown.insert(
            coincube_core::miniscript::bitcoin::psbt::raw::Key {
                type_value: 0xfc,
                key: vec![1],
            },
            vec![2],
        );
        assert!(!permit.consume(ChainId::BitcoinBlake2b, &exact));
        assert!(!permit.consume(ChainId::Bitcoin, &changed));
        assert!(permit.consume(ChainId::Bitcoin, &exact));
        assert!(!permit.consume(ChainId::Bitcoin, &exact));

        let revoked = PairedSigningPermit::new(ChainId::Bitcoin, &exact, receiver, 7);
        // A new session notification revokes even when it happens to carry
        // the same numeric generation as the one the permit saw.
        sender.send(7).unwrap();
        assert!(!revoked.consume(ChainId::Bitcoin, &exact));
    }
}
