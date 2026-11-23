//! Coins spent by a transaction this daemon broadcast, held out of automatic
//! coin selection until the poller has recorded that spend.
//!
//! The database learns of a spend from the poller, which reads it back from the
//! Bitcoin backend. Between the network accepting a transaction and the poller
//! recording it, the database still lists the transaction's inputs as unspent,
//! and a spend created with automatic coin selection could pick one of them and
//! conflict with the transaction just sent. How long that lasts depends on the
//! backend: about one poll on bitcoind and Electrum, and on Esplora as long as
//! the server takes to show the transaction to a poll that walks the wallet.
//!
//! A hold covers that gap and nothing more. The poller stays the only writer of
//! coin state, so a hold changes neither what the database stores nor what
//! `listcoins` reports, and it is not kept across a restart.
//!
//! The poller ends a hold, after each poll, once the database has the coin as
//! spent by the held transaction. From then on the coin's state is the
//! poller's to keep, including freeing it again if the spend is dropped or
//! replaced. Coin selection reads the holds before it reads the coins, so it
//! never sees a hold gone and its coin still unspent.

use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use miniscript::bitcoin::{OutPoint, Transaction, Txid};

use crate::database::DatabaseConnection;

#[derive(Debug, Clone, Copy)]
struct Hold {
    /// The transaction we broadcast that spends the coin.
    spender: Txid,
    since: Instant,
}

/// The coins currently held, by outpoint.
#[derive(Debug, Default)]
pub(crate) struct BroadcastHolds {
    held: Mutex<HashMap<OutPoint, Hold>>,
}

impl BroadcastHolds {
    /// The map has no invariant a panic could break halfway, so a poisoned
    /// lock is still good to use. Refusing it would fail every later broadcast
    /// after its transaction was sent.
    fn lock(&self) -> MutexGuard<'_, HashMap<OutPoint, Hold>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hold the inputs of a transaction the network has just accepted.
    ///
    /// The network accepting `tx` means any transaction we broadcast earlier
    /// that shares an input with it has been replaced. That one's holds are
    /// dropped, since the inputs `tx` does not spend are free again.
    pub(crate) fn hold_inputs_of(&self, tx: &Transaction, now: Instant) {
        let spender = tx.compute_txid();
        let mut held = self.lock();
        let replaced: HashSet<Txid> = tx
            .input
            .iter()
            .filter_map(|txin| held.get(&txin.previous_output))
            .map(|hold| hold.spender)
            .filter(|earlier| *earlier != spender)
            .collect();
        held.retain(|_, hold| !replaced.contains(&hold.spender));
        for txin in &tx.input {
            held.insert(
                txin.previous_output,
                Hold {
                    spender,
                    since: now,
                },
            );
        }
    }

    /// The coins automatic coin selection must leave alone. Call it before
    /// reading the coins from the database: see the module documentation.
    ///
    /// A hold the poller has not ended is dropped here after `max_age`, so a
    /// transaction the backend never shows cannot keep a coin out of selection
    /// forever.
    pub(crate) fn held(&self, now: Instant, max_age: Duration) -> HashSet<OutPoint> {
        let mut held = self.lock();
        held.retain(|outpoint, hold| {
            if now.saturating_duration_since(hold.since) >= max_age {
                log::warn!(
                    "The wallet still has no record of transaction '{}' spending coin '{}' \
                     {} seconds after its broadcast. Coin selection may use the coin again.",
                    hold.spender,
                    outpoint,
                    max_age.as_secs(),
                );
                return false;
            }
            true
        });
        held.keys().copied().collect()
    }

    /// End the holds on coins the wallet now records as spent by the very
    /// transaction they are held for. The poller calls this after each poll.
    ///
    /// `spender_of` is given the held coins and returns, for those the wallet
    /// has as spent, the spending transaction. A coin recorded as spent by
    /// another transaction stays held: that is the transaction ours replaced,
    /// and the poller has yet to see ours. That the wallet has stored our
    /// transaction does not end a hold either. It keeps a transaction it once
    /// stored after freeing the coin, so a transaction broadcast again would
    /// not be held at all, and the poller stores a transaction a few writes
    /// before it marks the coin.
    pub(crate) fn end_recorded(
        &self,
        spender_of: impl FnOnce(&[OutPoint]) -> HashMap<OutPoint, Txid>,
    ) {
        // The wallet is read without the lock, which a broadcast needs.
        let outpoints: Vec<OutPoint> = self.lock().keys().copied().collect();
        if outpoints.is_empty() {
            return;
        }
        let recorded = spender_of(&outpoints);
        self.lock()
            .retain(|outpoint, hold| recorded.get(outpoint) != Some(&hold.spender));
    }

    /// [`Self::end_recorded`] against the wallet database, which is connected
    /// to only when a coin is held.
    pub(crate) fn end_recorded_in(&self, connect: impl FnOnce() -> Box<dyn DatabaseConnection>) {
        self.end_recorded(|outpoints| {
            connect()
                .coins(&[], outpoints)
                .into_iter()
                .filter_map(|(outpoint, coin)| Some((outpoint, coin.spend_txid?)))
                .collect()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniscript::bitcoin::{
        absolute, hashes::Hash, transaction, ScriptBuf, Sequence, TxIn, Witness,
    };

    const MAX_AGE: Duration = Duration::from_secs(600);

    fn outpoint(byte: u8) -> OutPoint {
        OutPoint {
            txid: Txid::from_byte_array([byte; 32]),
            vout: 0,
        }
    }

    /// A transaction spending `inputs`. `tag` tells two spends of the same
    /// inputs apart.
    fn spend_of(inputs: &[OutPoint], tag: u32) -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::from_consensus(tag),
            input: inputs
                .iter()
                .map(|previous_output| TxIn {
                    previous_output: *previous_output,
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                    witness: Witness::new(),
                })
                .collect(),
            output: Vec::new(),
        }
    }

    /// A wallet that records each of `spends`' inputs as spent by it.
    fn recorded(spends: &[&Transaction]) -> impl FnOnce(&[OutPoint]) -> HashMap<OutPoint, Txid> {
        let spenders: HashMap<OutPoint, Txid> = spends
            .iter()
            .flat_map(|tx| {
                let txid = tx.compute_txid();
                tx.input
                    .iter()
                    .map(move |txin| (txin.previous_output, txid))
            })
            .collect();
        move |_| spenders
    }

    fn set(outpoints: &[OutPoint]) -> HashSet<OutPoint> {
        outpoints.iter().copied().collect()
    }

    #[test]
    fn nothing_is_held_and_the_wallet_is_not_read_before_a_broadcast() {
        let holds = BroadcastHolds::default();
        holds.end_recorded(|_| panic!("no hold, so no reason to read the wallet"));
        assert!(holds.held(Instant::now(), MAX_AGE).is_empty());
    }

    #[test]
    fn inputs_are_held_while_the_wallet_has_not_recorded_the_spend() {
        let (a, b) = (outpoint(1), outpoint(2));
        let holds = BroadcastHolds::default();
        let now = Instant::now();
        holds.hold_inputs_of(&spend_of(&[a, b], 0), now);
        holds.end_recorded(recorded(&[&spend_of(&[outpoint(3)], 0)]));
        assert_eq!(holds.held(now, MAX_AGE), set(&[a, b]));
        // Still held on a later look, short of the maximum age.
        let later = now + MAX_AGE - Duration::from_secs(1);
        assert_eq!(holds.held(later, MAX_AGE), set(&[a, b]));
    }

    /// The wallet is asked about the held coins and no others.
    #[test]
    fn the_wallet_is_asked_about_the_held_coins() {
        let (a, b) = (outpoint(1), outpoint(2));
        let holds = BroadcastHolds::default();
        holds.hold_inputs_of(&spend_of(&[a, b], 0), Instant::now());
        holds.end_recorded(|asked| {
            assert_eq!(set(asked), set(&[a, b]));
            HashMap::new()
        });
    }

    /// Once the poller has recorded the spend the coins' state is the
    /// poller's to keep, and the holds do not come back.
    #[test]
    fn a_hold_ends_for_good_once_the_wallet_records_the_spend() {
        let (a, b, c) = (outpoint(1), outpoint(2), outpoint(3));
        let holds = BroadcastHolds::default();
        let now = Instant::now();
        let first = spend_of(&[a, b], 0);
        holds.hold_inputs_of(&first, now);
        holds.hold_inputs_of(&spend_of(&[c], 0), now);
        holds.end_recorded(recorded(&[&first]));
        assert_eq!(holds.held(now, MAX_AGE), set(&[c]));
        // The poller has since freed the coins: nothing holds them again.
        holds.end_recorded(recorded(&[]));
        assert_eq!(holds.held(now, MAX_AGE), set(&[c]));
    }

    /// The backend can fail to give one input's spender. That input stays
    /// held while the other is released.
    #[test]
    fn only_the_inputs_recorded_as_spent_are_released() {
        let (a, b) = (outpoint(1), outpoint(2));
        let holds = BroadcastHolds::default();
        let now = Instant::now();
        let tx = spend_of(&[a, b], 0);
        holds.hold_inputs_of(&tx, now);
        holds.end_recorded(recorded(&[&spend_of(&[a], 0)]));
        assert_eq!(holds.held(now, MAX_AGE), set(&[a, b]));
        let spender = tx.compute_txid();
        holds.end_recorded(|_| std::iter::once((a, spender)).collect());
        assert_eq!(holds.held(now, MAX_AGE), set(&[b]));
    }

    #[test]
    fn a_hold_ends_at_the_maximum_age() {
        let a = outpoint(1);
        let holds = BroadcastHolds::default();
        let now = Instant::now();
        holds.hold_inputs_of(&spend_of(&[a], 0), now);
        assert!(holds.held(now + MAX_AGE, MAX_AGE).is_empty());
        assert!(holds.held(now, MAX_AGE).is_empty());
    }

    /// A replacement that spends only some of the first transaction's inputs
    /// (a cancel) frees the others at once.
    #[test]
    fn a_replacement_frees_the_inputs_it_does_not_spend() {
        let (a, b, c) = (outpoint(1), outpoint(2), outpoint(3));
        let holds = BroadcastHolds::default();
        let now = Instant::now();
        holds.hold_inputs_of(&spend_of(&[a, b], 0), now);
        holds.hold_inputs_of(&spend_of(&[c], 0), now);
        holds.hold_inputs_of(&spend_of(&[a], 1), now);
        assert_eq!(holds.held(now, MAX_AGE), set(&[a, c]));
    }

    /// The wallet having the coins as spent by the replaced transaction says
    /// nothing of the replacement: they stay held until it is recorded.
    #[test]
    fn a_replacement_is_held_while_the_wallet_only_knows_what_it_replaced() {
        let (a, b) = (outpoint(1), outpoint(2));
        let holds = BroadcastHolds::default();
        let now = Instant::now();
        let first = spend_of(&[a, b], 0);
        let replacement = spend_of(&[a, b], 1);
        holds.hold_inputs_of(&first, now);
        holds.end_recorded(recorded(&[&first]));
        assert!(holds.held(now, MAX_AGE).is_empty());
        holds.hold_inputs_of(&replacement, now);
        holds.end_recorded(recorded(&[&first]));
        assert_eq!(holds.held(now, MAX_AGE), set(&[a, b]));
        holds.end_recorded(recorded(&[&replacement]));
        assert!(holds.held(now, MAX_AGE).is_empty());
    }

    /// A transaction recorded once, then dropped, and broadcast again is held
    /// again until the wallet records its spend again.
    #[test]
    fn a_transaction_broadcast_again_after_the_wallet_freed_its_coin_is_held() {
        let a = outpoint(1);
        let holds = BroadcastHolds::default();
        let now = Instant::now();
        let tx = spend_of(&[a], 0);
        holds.hold_inputs_of(&tx, now);
        holds.end_recorded(recorded(&[&tx]));
        assert!(holds.held(now, MAX_AGE).is_empty());

        holds.hold_inputs_of(&tx, now);
        holds.end_recorded(recorded(&[]));
        assert_eq!(holds.held(now, MAX_AGE), set(&[a]));
        holds.end_recorded(recorded(&[&tx]));
        assert!(holds.held(now, MAX_AGE).is_empty());
    }

    #[test]
    fn broadcasting_the_same_transaction_again_restarts_its_hold() {
        let a = outpoint(1);
        let holds = BroadcastHolds::default();
        let now = Instant::now();
        let tx = spend_of(&[a], 0);
        holds.hold_inputs_of(&tx, now);
        let again = now + MAX_AGE - Duration::from_secs(1);
        holds.hold_inputs_of(&tx, again);
        assert_eq!(holds.held(now + MAX_AGE, MAX_AGE), set(&[a]));
    }

    /// A broadcast made while the wallet is being read is not lost, and a
    /// replacement made then is not released on what was read for the
    /// transaction it replaced.
    #[test]
    fn a_broadcast_during_the_wallet_read_keeps_its_hold() {
        let (a, b) = (outpoint(1), outpoint(2));
        let holds = BroadcastHolds::default();
        let now = Instant::now();
        let first = spend_of(&[a], 0);
        holds.hold_inputs_of(&first, now);
        holds.end_recorded(|_| {
            holds.hold_inputs_of(&spend_of(&[a], 1), now);
            holds.hold_inputs_of(&spend_of(&[b], 0), now);
            std::iter::once((a, first.compute_txid())).collect()
        });
        assert_eq!(holds.held(now, MAX_AGE), set(&[a, b]));
    }

    #[test]
    fn a_panic_while_holding_the_lock_does_not_disable_the_holds() {
        let a = outpoint(1);
        let holds = BroadcastHolds::default();
        let now = Instant::now();
        holds.hold_inputs_of(&spend_of(&[a], 0), now);
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = holds.lock();
            panic!("poison the lock");
        }));
        assert!(poisoned.is_err());
        assert!(holds.held.is_poisoned());
        holds.hold_inputs_of(&spend_of(&[outpoint(2)], 0), now);
        assert_eq!(holds.held(now, MAX_AGE), set(&[a, outpoint(2)]));
    }
}
