//! One-sided splits from Coin control (`decisions/2026-10-29-coincube-desktop-
//! btcb2-split-protection-in-coin-control.md`).
//!
//! A split is an ordinary self-send of the selected both-chain coins back to
//! a fresh change address in the same Vault, made so the twin chain rejects
//! it:
//!
//! - [`Method::SplitKey`] adds one coin proven to exist only on this chain
//!   (the split key). The twin chain has no such coin, so the transaction is
//!   invalid there. The key's proof is re-qualified against both canonical
//!   chains immediately before the transaction is built.
//! - [`Method::Signature`] (Bitcoin Blake2b only) relies on the Vault's
//!   replay-protected signature, which Bitcoin rejects. The signing screen's
//!   replay check still has to pass before broadcast.
//!
//! Either way one side is enough: once the split has depth, neither copy can
//! be replayed onto the other chain. The transaction is handed to the Vault's
//! normal PSBT screen for signing and broadcast, and recorded in a small
//! journal ([`journal`]) from which the Chains column derives *Splitting*
//! and the split outputs' proofs. The twin Cube's journal only nominates its
//! splits: a coin counts as separated by one only once Connect shows that
//! split [`SPLIT_CONFIRMATIONS`] deep on the twin chain now
//! ([`twin_confirmed`]).

pub mod journal;

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use coincube_core::{
    claim_ancestry::{retained::RetainedPath, search::OwnedLink},
    miniscript::bitcoin::{consensus::serialize, psbt::Psbt, OutPoint, Transaction, Txid},
};
use coincubed::commands::CoinStatus;
use serde::{Deserialize, Serialize};

use crate::{
    app::wallet::Wallet,
    chain::ChainId,
    daemon::{
        model::{Coin, CreateSpendResult},
        Daemon,
    },
    services::{
        chain_status::{ProofEntry, SPLIT_CONFIRMATIONS},
        claim_observation::{ObservationSource, TransactionObservation},
    },
};

/// How the split is made rejectable on the twin chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    SplitKey,
    Signature,
}

/// One split this Cube built. The Chains column is derived from it; nothing
/// here authorizes signing or broadcast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitRecord {
    pub txid: Txid,
    pub chain: ChainId,
    pub method: Method,
    /// The both-chain coins being split, without the key.
    pub inputs: Vec<OutPoint>,
    /// The split key, for [`Method::SplitKey`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<OutPoint>,
    /// The key's [`RetainedPath::encode`], hex, so the split's outputs can be
    /// proven by extending it by one link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    /// Unix seconds.
    pub created_at: u64,
    /// The split's confirmations when this Cube last looked: 0 while it is
    /// unconfirmed or after a reorg drops it. Display state only; the twin
    /// Cube never trusts it (see [`twin_confirmed`]).
    #[serde(default)]
    pub confirmations: u32,
}

/// A record whose transaction never showed up is dropped after this long.
pub const UNSEEN_RECORD_LIFETIME_SECONDS: u64 = 14 * 24 * 60 * 60;

/// The split key a split is built with.
#[derive(Debug, Clone)]
pub struct KeyChoice {
    pub outpoint: OutPoint,
    pub path: RetainedPath,
}

/// A built, stored, unsigned split ready for the PSBT screen.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub psbt: Psbt,
    pub record: SplitRecord,
}

/// What the Chains column learns from the journals.
#[derive(Debug, Default)]
pub struct Derived {
    /// Outputs of splits still short of [`SPLIT_CONFIRMATIONS`].
    pub splitting: HashMap<OutPoint, u32>,
    /// Outputs of confirmed split-key splits, proven by the key's extended path.
    pub proofs: Vec<(OutPoint, ProofEntry)>,
    /// Outputs of confirmed signature splits: one-chain by their signatures.
    pub signed_outputs: HashSet<OutPoint>,
    /// Whether any record changed (its confirmations moved or it was pruned).
    pub changed: bool,
}

/// Derive the Chains column's split facts from this Cube's records, updating
/// each record's confirmation count and pruning records whose transaction
/// never appeared. `coins` are the Vault's coins, `tip` the daemon's tip and
/// `transactions` the raw split transactions the daemon knows about.
pub fn derive(
    records: &mut Vec<SplitRecord>,
    coins: &[Coin],
    tip: i32,
    transactions: &HashMap<Txid, Transaction>,
    now: SystemTime,
) -> Derived {
    let now = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut derived = Derived::default();
    let before = records.len();
    records.retain(|record| {
        coins.iter().any(|coin| coin.outpoint.txid == record.txid)
            || now.saturating_sub(record.created_at) < UNSEEN_RECORD_LIFETIME_SECONDS
    });
    derived.changed |= records.len() != before;
    for record in records.iter_mut() {
        let outputs: Vec<&Coin> = coins
            .iter()
            .filter(|coin| coin.outpoint.txid == record.txid)
            .collect();
        let confirmations = match outputs.iter().find_map(|coin| coin.block_height) {
            Some(height) if height >= 0 && height <= tip => (tip - height + 1) as u32,
            _ => 0,
        };
        // Follow the chain both ways: a reorg that drops the split takes its
        // confirmations with it.
        if confirmations != record.confirmations {
            record.confirmations = confirmations;
            derived.changed = true;
        }
        if confirmations == 0 {
            for coin in &outputs {
                derived.splitting.insert(coin.outpoint, 0);
            }
            continue;
        }
        if confirmations < SPLIT_CONFIRMATIONS {
            for coin in &outputs {
                derived.splitting.insert(coin.outpoint, confirmations);
            }
            continue;
        }
        match record.method {
            Method::Signature => {
                derived
                    .signed_outputs
                    .extend(outputs.iter().map(|coin| coin.outpoint));
            }
            Method::SplitKey => {
                let Some(tx) = transactions.get(&record.txid) else {
                    continue;
                };
                for coin in &outputs {
                    if let Some(path) = extended_path(record, tx, coin.outpoint) {
                        derived
                            .proofs
                            .push((coin.outpoint, ProofEntry::Verified(path)));
                    }
                }
            }
        }
    }
    derived
}

/// The proof of a split-key split's output: the split transaction, linked
/// through the key's input, followed by the key's own path. Structurally
/// verified by [`RetainedPath::new`]; `None` if anything does not line up.
pub fn extended_path(
    record: &SplitRecord,
    tx: &Transaction,
    output: OutPoint,
) -> Option<RetainedPath> {
    let key = record.key?;
    if tx.compute_txid() != record.txid || output.txid != record.txid {
        return None;
    }
    let key_path = RetainedPath::decode(key, &hex::decode(record.key_path.as_ref()?).ok()?).ok()?;
    let parent_input = tx
        .input
        .iter()
        .position(|input| input.previous_output == key)?;
    let mut links = vec![OwnedLink {
        transaction: serialize(tx),
        parent_input: Some(parent_input),
    }];
    links.extend(key_path.links().iter().cloned());
    RetainedPath::new(output, links).ok()
}

/// At most this many of the twin Cube's splits are looked up per pass, newest
/// first.
pub const MAX_TWIN_LOOKUPS: usize = 8;

/// The twin Cube's splits worth asking Connect about, newest first. Its own
/// confirmation count is not consulted: it may be stale by any amount.
pub fn twin_candidates(twin_records: &[SplitRecord]) -> Vec<Txid> {
    let mut records: Vec<&SplitRecord> = twin_records.iter().collect();
    records.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    records
        .into_iter()
        .map(|record| record.txid)
        .take(MAX_TWIN_LOOKUPS)
        .collect()
}

/// Coins of this Cube whose twin copy a split on the twin Cube has separated:
/// the inputs of the twin's splits that Connect showed
/// [`SPLIT_CONFIRMATIONS`] deep on the twin chain in this pass (`confirmed`).
pub fn twin_separated(
    twin_records: &[SplitRecord],
    confirmed: &HashSet<Txid>,
) -> HashSet<OutPoint> {
    twin_records
        .iter()
        .filter(|record| confirmed.contains(&record.txid))
        .flat_map(|record| record.inputs.iter().copied())
        .collect()
}

/// How deep `chain`'s best chain holds `txid` now, from fresh reads: the tip,
/// the transaction's block, and the hash at that height, which must be the
/// same block. `None` when it is unconfirmed, absent, off the best chain or
/// any read fails; never a guess.
pub async fn depth_on(source: &dyn ObservationSource, chain: ChainId, txid: Txid) -> Option<u32> {
    let tip = source
        .tip(chain)
        .await
        .ok()
        .filter(|read| read.chain() == chain)?;
    let found = source
        .transaction(chain, txid)
        .await
        .ok()
        .filter(|read| read.chain() == chain)?;
    let TransactionObservation::Confirmed { txid: seen, block } = *found.value() else {
        return None;
    };
    if seen != txid || block.height > tip.value().height {
        return None;
    }
    let at = source
        .hash_at_height(chain, block.height)
        .await
        .ok()
        .filter(|read| read.chain() == chain)?;
    if *at.value() != block.hash {
        return None;
    }
    std::convert::TryFrom::try_from(tip.value().height - block.height + 1).ok()
}

/// Which of the twin Cube's splits (`txids`) Connect shows at least
/// [`SPLIT_CONFIRMATIONS`] deep on the twin of `here` now.
pub async fn twin_confirmed(
    client: crate::services::coincube::CoincubeClient,
    here: ChainId,
    txids: Vec<Txid>,
    generation: u64,
    live: tokio::sync::watch::Receiver<u64>,
) -> HashSet<Txid> {
    use crate::services::claim_observation::{http::HttpObservationSource, CollectionContext};

    let Some(twin) = crate::services::entangled::twin_chain(here) else {
        return HashSet::new();
    };
    let Ok(source) = HttpObservationSource::new(
        client,
        ChainId::Bitcoin,
        ChainId::BitcoinBlake2b,
        CollectionContext {
            expected_generation: generation,
            generation: live,
        },
    ) else {
        return HashSet::new();
    };
    let mut confirmed = HashSet::new();
    for txid in txids {
        if depth_on(&source, twin, txid)
            .await
            .is_some_and(|depth| depth >= SPLIT_CONFIRMATIONS)
        {
            confirmed.insert(txid);
        }
    }
    confirmed
}

/// Build the split, store it with the daemon, and return it for the PSBT
/// screen. The caller records it in the journal. For [`Method::SplitKey`]
/// the key is re-qualified against both chains first; nothing is built if
/// that fails.
#[allow(clippy::too_many_arguments)]
pub async fn prepare(
    daemon: Arc<dyn Daemon + Send + Sync>,
    wallet: Arc<Wallet>,
    method: Method,
    coins: Vec<OutPoint>,
    key: Option<KeyChoice>,
    feerate_vb: u64,
    client: crate::services::coincube::CoincubeClient,
    generation: u64,
    live: tokio::sync::watch::Receiver<u64>,
) -> Result<Prepared, String> {
    let chain = wallet.chain;
    if !crate::services::chain_status::checks_enabled(chain) {
        return Err("Splitting coins isn't available in this version.".into());
    }
    if coins.is_empty() {
        return Err("Select at least one coin to split.".into());
    }
    let key = match (method, key) {
        (Method::SplitKey, Some(key)) => Some(key),
        (Method::SplitKey, None) => return Err("This split needs a split key.".into()),
        (Method::Signature, _) if !chain.is_blake2b() => {
            return Err("Bitcoin can't split with a replay-protected signature.".into())
        }
        (Method::Signature, _) => None,
    };
    if let Some(key) = &key {
        requalify(&client, chain, key, generation, live.clone()).await?;
    }
    let mut requested: BTreeSet<OutPoint> = coins.iter().copied().collect();
    if requested.len() != coins.len() {
        return Err("The split lists a coin twice.".into());
    }
    if let Some(key) = &key {
        if !requested.insert(key.outpoint) {
            return Err("The split key can't also be a coin to split.".into());
        }
    }
    let requested: Vec<OutPoint> = requested.into_iter().collect();
    let mut fresh = daemon
        .list_coins(&[CoinStatus::Confirmed], &requested)
        .await
        .map_err(|e| e.to_string())?
        .coins;
    wallet.apply_coin_overrides(&mut fresh);
    let found: BTreeSet<OutPoint> = fresh.iter().map(|coin| coin.outpoint).collect();
    if found.len() != requested.len()
        || requested.iter().any(|outpoint| !found.contains(outpoint))
        || fresh.iter().any(|coin| {
            coin.spend_info.is_some() || coin.is_immature || coin.block_height.is_none()
        })
    {
        return Err(
            "Some of these coins changed, are spent or aren't confirmed yet. Read the Vault again."
                .into(),
        );
    }
    if !crate::services::claim_observation::session_current(&live, generation) {
        return Err("The session changed. Try again.".into());
    }
    let psbt = match daemon
        .create_spend_tx(&requested, &HashMap::new(), feerate_vb, None)
        .await
        .map_err(|e| e.to_string())?
    {
        CreateSpendResult::Success { psbt, .. } => psbt,
        CreateSpendResult::InsufficientFunds { missing } => {
            return Err(format!(
                "These coins don't cover the fee. {} sats short.",
                missing
            ))
        }
    };
    check_construction(&psbt, &requested)?;
    daemon
        .update_spend_tx(&psbt)
        .await
        .map_err(|e| e.to_string())?;
    let record = SplitRecord {
        txid: psbt.unsigned_tx.compute_txid(),
        chain,
        method,
        inputs: coins,
        key: key.as_ref().map(|key| key.outpoint),
        key_path: key.as_ref().map(|key| hex::encode(key.path.encode())),
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        confirmations: 0,
    };
    Ok(Prepared { psbt, record })
}

/// The daemon's self-send must spend exactly the requested coins into one
/// output back into this Vault.
fn check_construction(psbt: &Psbt, requested: &[OutPoint]) -> Result<(), String> {
    let spent: BTreeSet<OutPoint> = psbt
        .unsigned_tx
        .input
        .iter()
        .map(|input| input.previous_output)
        .collect();
    let wanted: BTreeSet<OutPoint> = requested.iter().copied().collect();
    let owned = |i: usize| {
        psbt.outputs.get(i).is_some_and(|output| {
            !output.bip32_derivation.is_empty() || !output.tap_key_origins.is_empty()
        })
    };
    if spent != wanted || psbt.unsigned_tx.input.len() != requested.len() {
        return Err("The split doesn't spend exactly the selected coins.".into());
    }
    if psbt.unsigned_tx.output.len() != 1 || !owned(0) {
        return Err("The split must pay back into this Vault only.".into());
    }
    Ok(())
}

/// Re-qualify the key's proof against both canonical chains now.
async fn requalify(
    client: &crate::services::coincube::CoincubeClient,
    chain: ChainId,
    key: &KeyChoice,
    generation: u64,
    live: tokio::sync::watch::Receiver<u64>,
) -> Result<(), String> {
    use crate::services::claim_observation::{http::HttpObservationSource, CollectionContext};
    if key.path.selected() != key.outpoint {
        return Err("The split key's proof doesn't match it.".into());
    }
    let source = HttpObservationSource::new(
        client.clone(),
        ChainId::Bitcoin,
        ChainId::BitcoinBlake2b,
        CollectionContext {
            expected_generation: generation,
            generation: live,
        },
    )
    .map_err(|e| format!("Couldn't check the split key: {:?}", e))?;
    let policy = coincube_core::claim::Policy {
        max_observation_age_seconds: 60,
        expiry_margin_seconds: 600,
    };
    let refreshed = if chain.is_blake2b() {
        source.requalify_fork_ancestry(&key.path, policy).await
    } else {
        source.requalify_ancestry(&key.path, policy).await
    }
    .map_err(|e| format!("The split key couldn't be verified right now: {:?}", e))?;
    let path = refreshed
        .retained_path()
        .map_err(|e| format!("The split key's proof is invalid: {:?}", e))?;
    if path.selected() != key.outpoint {
        return Err("The split key's proof doesn't match it.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::{
        absolute, bip32::ChildNumber, script::Builder, transaction, Address, Amount, ScriptBuf,
        Sequence, TxIn, TxOut, Witness,
    };
    use std::str::FromStr;

    fn tx(inputs: Vec<OutPoint>, coinbase_height: Option<i64>) -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: match coinbase_height {
                Some(height) => vec![TxIn {
                    previous_output: OutPoint::null(),
                    script_sig: Builder::new().push_int(height).push_int(1).into_script(),
                    sequence: Sequence::MAX,
                    witness: Witness::new(),
                }],
                None => inputs
                    .into_iter()
                    .map(|previous_output| TxIn {
                        previous_output,
                        script_sig: ScriptBuf::new(),
                        sequence: Sequence::MAX,
                        witness: Witness::new(),
                    })
                    .collect(),
            },
            output: vec![TxOut {
                value: Amount::from_sat(50_000),
                script_pubkey: ScriptBuf::new(),
            }],
        }
    }

    fn coin(outpoint: OutPoint, height: Option<i32>) -> Coin {
        Coin {
            amount: Amount::from_sat(50_000),
            outpoint,
            address: Address::from_str("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4")
                .unwrap()
                .assume_checked(),
            block_height: height,
            derivation_index: ChildNumber::from_normal_idx(0).unwrap(),
            spend_info: None,
            is_immature: false,
            is_change: true,
            is_from_self: true,
        }
    }

    /// A key with a one-link path to a coinbase, a shared coin, and the split
    /// spending both.
    fn split_fixture() -> (SplitRecord, Transaction) {
        let coinbase = tx(vec![], Some(962_000));
        let key = OutPoint::new(coinbase.compute_txid(), 0);
        let key_path = RetainedPath::new(
            key,
            vec![OwnedLink {
                transaction: serialize(&coinbase),
                parent_input: None,
            }],
        )
        .unwrap();
        let shared = OutPoint::new(Txid::from_str(&"11".repeat(32)).unwrap(), 3);
        let split = tx(vec![shared, key], None);
        let record = SplitRecord {
            txid: split.compute_txid(),
            chain: ChainId::Bitcoin,
            method: Method::SplitKey,
            inputs: vec![shared],
            key: Some(key),
            key_path: Some(hex::encode(key_path.encode())),
            created_at: 0,
            confirmations: 0,
        };
        (record, split)
    }

    #[test]
    fn split_output_is_splitting_until_six_confirmations_then_proven() {
        let (record, split) = split_fixture();
        let output = OutPoint::new(record.txid, 0);
        let transactions: HashMap<_, _> = [(record.txid, split)].into();
        let mut records = vec![record];

        let derived = derive(
            &mut records,
            &[coin(output, Some(100))],
            102,
            &transactions,
            UNIX_EPOCH,
        );
        assert_eq!(derived.splitting.get(&output), Some(&3));
        assert!(derived.proofs.is_empty());
        assert_eq!(records[0].confirmations, 3);
        assert!(derived.changed);

        let derived = derive(
            &mut records,
            &[coin(output, Some(100))],
            105,
            &transactions,
            UNIX_EPOCH,
        );
        assert!(derived.splitting.is_empty());
        assert_eq!(derived.proofs.len(), 1);
        assert!(
            matches!(&derived.proofs[0], (o, ProofEntry::Verified(p)) if *o == output && p.selected() == output)
        );
        assert_eq!(records[0].confirmations, 6);
    }

    #[test]
    fn unconfirmed_split_output_is_splitting_at_zero() {
        let (record, _) = split_fixture();
        let output = OutPoint::new(record.txid, 0);
        let mut records = vec![record];
        let derived = derive(
            &mut records,
            &[coin(output, None)],
            100,
            &HashMap::new(),
            UNIX_EPOCH,
        );
        assert_eq!(derived.splitting.get(&output), Some(&0));
    }

    #[test]
    fn signature_split_outputs_are_one_chain_without_a_path() {
        let (mut record, _) = split_fixture();
        record.method = Method::Signature;
        record.key = None;
        record.key_path = None;
        let output = OutPoint::new(record.txid, 0);
        let mut records = vec![record];
        let derived = derive(
            &mut records,
            &[coin(output, Some(10))],
            20,
            &HashMap::new(),
            UNIX_EPOCH,
        );
        assert!(derived.signed_outputs.contains(&output));
        assert!(derived.proofs.is_empty());
    }

    #[test]
    fn unseen_records_expire_and_seen_ones_stay() {
        let (record, _) = split_fixture();
        let later = UNIX_EPOCH + std::time::Duration::from_secs(UNSEEN_RECORD_LIFETIME_SECONDS);
        let mut records = vec![record.clone()];
        assert!(derive(&mut records, &[], 1, &HashMap::new(), later).changed);
        assert!(records.is_empty());
        let mut records = vec![record.clone()];
        derive(
            &mut records,
            &[coin(OutPoint::new(record.txid, 0), None)],
            1,
            &HashMap::new(),
            later,
        );
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn a_path_through_a_different_key_is_refused() {
        let (mut record, split) = split_fixture();
        let output = OutPoint::new(record.txid, 0);
        assert!(extended_path(&record, &split, output).is_some());
        record.key = Some(OutPoint::new(record.txid, 9));
        assert!(extended_path(&record, &split, output).is_none());
    }

    #[test]
    fn a_reorg_takes_the_confirmations_back() {
        let (record, split) = split_fixture();
        let output = OutPoint::new(record.txid, 0);
        let transactions: HashMap<_, _> = [(record.txid, split)].into();
        let mut records = vec![record];
        derive(
            &mut records,
            &[coin(output, Some(100))],
            107,
            &transactions,
            UNIX_EPOCH,
        );
        assert_eq!(records[0].confirmations, 8);
        // The block is gone: the output is unconfirmed again.
        let derived = derive(
            &mut records,
            &[coin(output, None)],
            107,
            &transactions,
            UNIX_EPOCH,
        );
        assert_eq!(records[0].confirmations, 0);
        assert!(derived.changed);
        assert_eq!(derived.splitting.get(&output), Some(&0));
        assert!(derived.proofs.is_empty());
        // Or the transaction left the wallet entirely.
        records[0].confirmations = 8;
        assert!(derive(&mut records, &[], 107, &transactions, UNIX_EPOCH).changed);
        assert_eq!(records[0].confirmations, 0);
    }

    #[test]
    fn twin_coins_separate_only_on_a_fresh_confirmation_not_the_stored_count() {
        let (mut record, _) = split_fixture();
        record.confirmations = 100;
        assert!(
            twin_separated(&[record.clone()], &HashSet::new()).is_empty(),
            "the twin journal's count is never trusted"
        );
        record.confirmations = 0;
        assert_eq!(
            twin_separated(&[record.clone()], &vec![record.txid].into_iter().collect()),
            record.inputs.iter().copied().collect()
        );
    }

    #[test]
    fn twin_candidates_are_the_newest_records_capped() {
        let (record, _) = split_fixture();
        let records: Vec<SplitRecord> = (0..(MAX_TWIN_LOOKUPS as u8 + 2))
            .map(|n| SplitRecord {
                txid: Txid::from_str(&format!("{:02x}", n).repeat(32)).unwrap(),
                created_at: u64::from(n),
                ..record.clone()
            })
            .collect();
        let candidates = twin_candidates(&records);
        assert_eq!(candidates.len(), MAX_TWIN_LOOKUPS);
        assert_eq!(candidates[0], records.last().unwrap().txid);
    }

    mod depth {
        use super::super::*;
        use crate::services::claim_observation::{FailureKind, FreshRead};
        use coincube_core::claim::BlockRef;
        use coincube_core::miniscript::bitcoin::{hashes::Hash, BlockHash};
        use reqwest::header::{HeaderMap, CACHE_CONTROL};

        fn fresh<T>(chain: ChainId, value: T) -> FreshRead<T> {
            let mut headers = HeaderMap::new();
            headers.insert("x-cache", "BYPASS".parse().unwrap());
            headers.insert(CACHE_CONTROL, "private, no-store".parse().unwrap());
            FreshRead::from_response(chain, value, 1, &headers).unwrap()
        }

        struct Source {
            tip: u64,
            tx: TransactionObservation,
            at_height: BlockHash,
        }

        #[async_trait::async_trait]
        impl ObservationSource for Source {
            fn now(&self) -> i64 {
                1
            }
            async fn anchor(
                &self,
                _: ChainId,
            ) -> Result<crate::services::coincube::network_anchor::NetworkAnchorStatus, FailureKind>
            {
                Err(FailureKind::Unavailable)
            }
            async fn tip(&self, chain: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
                Ok(fresh(
                    chain,
                    BlockRef {
                        height: self.tip,
                        hash: BlockHash::from_byte_array([9; 32]),
                    },
                ))
            }
            async fn transaction(
                &self,
                chain: ChainId,
                _: Txid,
            ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
                Ok(fresh(chain, self.tx))
            }
            async fn hash_at_height(
                &self,
                chain: ChainId,
                _: u64,
            ) -> Result<FreshRead<BlockHash>, FailureKind> {
                Ok(fresh(chain, self.at_height))
            }
        }

        fn txid() -> Txid {
            Txid::from_byte_array([4; 32])
        }

        fn confirmed_at(height: u64) -> TransactionObservation {
            TransactionObservation::Confirmed {
                txid: txid(),
                block: BlockRef {
                    height,
                    hash: BlockHash::from_byte_array([1; 32]),
                },
            }
        }

        #[tokio::test]
        async fn depth_counts_from_the_fresh_tip() {
            let source = Source {
                tip: 105,
                tx: confirmed_at(100),
                at_height: BlockHash::from_byte_array([1; 32]),
            };
            assert_eq!(
                depth_on(&source, ChainId::BitcoinBlake2b, txid()).await,
                Some(6)
            );
        }

        #[tokio::test]
        async fn a_block_off_the_best_chain_has_no_depth() {
            let source = Source {
                tip: 105,
                tx: confirmed_at(100),
                at_height: BlockHash::from_byte_array([2; 32]),
            };
            assert_eq!(
                depth_on(&source, ChainId::BitcoinBlake2b, txid()).await,
                None
            );
        }

        #[tokio::test]
        async fn unconfirmed_or_absent_has_no_depth() {
            for tx in vec![
                TransactionObservation::Absent,
                TransactionObservation::Unconfirmed { txid: txid() },
            ] {
                let source = Source {
                    tip: 105,
                    tx,
                    at_height: BlockHash::from_byte_array([1; 32]),
                };
                assert_eq!(
                    depth_on(&source, ChainId::BitcoinBlake2b, txid()).await,
                    None
                );
            }
        }
    }

    #[test]
    fn construction_must_spend_exactly_the_request_into_one_owned_output() {
        let (_, split) = split_fixture();
        let requested: Vec<_> = split.input.iter().map(|i| i.previous_output).collect();
        let mut psbt = Psbt::from_unsigned_tx(split).unwrap();
        assert!(
            check_construction(&psbt, &requested).is_err(),
            "output not owned"
        );
        psbt.outputs[0].bip32_derivation.insert(
            coincube_core::miniscript::bitcoin::secp256k1::PublicKey::from_str(
                "02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5",
            )
            .unwrap(),
            (Default::default(), Default::default()),
        );
        assert!(check_construction(&psbt, &requested).is_ok());
        assert!(check_construction(&psbt, &requested[..1]).is_err());
    }
}
