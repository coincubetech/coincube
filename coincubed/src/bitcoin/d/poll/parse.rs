//! Strict fallible decoding for the wallet responses used by a poll.
use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct Received {
    txid: bitcoin::Txid,
    vout: u32,
    amount: f64,
    blockheight: Option<i32>,
    address: bitcoin::Address<address::NetworkUnchecked>,
    parent_descs: Vec<String>,
}

pub(super) fn coins(value: Json) -> Result<LSBlockRes, String> {
    let entries = value
        .get("transactions")
        .and_then(Json::as_array)
        .ok_or("Missing transaction list")?;
    let mut received_coins = Vec::new();
    for entry in entries {
        let category = entry
            .get("category")
            .and_then(Json::as_str)
            .ok_or("Missing transaction category")?;
        if !["receive", "generate", "immature"].contains(&category) {
            continue;
        }
        let item: Received =
            serde_json::from_value(entry.clone()).map_err(|_| "Malformed received coin")?;
        if item.blockheight.is_some_and(|h| h < 0) {
            return Err("Invalid received coin height".into());
        }
        let amount =
            bitcoin::Amount::from_btc(item.amount).map_err(|_| "Invalid received amount")?;
        let parent_descs = item
            .parent_descs
            .into_iter()
            .map(|s| {
                s.parse::<descriptor::Descriptor<descriptor::DescriptorPublicKey>>()
                    .map_err(|_| "Invalid parent descriptor".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        received_coins.push(LSBlockEntry {
            outpoint: bitcoin::OutPoint {
                txid: item.txid,
                vout: item.vout,
            },
            amount,
            block_height: item.blockheight,
            address: item.address,
            parent_descs,
            is_immature: category == "immature",
        });
    }
    Ok(LSBlockRes { received_coins })
}

#[derive(Deserialize)]
struct Transaction {
    hex: String,
    confirmations: i32,
    confirmations_assumed: Option<u32>,
    blockhash: Option<bitcoin::BlockHash>,
    blockheight: Option<i32>,
    blocktime: Option<u32>,
    #[serde(default)]
    walletconflicts: Vec<bitcoin::Txid>,
    generated: Option<bool>,
}

pub(super) fn transaction(value: Json, expected: bitcoin::Txid) -> Result<GetTxRes, String> {
    let item: Transaction =
        serde_json::from_value(value).map_err(|_| "Malformed wallet transaction")?;
    let bytes = Vec::from_hex(&item.hex).map_err(|_| "Invalid transaction hex")?;
    let tx: bitcoin::Transaction =
        bitcoin::consensus::deserialize(&bytes).map_err(|_| "Invalid transaction bytes")?;
    if tx.compute_txid() != expected {
        return Err("Wallet transaction id does not match the request".into());
    }
    let is_coinbase = tx.is_coinbase();
    if item
        .generated
        .is_some_and(|generated| generated != is_coinbase)
    {
        return Err("Inconsistent coinbase metadata".into());
    }
    let block = match (item.blockhash, item.blockheight, item.blocktime) {
        (None, None, None) if item.confirmations <= 0 => None,
        (Some(hash), Some(height), Some(time))
            if height >= 0
                && (item.confirmations > 0
                    || (item.confirmations == 0
                        && item.confirmations_assumed.is_some_and(|n| n > 0))) =>
        {
            Some(Block { hash, height, time })
        }
        _ => return Err("Inconsistent transaction confirmation metadata".into()),
    };
    Ok(GetTxRes {
        tx,
        block,
        is_coinbase,
        confirmations: item.confirmations,
        conflicting_txs: item.walletconflicts,
    })
}
