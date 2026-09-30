//! Test bridge for the two-chain regtest, Split step 1 only (#568 B6a).
//! Builds, reconstructs and finalizes: no keys, network, wallet writes, fee
//! source, freshness proof or Split authorization. Python supplies the chain
//! observations and the signatures from disposable regtest keys, and checks
//! node acceptance separately.
use coincube_core::{
    chain::ChainId,
    claim::BlockRef,
    foreign_split::{
        create_split_step1, finalize_split_step1, reconstruct_split_step1, SplitBranch, SplitCoin,
        SplitInputs, SplitSource,
    },
    miniscript::{
        bitcoin::{
            absolute::LockTime,
            consensus::encode::{deserialize_hex, serialize_hex},
            psbt::Psbt,
            secp256k1, BlockHash, OutPoint, Transaction,
        },
        Descriptor, DescriptorPublicKey,
    },
};
use serde::Deserialize;
use serde_json::json;
use std::{
    error::Error,
    io::{self, Read},
    str::FromStr,
};

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Branch {
    External,
    Internal,
}

#[derive(Deserialize)]
struct Coin {
    previous: String,
    vout: u32,
    branch: Branch,
    index: u32,
    bitcoin_block: Option<BlockRef>,
    btcb2_block: Option<BlockRef>,
}

#[derive(Deserialize)]
struct Request {
    external: String,
    internal: Option<String>,
    coins: Vec<Coin>,
    /// Observed on the BTCB2 node by the test; never a constant here.
    fork_height: u64,
    fork_marker: BlockHash,
    destination: u32,
    feerate_vb: u64,
    locktime: u32,
    bitcoin_tip_height: u32,
    signed_step1: Option<String>,
    /// An unsigned step 1 to rebuild from the same observations.
    recorded_step1: Option<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut input = String::new();
    io::stdin()
        .take(2 * 1024 * 1024)
        .read_to_string(&mut input)?;
    let request: Request = serde_json::from_str(&input)?;
    let external = Descriptor::<DescriptorPublicKey>::from_str(&request.external)?;
    let internal = request
        .internal
        .as_deref()
        .map(Descriptor::<DescriptorPublicKey>::from_str)
        .transpose()?;
    let source = SplitSource::new(external, internal)?;
    let mut coins = Vec::with_capacity(request.coins.len());
    for coin in &request.coins {
        let previous: Transaction = deserialize_hex(&coin.previous)?;
        coins.push(SplitCoin {
            outpoint: OutPoint::new(previous.compute_txid(), coin.vout),
            branch: match coin.branch {
                Branch::External => SplitBranch::External,
                Branch::Internal => SplitBranch::Internal,
            },
            index: coin.index,
            previous,
            bitcoin_block: coin.bitcoin_block,
            btcb2_block: coin.btcb2_block,
        });
    }
    // The chain label selects the production construction rules (and the
    // poison's chain byte). These vectors are sent only to isolated regtest
    // nodes; every supported output script is network-neutral.
    let inputs = SplitInputs {
        chain: ChainId::Bitcoin,
        source: &source,
        coins: &coins,
        fork_height: request.fork_height,
        destination: request.destination,
    };
    let secp = secp256k1::Secp256k1::verification_only();

    let step1 = create_split_step1(
        &inputs,
        request.feerate_vb,
        LockTime::from_height(request.locktime)?,
        request.bitcoin_tip_height,
        request.fork_marker,
    )?;
    let mut result = json!({
        "step1_psbt": step1.psbt().to_string(),
        "unsigned_txid": step1.txid(),
        "maximum_signed_vbytes": step1.maximum_signed_vbytes(),
        "fee": step1.fee().to_sat(),
        "claimed_prevouts": step1
            .claimed_prevouts()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
    });
    if let Some(recorded) = &request.recorded_step1 {
        let recorded: Transaction = deserialize_hex(recorded)?;
        let rebuilt = reconstruct_split_step1(&inputs, &recorded, request.bitcoin_tip_height)?;
        result["reconstructed_txid"] = json!(rebuilt.txid());
    }
    if let Some(signed) = &request.signed_step1 {
        let verified = finalize_split_step1(&step1, &Psbt::from_str(signed)?, &secp)?;
        result["step1_raw"] = json!(serialize_hex(verified.transaction()));
        result["step1_txid"] = json!(verified.transaction().compute_txid());
        result["construction_txid"] = json!(verified.construction_txid());
        result["vsize"] = json!(verified.vsize());
        result["signatures_per_input"] = json!(verified.signatures_per_input());
    }
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
