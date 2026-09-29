//! Test bridge for the two-chain regtest. Builds/finalizes only: no keys,
//! network, wallet writes, or Claim authorization. Python supplies signatures
//! from disposable regtest keys and separately checks node acceptance.
use coincube_core::{
    chain::ChainId,
    claim_ancestry::{self, Link},
    claim_finalize::{
        finalize_ancestry_transfer, finalize_claim_fork_sweep, finalize_poison_transfer,
    },
    claim_spend::{
        create_ancestry_fork_sweep, create_ancestry_self_transfer, create_claim_fork_sweep,
        create_poison_self_transfer,
    },
    descriptors::CoincubeDescriptor,
    miniscript::bitcoin::{
        absolute,
        bip32::ChildNumber,
        consensus::encode::{deserialize_hex, serialize, serialize_hex},
        psbt::Psbt,
        secp256k1, BlockHash, OutPoint, Transaction, Txid,
    },
    psbt_unified::UnifiedPsbt,
    spend::{CandidateCoin, TxGetter},
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::HashMap,
    error::Error,
    io::{self, Read},
    str::FromStr,
};

#[derive(Deserialize)]
struct Coin {
    previous: String,
    vout: u32,
    index: u32,
}
#[derive(Deserialize)]
struct Request {
    descriptor: String,
    fork_marker: BlockHash,
    coins: Vec<Coin>,
    /// Direct coinbase dependency for isolated consensus tests only.
    coinbase_input: Option<OutPoint>,
    signed_step1: Option<String>,
    signed_fork: Option<String>,
}
struct Transactions(HashMap<Txid, Transaction>);
impl TxGetter for Transactions {
    fn get_tx(&mut self, txid: &Txid) -> Option<Transaction> {
        self.0.get(txid).cloned()
    }
}
fn main() -> Result<(), Box<dyn Error>> {
    let mut input = String::new();
    io::stdin()
        .take(2 * 1024 * 1024)
        .read_to_string(&mut input)?;
    let request: Request = serde_json::from_str(&input)?;
    let descriptor = CoincubeDescriptor::from_str(&request.descriptor)?;
    let secp = secp256k1::Secp256k1::verification_only();
    let mut previous = Transactions(HashMap::new());
    let mut coins = Vec::new();
    for coin in &request.coins {
        let tx: Transaction = deserialize_hex(&coin.previous)?;
        let amount = tx
            .output
            .get(coin.vout as usize)
            .ok_or("missing previous output")?
            .value;
        let txid = tx.compute_txid();
        previous.0.insert(txid, tx);
        coins.push(CandidateCoin {
            outpoint: OutPoint::new(txid, coin.vout),
            amount,
            deriv_index: ChildNumber::from_normal_idx(coin.index)?,
            is_change: false,
            must_select: true,
            sequence: None,
            ancestor_info: None,
        });
    }
    if let Some(selected) = request.coinbase_input {
        let raw = serialize(previous.0.get(&selected.txid).ok_or("missing coinbase")?);
        let dependency = claim_ancestry::verify(
            selected,
            &[Link {
                transaction: &raw,
                parent_input: None,
            }],
        )
        .map_err(|error| format!("invalid coinbase dependency: {:?}", error))?;
        // Structural dependency only. No regtest height is admitted by the
        // mainnet canonical-pair verifier and no eligibility is manufactured.
        let source = create_ancestry_self_transfer(
            ChainId::Bitcoin,
            &descriptor,
            &secp,
            &mut previous,
            &coins,
            ChildNumber::from_normal_idx(100)?,
            5,
            absolute::LockTime::ZERO,
            &dependency,
        )?;
        let mut result = json!({"step1_psbt": source.psbt().to_string()});
        if let Some(signed) = &request.signed_step1 {
            let verified = finalize_ancestry_transfer(&source, &Psbt::from_str(signed)?, &secp)?;
            result["step1_raw"] = json!(serialize_hex(verified.transaction()));
            result["step1_txid"] = json!(verified.transaction().compute_txid());
            let shared: Vec<_> = coins
                .iter()
                .filter(|coin| coin.outpoint != selected)
                .copied()
                .collect();
            // A fork builder must not look up the Bitcoin-only transaction.
            previous.0.remove(&selected.txid);
            let fork = create_ancestry_fork_sweep(
                &source,
                ChainId::BitcoinBlake2b,
                &secp,
                &mut previous,
                &shared,
                ChildNumber::from_normal_idx(101)?,
                5,
                absolute::LockTime::ZERO,
            )?;
            result["fork_psbt"] = json!(fork.psbt().to_string());
            if let Some(signed) = &request.signed_fork {
                let signed = UnifiedPsbt::from_psbt(Psbt::from_str(signed)?)?;
                let verified = finalize_claim_fork_sweep(&fork, &signed, &secp)?;
                result["fork_raw"] = json!(serialize_hex(verified.transaction()));
                result["fork_txid"] = json!(verified.transaction().compute_txid());
            }
        }
        println!("{}", serde_json::to_string(&result)?);
        return Ok(());
    }
    // Network labels select the production construction rules. These vectors
    // are sent only to isolated regtest nodes; P2WSH scripts are network-neutral.
    let source = create_poison_self_transfer(
        ChainId::Bitcoin,
        &descriptor,
        &secp,
        &mut previous,
        &coins,
        ChildNumber::from_normal_idx(100)?,
        5,
        absolute::LockTime::ZERO,
        request.fork_marker,
    )?;
    let mut result = json!({"step1_psbt": source.psbt().to_string()});
    if let Some(signed) = request.signed_step1 {
        let verified = finalize_poison_transfer(&source, &Psbt::from_str(&signed)?, &secp)?;
        result["step1_raw"] = json!(serialize_hex(verified.transaction()));
        result["step1_txid"] = json!(verified.transaction().compute_txid());
        let fork = create_claim_fork_sweep(
            &source,
            ChainId::BitcoinBlake2b,
            &secp,
            &mut previous,
            &coins,
            ChildNumber::from_normal_idx(101)?,
            5,
            absolute::LockTime::ZERO,
        )?;
        result["fork_psbt"] = json!(fork.psbt().to_string());
        if let Some(signed) = request.signed_fork {
            let signed = UnifiedPsbt::from_psbt(Psbt::from_str(&signed)?)?;
            let verified = finalize_claim_fork_sweep(&fork, &signed, &secp)?;
            result["fork_raw"] = json!(serialize_hex(verified.transaction()));
            result["fork_txid"] = json!(verified.transaction().compute_txid());
        }
    }
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
