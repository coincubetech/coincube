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
//!
//! Only a record the poller wrote after the broadcast counts. A transaction
//! that left the mempool stays recorded as its coin's spender until a poll
//! notices, and it can be broadcast again in the meantime. That record tells
//! nothing of the new broadcast: a hold ended on it would leave the coin free
//! as soon as a poll that does not see the transaction yet unspends it. So a
//! broadcast notes which of its inputs the wallet already has as spent by it,
//! and those stay held until the wallet has been seen without that record and
//! then with it again.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, MutexGuard, PoisonError,
    },
    time::{Duration, Instant},
};

use miniscript::bitcoin::{OutPoint, Transaction, Txid};

use crate::database::DatabaseConnection;

#[derive(Debug, Clone, Copy)]
struct Hold {
    /// The transaction we broadcast that spends the coin.
    spender: Txid,
    since: Instant,
    /// The wallet already had the coin as spent by `spender` when we broadcast
    /// it, and has had it so at every look since. Such a record predates the
    /// broadcast and does not end the hold.
    record_predates_broadcast: bool,
    /// Which broadcast took this hold. A look at the wallet tells only of the
    /// holds there when it began, and a broadcast made during it replaces or
    /// adds one.
    broadcast: u64,
}

/// The coins currently held, by outpoint.
#[derive(Debug, Default)]
pub(crate) struct BroadcastHolds {
    held: Mutex<HashMap<OutPoint, Hold>>,
    /// The number of broadcasts held so far. Written under the lock.
    broadcasts: AtomicU64,
}

impl BroadcastHolds {
    /// The map has no invariant a panic could break halfway, so a poisoned
    /// lock is still good to use. Refusing it would fail every later broadcast
    /// after its transaction was sent.
    fn lock(&self) -> MutexGuard<'_, HashMap<OutPoint, Hold>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The inputs of `tx` the wallet has as spent by `tx` itself. Read it
    /// before sending `tx` to the network, for [`Self::hold_broadcast`]: what
    /// the wallet records by then cannot come from that broadcast.
    pub(crate) fn recorded_inputs_of(
        tx: &Transaction,
        db_conn: &mut Box<dyn DatabaseConnection>,
    ) -> HashSet<OutPoint> {
        let spender = tx.compute_txid();
        let outpoints: Vec<OutPoint> = tx.input.iter().map(|txin| txin.previous_output).collect();
        db_conn
            .coins(&[], &outpoints)
            .into_iter()
            .filter(|(_, coin)| coin.spend_txid == Some(spender))
            .map(|(outpoint, _)| outpoint)
            .collect()
    }

    /// [`Self::hold_broadcast`] of a transaction the wallet had no record of.
    #[cfg(test)]
    pub(crate) fn hold_inputs_of(&self, tx: &Transaction, now: Instant) {
        self.hold_broadcast(tx, &HashSet::new(), now);
    }

    /// Hold the inputs of a transaction the network has just accepted.
    ///
    /// `recorded_before` is [`Self::recorded_inputs_of`] the transaction, read
    /// before it was sent.
    ///
    /// The network accepting `tx` means any transaction we broadcast earlier
    /// that shares an input with it has been replaced. That one's holds are
    /// dropped, since the inputs `tx` does not spend are free again.
    pub(crate) fn hold_broadcast(
        &self,
        tx: &Transaction,
        recorded_before: &HashSet<OutPoint>,
        now: Instant,
    ) {
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
        let broadcast = self.broadcasts.fetch_add(1, Ordering::Relaxed);
        for txin in &tx.input {
            held.insert(
                txin.previous_output,
                Hold {
                    spender,
                    since: now,
                    record_predates_broadcast: recorded_before.contains(&txin.previous_output),
                    broadcast,
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
            if now.saturating_duration_since(hold.since) < max_age {
                return true;
            }
            if hold.record_predates_broadcast {
                // The coin was recorded as spent throughout, so selection
                // never depended on this hold.
                log::debug!(
                    "The wallet had transaction '{}' as spending coin '{}' before its \
                     broadcast and has not recorded it anew {} seconds later.",
                    hold.spender,
                    outpoint,
                    max_age.as_secs(),
                );
            } else {
                log::warn!(
                    "The wallet still has no record of transaction '{}' spending coin '{}' \
                     {} seconds after its broadcast. Coin selection may use the coin again.",
                    hold.spender,
                    outpoint,
                    max_age.as_secs(),
                );
            }
            false
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
    ///
    /// A record that was there before the broadcast does not end a hold. It
    /// stops being that once the wallet is seen without it: the next one is
    /// the poller's, written since.
    pub(crate) fn end_recorded(
        &self,
        spender_of: impl FnOnce(&[OutPoint]) -> HashMap<OutPoint, Txid>,
    ) {
        // The wallet is read without the lock, which a broadcast needs.
        let looked_at: HashMap<OutPoint, u64> = self
            .lock()
            .iter()
            .map(|(outpoint, hold)| (*outpoint, hold.broadcast))
            .collect();
        if looked_at.is_empty() {
            return;
        }
        let outpoints: Vec<OutPoint> = looked_at.keys().copied().collect();
        let recorded = spender_of(&outpoints);
        self.lock().retain(|outpoint, hold| {
            // A hold taken during the read is left as it is. The read did not
            // ask about its coin, or asked for the hold it replaced, so it
            // shows neither the coin recorded nor the wallet without a record
            // left from before.
            if looked_at.get(outpoint) != Some(&hold.broadcast) {
                return true;
            }
            if recorded.get(outpoint) != Some(&hold.spender) {
                hold.record_predates_broadcast = false;
                return true;
            }
            hold.record_predates_broadcast
        });
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

    /// The transaction left the mempool and was broadcast again before a poll
    /// freed its coin. The record left from the first broadcast ends nothing,
    /// in a poll that writes no coin or in the one that frees the coin. Only
    /// the record the poller writes afterwards does.
    #[test]
    fn a_record_left_from_an_earlier_broadcast_does_not_end_the_hold() {
        let (a, b) = (outpoint(1), outpoint(2));
        let holds = BroadcastHolds::default();
        let now = Instant::now();
        let tx = spend_of(&[a, b], 0);
        // The wallet has `a` alone as spent by the transaction.
        holds.hold_broadcast(&tx, &set(&[a]), now);
        let spender = tx.compute_txid();
        let recorded_a = || move |_: &[OutPoint]| std::iter::once((a, spender)).collect();
        holds.end_recorded(recorded_a());
        holds.end_recorded(recorded_a());
        assert_eq!(holds.held(now, MAX_AGE), set(&[a, b]));

        // The poller frees the coin, then records the spend it now sees.
        holds.end_recorded(recorded(&[]));
        assert_eq!(holds.held(now, MAX_AGE), set(&[a, b]));
        holds.end_recorded(recorded_a());
        assert_eq!(holds.held(now, MAX_AGE), set(&[b]));
    }

    /// A hold the wallet never records anew still ends at the maximum age.
    #[test]
    fn a_hold_on_a_record_left_from_an_earlier_broadcast_ends_at_the_maximum_age() {
        let a = outpoint(1);
        let holds = BroadcastHolds::default();
        let now = Instant::now();
        let tx = spend_of(&[a], 0);
        holds.hold_broadcast(&tx, &set(&[a]), now);
        holds.end_recorded(recorded(&[&tx]));
        assert_eq!(holds.held(now, MAX_AGE), set(&[a]));
        assert!(holds.held(now + MAX_AGE, MAX_AGE).is_empty());
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

    /// A transaction broadcast again during the wallet read, its coin still
    /// recorded from before, is not taken for one the wallet was seen without
    /// a record of: the read did not cover its hold. That holds for a coin the
    /// read did not ask about and for one whose earlier hold it did.
    #[test]
    fn a_broadcast_during_the_wallet_read_keeps_its_record_left_from_before() {
        let (a, b) = (outpoint(1), outpoint(2));
        let holds = BroadcastHolds::default();
        let now = Instant::now();
        let (spend_a, spend_b) = (spend_of(&[a], 0), spend_of(&[b], 0));
        holds.hold_inputs_of(&spend_a, now);
        holds.end_recorded(|_| {
            holds.hold_broadcast(&spend_a, &set(&[a]), now);
            holds.hold_broadcast(&spend_b, &set(&[b]), now);
            HashMap::new()
        });
        // The record left from before ends neither hold.
        holds.end_recorded(recorded(&[&spend_a, &spend_b]));
        assert_eq!(holds.held(now, MAX_AGE), set(&[a, b]));

        holds.end_recorded(recorded(&[]));
        holds.end_recorded(recorded(&[&spend_a, &spend_b]));
        assert!(holds.held(now, MAX_AGE).is_empty());
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
