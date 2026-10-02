//! Test bridge for the two-chain regtest, Split steps 1 and 2 (#568 B6a,
//! B6b). Builds, reconstructs and finalizes: no keys, network, wallet writes,
//! fee source, freshness proof or Split authorization. Python supplies the
//! chain observations, the claimed prevouts, the step-2 target and the
//! signatures from disposable regtest keys, and checks node acceptance
//! separately.
//!
//! Without `step2` the request is a step 1 (the B6a shape). With `step2` the
//! bridge builds only step 2, from the same coins, source and fork height.
use coincube_core::{
    chain::ChainId,
    claim::BlockRef,
    foreign_split::{
        create_split_step1, create_split_step2, finalize_split_step1, finalize_split_step2,
        reconstruct_split_step1, reconstruct_split_step2, SplitBranch, SplitCoin, SplitInputs,
        SplitSource, SplitStep2Inputs,
    },
    miniscript::{
        bitcoin::{
            absolute::LockTime,
            consensus::encode::{deserialize_hex, serialize_hex},
            psbt::Psbt,
            secp256k1, BlockHash, OutPoint, ScriptBuf, Transaction,
        },
        Descriptor, DescriptorPublicKey,
    },
    split_poison::{split_poison_fork_marker, split_poison_script},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
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
    // Step 1 only.
    fork_marker: Option<BlockHash>,
    destination: Option<u32>,
    feerate_vb: Option<u64>,
    locktime: Option<u32>,
    bitcoin_tip_height: Option<u32>,
    signed_step1: Option<String>,
    /// An unsigned step 1 to rebuild from the same observations.
    recorded_step1: Option<String>,
    /// Present: build step 2 instead of step 1.
    step2: Option<Step2Request>,
}

#[derive(Deserialize)]
struct Step2Request {
    /// Step 1's claimed prevouts, `txid:vout`.
    claimed: Vec<String>,
    /// The target script, hex.
    target: String,
    feerate_vb: u64,
    locktime: u32,
    btcb2_tip_height: u32,
    signed: Option<String>,
    /// An unsigned step 2 to rebuild from the same observations.
    recorded: Option<String>,
}

fn required<T>(value: Option<T>, name: &str) -> Result<T, Box<dyn Error>> {
    value.ok_or_else(|| format!("step 1 needs `{name}`").into())
}

fn step1(
    request: &Request,
    source: &SplitSource,
    coins: &[SplitCoin],
) -> Result<Value, Box<dyn Error>> {
    // The chain label selects the production construction rules (and the
    // poison's chain byte). These vectors are sent only to isolated regtest
    // nodes; every supported output script is network-neutral.
    let inputs = SplitInputs {
        chain: ChainId::Bitcoin,
        source,
        coins,
        fork_height: request.fork_height,
        destination: required(request.destination, "destination")?,
    };
    let secp = secp256k1::Secp256k1::verification_only();
    let fork_marker = required(request.fork_marker, "fork_marker")?;
    let bitcoin_tip_height = required(request.bitcoin_tip_height, "bitcoin_tip_height")?;

    let step1 = create_split_step1(
        &inputs,
        required(request.feerate_vb, "feerate_vb")?,
        LockTime::from_height(required(request.locktime, "locktime")?)?,
        bitcoin_tip_height,
        fork_marker,
    )?;
    let claimed = step1.claimed_prevouts();
    // The production decoder and encoder, for Python to compare with its own
    // independent parse of the same bytes.
    let poison = &step1.psbt().unsigned_tx.output[0].script_pubkey;
    let rebuilt = split_poison_script(
        ChainId::Bitcoin,
        fork_marker,
        &claimed.iter().copied().collect::<BTreeSet<_>>(),
    );
    let mut result = json!({
        "step1_psbt": step1.psbt().to_string(),
        "unsigned_txid": step1.txid(),
        "maximum_signed_vbytes": step1.maximum_signed_vbytes(),
        "fee": step1.fee().to_sat(),
        "claimed_prevouts": claimed.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "poison_script": poison.to_hex_string(),
        "poison_fork_marker": split_poison_fork_marker(poison),
        "poison_rebuilds": rebuilt.as_ref() == Some(poison),
    });
    if let Some(recorded) = &request.recorded_step1 {
        let recorded: Transaction = deserialize_hex(recorded)?;
        let rebuilt = reconstruct_split_step1(&inputs, &recorded, bitcoin_tip_height)?;
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
    Ok(result)
}

fn step2(
    request: &Request,
    step2: &Step2Request,
    source: &SplitSource,
    coins: &[SplitCoin],
) -> Result<Value, Box<dyn Error>> {
    let claimed = step2
        .claimed
        .iter()
        .map(|outpoint| OutPoint::from_str(outpoint))
        .collect::<Result<Vec<_>, _>>()?;
    let target = ScriptBuf::from_hex(&step2.target)?;
    // As for step 1, the label selects the production rules; the target and
    // every spent script are network-neutral.
    let inputs = SplitStep2Inputs {
        chain: ChainId::BitcoinBlake2b,
        source,
        coins,
        fork_height: request.fork_height,
        claimed: &claimed,
        target: &target,
    };
    let secp = secp256k1::Secp256k1::verification_only();
    let built = create_split_step2(
        &inputs,
        step2.feerate_vb,
        LockTime::from_height(step2.locktime)?,
        step2.btcb2_tip_height,
    )?;
    let mut result = json!({
        "step2_psbt": built.psbt().to_string(),
        "unsigned_txid": built.txid(),
        "maximum_signed_vbytes": built.maximum_signed_vbytes(),
        "fee": built.fee().to_sat(),
        "claimed_prevouts": built
            .claimed_prevouts()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        "target": built.target().to_hex_string(),
    });
    if let Some(recorded) = &step2.recorded {
        let recorded: Transaction = deserialize_hex(recorded)?;
        let rebuilt = reconstruct_split_step2(&inputs, &recorded, step2.btcb2_tip_height)?;
        result["reconstructed_txid"] = json!(rebuilt.txid());
    }
    if let Some(signed) = &step2.signed {
        let verified =
            finalize_split_step2(&built, coins, source, &Psbt::from_str(signed)?, &secp)?;
        result["step2_raw"] = json!(serialize_hex(verified.transaction()));
        result["step2_txid"] = json!(verified.transaction().compute_txid());
        result["construction_txid"] = json!(verified.construction_txid());
        result["vsize"] = json!(verified.vsize());
        result["signatures_per_input"] = json!(verified.signatures_per_input());
    }
    Ok(result)
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
    let result = match &request.step2 {
        Some(step2_request) => step2(&request, step2_request, &source, &coins)?,
        None => step1(&request, &source, &coins)?,
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
