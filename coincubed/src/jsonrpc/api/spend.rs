use crate::{
    jsonrpc::rpc::{Error, Params},
    DaemonControl,
};
use miniscript::bitcoin::{self, psbt::Psbt, Txid};
use std::{collections::HashMap, convert::TryInto, str::FromStr};

pub(super) fn create_spend(
    control: &DaemonControl,
    params: Params,
) -> Result<serde_json::Value, Error> {
    let destinations = params
        .get(0, "destinations")
        .ok_or_else(|| Error::invalid_params("Missing 'destinations' parameter."))?
        .as_object()
        .and_then(|obj| {
            obj.into_iter()
                .map(|(k, v)| {
                    let addr = bitcoin::Address::from_str(k).ok()?;
                    let amount: u64 = v.as_i64()?.try_into().ok()?;
                    Some((addr, amount))
                })
                .collect::<Option<HashMap<bitcoin::Address<bitcoin::address::NetworkUnchecked>, u64>>>()
        })
        .ok_or_else(|| Error::invalid_params("Invalid 'destinations' parameter."))?;
    let outpoints = params
        .get(1, "outpoints")
        .ok_or_else(|| Error::invalid_params("Missing 'outpoints' parameter."))?
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .map(|entry| {
                    entry
                        .as_str()
                        .and_then(|e| bitcoin::OutPoint::from_str(e).ok())
                })
                .collect::<Option<Vec<bitcoin::OutPoint>>>()
        })
        .ok_or_else(|| Error::invalid_params("Invalid 'outpoints' parameter."))?;
    let feerate: u64 = params
        .get(2, "feerate")
        .ok_or_else(|| Error::invalid_params("Missing 'feerate' parameter."))?
        .as_u64()
        .ok_or_else(|| Error::invalid_params("Invalid 'feerate' parameter."))?;
    let change_address: Option<bitcoin::Address<bitcoin::address::NetworkUnchecked>> = params
        .get(3, "change_address")
        .map(|addr| {
            let addr_str = addr.as_str().ok_or_else(|| {
                Error::invalid_params("Invalid 'change_address' parameter: must be a string.")
            })?;
            bitcoin::Address::from_str(addr_str).map_err(|e| {
                Error::invalid_params(format!("Invalid 'change_address' parameter: {}.", e))
            })
        })
        .transpose()?;

    let res = control.create_spend(&destinations, &outpoints, feerate, change_address)?;
    Ok(serde_json::json!(&res))
}

pub(super) fn update_spend(
    control: &DaemonControl,
    params: Params,
) -> Result<serde_json::Value, Error> {
    let psbt: Psbt = params
        .get(0, "psbt")
        .ok_or_else(|| Error::invalid_params("Missing 'psbt' parameter."))?
        .as_str()
        .and_then(|s| Psbt::from_str(s).ok())
        .ok_or_else(|| Error::invalid_params("Invalid 'psbt' parameter."))?;
    control.update_spend(psbt)?;

    Ok(serde_json::json!({}))
}

pub(super) fn delete_spend(
    control: &DaemonControl,
    params: Params,
) -> Result<serde_json::Value, Error> {
    let txid = params
        .get(0, "txid")
        .ok_or_else(|| Error::invalid_params("Missing 'txid' parameter."))?
        .as_str()
        .and_then(|s| bitcoin::Txid::from_str(s).ok())
        .ok_or_else(|| Error::invalid_params("Invalid 'txid' parameter."))?;
    control.delete_spend(&txid);

    Ok(serde_json::json!({}))
}

pub(super) fn broadcast_spend(
    control: &DaemonControl,
    params: Params,
) -> Result<serde_json::Value, Error> {
    let txid = params
        .get(0, "txid")
        .ok_or_else(|| Error::invalid_params("Missing 'txid' parameter."))?
        .as_str()
        .and_then(|s| bitcoin::Txid::from_str(s).ok())
        .ok_or_else(|| Error::invalid_params("Invalid 'txid' parameter."))?;
    control.broadcast_spend(&txid)?;

    Ok(serde_json::json!({}))
}

pub(super) fn rbf_psbt(
    control: &DaemonControl,
    params: Params,
) -> Result<serde_json::Value, Error> {
    let txid = params
        .get(0, "txid")
        .ok_or_else(|| Error::invalid_params("Missing 'txid' parameter."))?
        .as_str()
        .and_then(|s| bitcoin::Txid::from_str(s).ok())
        .ok_or_else(|| Error::invalid_params("Invalid 'txid' parameter."))?;
    let is_cancel: bool = params
        .get(1, "is_cancel")
        .ok_or_else(|| Error::invalid_params("Missing 'is_cancel' parameter."))?
        .as_bool()
        .ok_or_else(|| Error::invalid_params("Invalid 'is_cancel' parameter."))?;
    let feerate_vb: Option<u64> = if let Some(feerate) = params.get(2, "feerate") {
        Some(
            feerate
                .as_u64()
                .ok_or_else(|| Error::invalid_params("Invalid 'feerate' parameter."))?,
        )
    } else {
        None
    };
    let res = control.rbf_psbt(&txid, is_cancel, feerate_vb)?;
    Ok(serde_json::json!(&res))
}

pub(super) fn list_spendtxs(
    control: &DaemonControl,
    params: Option<Params>,
) -> Result<serde_json::Value, Error> {
    let txids: Option<Vec<bitcoin::Txid>> = if let Some(p) = params {
        let tx_ids = p.get(0, "txids");
        if let Some(ids) = tx_ids {
            let ids: Vec<Txid> = ids
                .as_array()
                .and_then(|arr| {
                    arr.iter()
                        .map(|entry| entry.as_str().and_then(|e| bitcoin::Txid::from_str(e).ok()))
                        .collect()
                })
                .ok_or_else(|| Error::invalid_params("Invalid 'txids' parameter."))?;
            Some(ids)
        } else {
            None
        }
    } else {
        None
    };

    Ok(serde_json::json!(&control.list_spend(txids)?))
}

pub(super) fn create_recovery(
    control: &DaemonControl,
    params: Params,
) -> Result<serde_json::Value, Error> {
    let address = params
        .get(0, "address")
        .ok_or_else(|| Error::invalid_params("Missing 'address' parameter."))?
        .as_str()
        .and_then(|s| bitcoin::Address::from_str(s).ok())
        .ok_or_else(|| Error::invalid_params("Invalid 'address' parameter."))?;
    let feerate: u64 = params
        .get(1, "feerate")
        .ok_or_else(|| Error::invalid_params("Missing 'feerate' parameter."))?
        .as_u64()
        .ok_or_else(|| Error::invalid_params("Invalid 'feerate' parameter."))?;
    let timelock: Option<u16> = params
        .get(2, "timelock")
        .map(|tl| {
            tl.as_u64()
                .and_then(|tl| tl.try_into().ok())
                .ok_or_else(|| Error::invalid_params("Invalid 'timelock' parameter."))
        })
        .transpose()?;
    let outpoints = params
        .get(3, "outpoints")
        .map(|param| {
            param
                .as_array()
                .and_then(|arr| {
                    arr.iter()
                        .map(|entry| {
                            entry
                                .as_str()
                                .and_then(|e| bitcoin::OutPoint::from_str(e).ok())
                        })
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| Error::invalid_params("Invalid 'outpoints' parameter."))
        })
        .transpose()?
        .unwrap_or_default(); // missing is same as empty array

    let res = control.create_recovery(address, &outpoints, feerate, timelock)?;
    Ok(serde_json::json!(&res))
}
