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
//!   replay check still has to pass before broadcast. Its confirmed
//!   transaction is not yet checked for those signatures, so it is not
//!   counted as evidence (decision open item 1): past the threshold its
//!   outputs show *Split · unconfirmed*, never verified, and the twin Cube
//!   does not treat its inputs as separated.
//!
//! Either way one side is enough: once the split has depth, neither copy can
//! be replayed onto the other chain. The transaction is handed to the Vault's
//! normal PSBT screen for signing and broadcast, and recorded in a small
//! journal ([`journal`]) from which the Chains column derives *Splitting*
//! and candidate proofs for the split outputs, which a fresh ancestry check
//! must still verify. The twin Cube's journal only nominates its splits: a
//! coin counts as separated by one only once Connect shows that split
//! [`SPLIT_CONFIRMATIONS`] deep on the twin chain now and its confirmed
//! transaction spends the coin ([`twin_confirmed`]).

pub mod journal;

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use coincube_core::{
    claim_ancestry::{retained::RetainedPath, search::OwnedLink},
    descriptors::CoincubeDescriptor,
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
    /// Outputs of signature splits past [`SPLIT_CONFIRMATIONS`], whose
    /// signatures nothing has checked.
    pub unconfirmed: HashSet<OutPoint>,
    /// Outputs of confirmed split-key splits with the key's extended path,
    /// as [`ProofEntry::Saved`]: the path is only structurally checked, so a
    /// fresh ancestry check must qualify it before it is shown as verified or
    /// used as a split key. Merge with [`merge_proofs`].
    pub proofs: Vec<(OutPoint, ProofEntry)>,
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
            // Not evidence yet: nothing here verifies that the confirmed
            // transaction carries a replay-protected signature on every input
            // (it could have been signed elsewhere, legacy). Until that check
            // exists (decision open item 1) a signature split separates
            // nothing; its outputs are shown as split but unconfirmed, never
            // proven, and its inputs stay on both chains in the twin Cube.
            Method::Signature => {
                derived
                    .unconfirmed
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
                            .push((coin.outpoint, ProofEntry::Saved(path)));
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
/// The key's path comes from the journal, so nothing here re-qualifies it
/// against the chains: [`derive`] offers it as [`ProofEntry::Saved`].
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

/// Add [`Derived::proofs`] to the cache's ancestry results. A derived path
/// only fills a gap: it never replaces a check's own answer, verified or not,
/// so a coin a check found no path for keeps that answer until it is due
/// again. Returns whether anything was added.
pub fn merge_proofs(
    chain_proofs: &mut HashMap<OutPoint, ProofEntry>,
    derived: Vec<(OutPoint, ProofEntry)>,
) -> bool {
    let mut added = false;
    for (outpoint, entry) in derived {
        if let std::collections::hash_map::Entry::Vacant(slot) = chain_proofs.entry(outpoint) {
            slot.insert(entry);
            added = true;
        }
    }
    added
}

/// At most this many of the twin Cube's splits are looked up per pass.
pub const MAX_TWIN_LOOKUPS: usize = 8;

/// A twin split's last fresh check stops counting after this long; by then
/// the round-robin in [`twin_candidates`] has normally checked it again.
pub const TWIN_CHECK_LIFETIME: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The last fresh check of one twin split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwinCheck {
    /// The inputs of the transaction Connect showed at least
    /// [`SPLIT_CONFIRMATIONS`] deep on the twin chain, read from its
    /// txid-authenticated bytes. `None` when it was shallower or absent, a
    /// read failed, or the lookup did not run.
    pub spent: Option<HashSet<OutPoint>>,
    /// When the pass that made the check finished.
    pub at: std::time::Instant,
}

/// Record the twin lookups of one pass, which finished at `at`. A lookup
/// replaces a split's previous check only if that check is older, so a pass
/// that lands after a newer one still counts for the splits it looked up,
/// and a pass that did not look a split up leaves its check alone. Returns
/// whether any check changed.
pub fn record_twin_checks(
    checks: &mut HashMap<Txid, TwinCheck>,
    checked: Vec<(Txid, Option<HashSet<OutPoint>>)>,
    at: std::time::Instant,
) -> bool {
    let mut changed = false;
    for (txid, spent) in checked {
        if checks.get(&txid).is_some_and(|check| check.at >= at) {
            continue;
        }
        checks.insert(txid, TwinCheck { spent, at });
        changed = true;
    }
    changed
}

/// The twin Cube's split-key splits to ask Connect about this pass: never
/// checked first, then the longest unchecked, newest first among equals, up
/// to [`MAX_TWIN_LOOKUPS`]. Every record is reached in turn however many
/// there are. The twin journal's own confirmation count is not consulted.
pub fn twin_candidates(
    twin_records: &[SplitRecord],
    checks: &HashMap<Txid, TwinCheck>,
) -> Vec<Txid> {
    let mut records: Vec<&SplitRecord> = twin_records
        .iter()
        .filter(|record| record.method == Method::SplitKey)
        .collect();
    records.sort_by_key(|record| {
        (
            checks.get(&record.txid).map(|check| check.at),
            std::cmp::Reverse(record.created_at),
        )
    });
    let mut txids: Vec<Txid> = records.into_iter().map(|record| record.txid).collect();
    txids.dedup();
    txids.truncate(MAX_TWIN_LOOKUPS);
    txids
}

/// Coins of this Cube whose twin copy a split on the twin Cube has separated:
/// the inputs of the twin's split-key splits whose last fresh check, within
/// [`TWIN_CHECK_LIFETIME`] of `now`, showed them [`SPLIT_CONFIRMATIONS`]
/// deep on the twin chain. Only inputs the journal lists *and* the confirmed
/// transaction spends count: the journal nominates, the chain decides.
/// Signature splits never count (see [`derive`]).
///
/// Each coin maps to the instant its check stops counting, so a reader can
/// drop it then even if no later pass lands ([`is_separated`]).
pub fn twin_separated(
    twin_records: &[SplitRecord],
    checks: &HashMap<Txid, TwinCheck>,
    now: std::time::Instant,
) -> HashMap<OutPoint, std::time::Instant> {
    let mut separated = HashMap::new();
    for record in twin_records
        .iter()
        .filter(|record| record.method == Method::SplitKey)
    {
        let Some(check) = checks.get(&record.txid) else {
            continue;
        };
        let Some(spent) = check.spent.as_ref() else {
            continue;
        };
        let until = check.at + TWIN_CHECK_LIFETIME;
        if now > until {
            continue;
        }
        for input in record.inputs.iter().filter(|input| spent.contains(input)) {
            let entry = separated.entry(*input).or_insert(until);
            *entry = (*entry).max(until);
        }
    }
    separated
}

/// Whether `outpoint` counts as separated at `now` by a [`twin_separated`]
/// result: its check has not expired yet.
pub fn is_separated(
    separated: &HashMap<OutPoint, std::time::Instant>,
    outpoint: &OutPoint,
    now: std::time::Instant,
) -> bool {
    separated.get(outpoint).is_some_and(|until| now <= *until)
}

/// Upper bound, in vbytes, of a split spending `shared_count` both-chain
/// coins plus one more input (the split key, or for a signature split an
/// overestimate by one input) to one change output of `descriptor`'s Vault.
/// A funding estimate only, not authorization to construct, sign or
/// broadcast anything. `None` for a Taproot Vault, no coins, or more inputs
/// than the estimate covers. Moved from the removed Claim engine
/// (`coincube_core::claim_spend::estimate_ancestry_self_transfer_vbytes`).
pub fn estimate_split_vbytes(descriptor: &CoincubeDescriptor, shared_count: usize) -> Option<u64> {
    use coincube_core::miniscript::bitcoin::{
        absolute::LockTime, bip32::ChildNumber, secp256k1, transaction, Amount, TxIn, TxOut,
    };
    if descriptor.is_taproot() || shared_count == 0 {
        return None;
    }
    let input_count = shared_count
        .checked_add(1)
        .filter(|count| *count <= 20_001)?;
    let secp = secp256k1::Secp256k1::verification_only();
    let change_script = descriptor
        .change_descriptor()
        .derive(ChildNumber::Normal { index: 0 }, &secp)
        .script_pubkey();
    let tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn::default(); input_count],
        output: vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: change_script,
        }],
    };
    Some(descriptor.unsigned_tx_max_vbytes(&tx, true))
}

/// Whether this Vault can split with a replay-protected signature: Bitcoin
/// Blake2b, and every primary-path signer either signs in process or is a
/// recorded Keychain key. Any other signer is treated as a hardware device,
/// which can't make the signature (decision item 3).
pub fn signature_available(wallet: &Wallet) -> bool {
    if !wallet.chain.is_blake2b() {
        return false;
    }
    let Some(descriptor) = wallet.inheritance_descriptor() else {
        return false;
    };
    let mut in_process: HashSet<_> = wallet.border_wallet_fingerprints.iter().copied().collect();
    if let Some(signer) = &wallet.signer {
        in_process.insert(signer.fingerprint());
    }
    descriptor
        .policy()
        .primary_path()
        .thresh_origins()
        .1
        .keys()
        .all(|fingerprint| {
            in_process.contains(fingerprint)
                || (wallet.keychain_keys_recorded
                    && wallet.keychain_key_ids.contains_key(fingerprint))
        })
}

/// The split's fee rate: the six-block estimate of Connect's Esplora for
/// this chain only, failing closed (`services::split_fees`). Never another
/// chain's estimate and never a local guess.
pub async fn fee_rate(
    client: Option<crate::services::coincube::CoincubeClient>,
    chain: ChainId,
) -> Option<u64> {
    use crate::services::split_fees::{self, SweepFeeSource};
    let client = client?;
    let rate = match chain {
        ChainId::Bitcoin => {
            split_fees::ConnectBitcoinFees::new(&client)?
                .mid_priority_sat_vb()
                .await
        }
        ChainId::BitcoinBlake2b => {
            split_fees::ConnectBtcb2Fees::new(client)?
                .mid_priority_sat_vb()
                .await
        }
        _ => None,
    }?;
    (1..=coincube_core::spend::MAX_FEERATE)
        .contains(&rate)
        .then_some(rate)
}

/// Whether a refresh pass numbered `seq` may be applied after the pass
/// numbered `applied`: only a newer one. A slow pass that lands after a newer
/// one would otherwise put back facts the newer one had cleared.
pub fn newer_pass(applied: &mut u64, seq: u64) -> bool {
    if seq <= *applied {
        return false;
    }
    *applied = seq;
    true
}

/// How deep `chain`'s best chain holds `txid` now, from fresh reads: the tip,
/// the transaction's block, the hash at that height, which must be the same
/// block, and the tip again, which must not have moved. `None` when it is
/// unconfirmed, absent, off the best chain, the chain moved during the reads
/// or any read fails; never a guess.
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
    let after = source
        .tip(chain)
        .await
        .ok()
        .filter(|read| read.chain() == chain)?;
    if after.value() != tip.value() {
        return None;
    }
    std::convert::TryFrom::try_from(tip.value().height - block.height + 1).ok()
}

/// Check the twin Cube's splits (`txids`) on the twin of `here` now: for
/// each that Connect shows at least [`SPLIT_CONFIRMATIONS`] deep, the inputs
/// of that transaction (see [`TwinCheck::spent`]). Every txid gets an answer;
/// a shallow or absent split, a failed read or a source that cannot be built
/// answers `None`.
pub async fn twin_confirmed(
    client: crate::services::coincube::CoincubeClient,
    here: ChainId,
    txids: Vec<Txid>,
    generation: u64,
    live: tokio::sync::watch::Receiver<u64>,
) -> Vec<(Txid, Option<HashSet<OutPoint>>)> {
    use crate::services::claim_observation::{http::HttpObservationSource, CollectionContext};

    let unchecked = |txids: Vec<Txid>| txids.into_iter().map(|txid| (txid, None)).collect();
    let Some(twin) = crate::services::entangled::twin_chain(here) else {
        return unchecked(txids);
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
        return unchecked(txids);
    };
    let mut checked = Vec::with_capacity(txids.len());
    for txid in txids {
        let deep = depth_on(&source, twin, txid)
            .await
            .is_some_and(|depth| depth >= SPLIT_CONFIRMATIONS);
        let spent = if deep {
            source
                .ancestry_transaction(twin, txid)
                .await
                .ok()
                .and_then(|raw| spent_by(&raw, txid))
        } else {
            None
        };
        checked.push((txid, spent));
    }
    checked
}

/// The inputs of `raw` if it is the transaction `txid`. A txid commits to
/// every input, so these are the inputs of the transaction the chain holds.
fn spent_by(raw: &[u8], txid: Txid) -> Option<HashSet<OutPoint>> {
    let tx: Transaction = coincube_core::miniscript::bitcoin::consensus::deserialize(raw).ok()?;
    (tx.compute_txid() == txid)
        .then(|| tx.input.iter().map(|input| input.previous_output).collect())
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
        return Err("Splitting coins isn't available on this network.".into());
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

    /// The split size estimate: refused for no coins or too many, and one
    /// more shared coin costs at least one more input.
    #[test]
    fn split_vbytes_estimate_bounds_and_refusals() {
        let wsh = CoincubeDescriptor::from_str("wsh(or_d(pk([f5acc2fd]tpubD6NzVbkrYhZ4YgUx2ZLNt2rLYAMTdYysCRzKoLu2BeSHKvzqPaBDvf17GeBPnExUVPkuBpx4kniP964e2MxyzzazcXLptxLXModSVCVEV1T/<0;1>/*),and_v(v:pkh([8a64f2a9]tpubD6NzVbkrYhZ4WmzFjvQrp7sDa4ECUxTi9oby8K4FZkd3XCBtEdKwUiQyYJaxiJo5y42gyDWEczrFpozEjeLxMPxjf2WtkfcbpUdfvNnozWF/<0;1>/*),older(10))))#d72le4dr").unwrap();
        assert_eq!(estimate_split_vbytes(&wsh, 0), None);
        let one = estimate_split_vbytes(&wsh, 1).unwrap();
        let two = estimate_split_vbytes(&wsh, 2).unwrap();
        // Two P2WSH inputs and one P2WSH output: well above a bare skeleton.
        assert!(one > 2 * 41 + 43, "{}", one);
        // Each input adds at least its outpoint, sequence and a signature.
        assert!(two >= one + 41 + 18, "{} {}", one, two);
        assert_eq!(estimate_split_vbytes(&wsh, 20_001), None);
    }

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
    fn split_output_is_splitting_until_six_confirmations_then_saved() {
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
            matches!(&derived.proofs[0], (o, ProofEntry::Saved(p)) if *o == output && p.selected() == output),
            "the journal's key path is not re-qualified, so a fresh check must verify it"
        );
        assert_eq!(derived.proofs[0].1.outcome(), None, "not shown as verified");
        assert_eq!(records[0].confirmations, 6);
    }

    #[test]
    fn a_derived_path_only_fills_a_gap() {
        let (record, split) = split_fixture();
        let output = OutPoint::new(record.txid, 0);
        let path = extended_path(&record, &split, output).unwrap();
        let derived = || vec![(output, ProofEntry::Saved(path.clone()))];

        let mut proofs = HashMap::new();
        assert!(merge_proofs(&mut proofs, derived()));
        assert!(matches!(proofs.get(&output), Some(ProofEntry::Saved(_))));
        assert!(!merge_proofs(&mut proofs, derived()), "already there");

        for answer in [
            ProofEntry::Verified(path.clone()),
            ProofEntry::NoQualifyingAncestry {
                checked_at: UNIX_EPOCH,
            },
            ProofEntry::Failed {
                failed_at: UNIX_EPOCH,
            },
        ] {
            let mut proofs: HashMap<_, _> = [(output, answer.clone())].into();
            assert!(!merge_proofs(&mut proofs, derived()));
            assert_eq!(
                std::mem::discriminant(&proofs[&output]),
                std::mem::discriminant(&answer),
                "a check's own answer stays"
            );
        }
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
    fn a_signature_split_separates_nothing_on_intent_alone() {
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
        assert!(derived.proofs.is_empty(), "no proof from a record's intent");
        assert!(
            derived.splitting.is_empty(),
            "past the threshold, so not splitting"
        );
        assert_eq!(
            derived.unconfirmed,
            vec![output].into_iter().collect::<HashSet<_>>(),
            "shown as split but unconfirmed instead"
        );
        let checks: HashMap<Txid, TwinCheck> = vec![(
            records[0].txid,
            TwinCheck {
                spent: Some(records[0].inputs.iter().copied().collect()),
                at: std::time::Instant::now(),
            },
        )]
        .into_iter()
        .collect();
        assert!(
            twin_separated(&records, &checks, std::time::Instant::now()).is_empty(),
            "nor on the twin side, however deep"
        );
        assert!(twin_candidates(&records, &checks).is_empty());
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
    fn twin_coins_separate_only_on_a_recent_fresh_confirmation() {
        let (mut record, _) = split_fixture();
        let now = std::time::Instant::now();
        record.confirmations = 100;
        assert!(
            twin_separated(&[record.clone()], &HashMap::new(), now).is_empty(),
            "the twin journal's count is never trusted"
        );
        let (_, split) = split_fixture();
        let confirmed = spent_by(&serialize(&split), record.txid);
        let check = |spent, at| -> HashMap<Txid, TwinCheck> {
            vec![(record.txid, TwinCheck { spent, at })]
                .into_iter()
                .collect()
        };
        assert_eq!(
            twin_separated(&[record.clone()], &check(confirmed.clone(), now), now)
                .into_keys()
                .collect::<HashSet<_>>(),
            record.inputs.iter().copied().collect()
        );
        assert!(
            twin_separated(&[record.clone()], &check(None, now), now).is_empty(),
            "a failed or shallow read separates nothing"
        );
        let later = now + TWIN_CHECK_LIFETIME + std::time::Duration::from_secs(1);
        assert!(
            twin_separated(&[record.clone()], &check(confirmed, now), later).is_empty(),
            "an old check stops counting"
        );
    }

    #[test]
    fn a_separation_expires_without_another_pass() {
        let (record, split) = split_fixture();
        let at = std::time::Instant::now();
        let checks: HashMap<Txid, TwinCheck> = [(
            record.txid,
            TwinCheck {
                spent: spent_by(&serialize(&split), record.txid),
                at,
            },
        )]
        .into();
        // Computed once, by the last pass that landed; no pass follows.
        let separated = twin_separated(std::slice::from_ref(&record), &checks, at);
        let coin = record.inputs[0];
        assert!(is_separated(&separated, &coin, at));
        assert!(is_separated(&separated, &coin, at + TWIN_CHECK_LIFETIME));
        assert!(
            !is_separated(
                &separated,
                &coin,
                at + TWIN_CHECK_LIFETIME + std::time::Duration::from_secs(1)
            ),
            "the stored result stops counting when its check does"
        );
    }

    #[test]
    fn only_inputs_the_confirmed_transaction_spends_are_separated() {
        let (mut record, split) = split_fixture();
        let shared = record.inputs[0];
        let extra = OutPoint::new(Txid::from_str(&"22".repeat(32)).unwrap(), 1);
        record.inputs.push(extra);
        let now = std::time::Instant::now();
        let checks: HashMap<Txid, TwinCheck> = [(
            record.txid,
            TwinCheck {
                spent: spent_by(&serialize(&split), record.txid),
                at: now,
            },
        )]
        .into();
        assert_eq!(
            twin_separated(&[record], &checks, now)
                .into_keys()
                .collect::<HashSet<_>>(),
            [shared].into(),
            "a journal input the confirmed transaction does not spend stays"
        );
    }

    #[test]
    fn spent_inputs_come_only_from_the_named_transaction() {
        let (record, split) = split_fixture();
        assert_eq!(
            spent_by(&serialize(&split), record.txid),
            Some(
                split
                    .input
                    .iter()
                    .map(|input| input.previous_output)
                    .collect()
            )
        );
        let other = Txid::from_str(&"33".repeat(32)).unwrap();
        assert_eq!(spent_by(&serialize(&split), other), None);
        assert_eq!(spent_by(&[0, 1, 2], record.txid), None);
    }

    #[test]
    fn a_late_pass_keeps_its_checks_and_an_unchecked_split_clears() {
        let (record, split) = split_fixture();
        let spent = spent_by(&serialize(&split), record.txid);
        let other = Txid::from_str(&"44".repeat(32)).unwrap();
        let early = std::time::Instant::now();
        let late = early + std::time::Duration::from_secs(1);
        let mut checks = HashMap::new();

        // The newer pass finished first and did not look `record` up.
        assert!(record_twin_checks(&mut checks, vec![(other, None)], late));
        // The older pass that did look it up still counts for it.
        assert!(record_twin_checks(
            &mut checks,
            vec![(record.txid, spent.clone()), (other, spent.clone())],
            early
        ));
        assert_eq!(checks[&record.txid].spent, spent);
        assert_eq!(
            checks[&other].spent, None,
            "an older check never replaces a newer one"
        );
        assert_eq!(
            twin_separated(std::slice::from_ref(&record), &checks, late)
                .into_keys()
                .collect::<HashSet<_>>(),
            record.inputs.iter().copied().collect()
        );

        // A later lookup that did not run or came back negative clears it.
        let later = late + std::time::Duration::from_secs(1);
        assert!(record_twin_checks(
            &mut checks,
            vec![(record.txid, None)],
            later
        ));
        assert!(twin_separated(&[record], &checks, later).is_empty());
    }

    #[test]
    fn twin_candidates_reach_every_record_in_turn() {
        let (record, _) = split_fixture();
        let records: Vec<SplitRecord> = (0..(MAX_TWIN_LOOKUPS as u8 + 2))
            .map(|n| SplitRecord {
                txid: Txid::from_str(&format!("{:02x}", n).repeat(32)).unwrap(),
                created_at: u64::from(n),
                ..record.clone()
            })
            .collect();
        let mut checks = HashMap::new();
        let first = twin_candidates(&records, &checks);
        assert_eq!(first.len(), MAX_TWIN_LOOKUPS);
        assert_eq!(first[0], records.last().unwrap().txid, "newest first");
        let at = std::time::Instant::now();
        for txid in &first {
            checks.insert(*txid, TwinCheck { spent: None, at });
        }
        let second = twin_candidates(&records, &checks);
        assert_eq!(
            &second[..2],
            &[records[1].txid, records[0].txid],
            "the two never checked come next"
        );
    }

    #[test]
    fn only_a_newer_pass_applies() {
        let mut applied = 0;
        assert!(newer_pass(&mut applied, 2));
        assert!(!newer_pass(&mut applied, 1), "a slow older pass is dropped");
        assert!(!newer_pass(&mut applied, 2));
        assert!(newer_pass(&mut applied, 3));
        assert_eq!(applied, 3);
    }

    mod depth {
        use super::super::*;
        use crate::services::claim_observation::{FailureKind, FreshRead};
        use coincube_core::claim::BlockRef;
        use coincube_core::miniscript::bitcoin::{hashes::Hash, BlockHash};
        use reqwest::header::{HeaderMap, CACHE_CONTROL};
        use std::sync::Mutex;

        fn fresh<T>(chain: ChainId, value: T) -> FreshRead<T> {
            let mut headers = HeaderMap::new();
            headers.insert("x-cache", "BYPASS".parse().unwrap());
            headers.insert(CACHE_CONTROL, "private, no-store".parse().unwrap());
            FreshRead::from_response(chain, value, 1, &headers).unwrap()
        }

        /// Answers from one chain's view; records every request so tests
        /// can pin which chain, transaction and height were asked for.
        struct Source {
            /// Tip heights, one per `tip` call; the last repeats.
            tips: Vec<u64>,
            tx: TransactionObservation,
            at_height: BlockHash,
            /// Stamp replies with this chain instead of the requested one.
            answer_as: Option<ChainId>,
            asked: Mutex<Vec<String>>,
        }

        impl Source {
            fn new(tip: u64, tx: TransactionObservation) -> Self {
                Self {
                    tips: vec![tip],
                    tx,
                    at_height: block_hash(),
                    answer_as: None,
                    asked: Mutex::new(Vec::new()),
                }
            }
            fn chain(&self, asked: ChainId) -> ChainId {
                self.answer_as.unwrap_or(asked)
            }
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
                let mut asked = self.asked.lock().unwrap();
                let n = asked.iter().filter(|a| a.starts_with("tip")).count();
                asked.push(format!("tip {:?}", chain));
                let height = *self.tips.get(n).or(self.tips.last()).unwrap();
                Ok(fresh(
                    self.chain(chain),
                    BlockRef {
                        height,
                        hash: BlockHash::from_byte_array([height as u8; 32]),
                    },
                ))
            }
            async fn transaction(
                &self,
                chain: ChainId,
                txid: Txid,
            ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
                self.asked
                    .lock()
                    .unwrap()
                    .push(format!("tx {:?} {}", chain, txid));
                Ok(fresh(self.chain(chain), self.tx))
            }
            async fn hash_at_height(
                &self,
                chain: ChainId,
                height: u64,
            ) -> Result<FreshRead<BlockHash>, FailureKind> {
                self.asked
                    .lock()
                    .unwrap()
                    .push(format!("hash {:?} {}", chain, height));
                Ok(fresh(self.chain(chain), self.at_height))
            }
        }

        fn txid() -> Txid {
            Txid::from_byte_array([4; 32])
        }

        fn block_hash() -> BlockHash {
            BlockHash::from_byte_array([1; 32])
        }

        fn confirmed_at(height: u64) -> TransactionObservation {
            TransactionObservation::Confirmed {
                txid: txid(),
                block: BlockRef {
                    height,
                    hash: block_hash(),
                },
            }
        }

        #[tokio::test]
        async fn depth_counts_from_the_fresh_tip_on_the_asked_chain() {
            let source = Source::new(105, confirmed_at(100));
            assert_eq!(
                depth_on(&source, ChainId::BitcoinBlake2b, txid()).await,
                Some(6)
            );
            assert_eq!(
                *source.asked.lock().unwrap(),
                vec![
                    "tip BitcoinBlake2b".to_string(),
                    format!("tx BitcoinBlake2b {}", txid()),
                    "hash BitcoinBlake2b 100".to_string(),
                    "tip BitcoinBlake2b".to_string(),
                ]
            );
        }

        #[tokio::test]
        async fn a_block_off_the_best_chain_has_no_depth() {
            let mut source = Source::new(105, confirmed_at(100));
            source.at_height = BlockHash::from_byte_array([2; 32]);
            assert_eq!(
                depth_on(&source, ChainId::BitcoinBlake2b, txid()).await,
                None
            );
        }

        #[tokio::test]
        async fn a_tip_that_moves_during_the_reads_has_no_depth() {
            let mut source = Source::new(105, confirmed_at(100));
            source.tips = vec![105, 103];
            assert_eq!(
                depth_on(&source, ChainId::BitcoinBlake2b, txid()).await,
                None
            );
        }

        #[tokio::test]
        async fn a_reply_from_another_chain_has_no_depth() {
            let mut source = Source::new(105, confirmed_at(100));
            source.answer_as = Some(ChainId::Bitcoin);
            assert_eq!(
                depth_on(&source, ChainId::BitcoinBlake2b, txid()).await,
                None
            );
        }

        #[tokio::test]
        async fn another_transaction_or_a_block_above_the_tip_has_no_depth() {
            let other = TransactionObservation::Confirmed {
                txid: Txid::from_byte_array([5; 32]),
                block: BlockRef {
                    height: 100,
                    hash: block_hash(),
                },
            };
            for (tip, tx) in [(105, other), (99, confirmed_at(100))].iter().copied() {
                let source = Source::new(tip, tx);
                assert_eq!(
                    depth_on(&source, ChainId::BitcoinBlake2b, txid()).await,
                    None
                );
            }
        }

        #[tokio::test]
        async fn unconfirmed_or_absent_has_no_depth() {
            for tx in [
                TransactionObservation::Absent,
                TransactionObservation::Unconfirmed { txid: txid() },
            ]
            .iter()
            .copied()
            {
                let source = Source::new(105, tx);
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
