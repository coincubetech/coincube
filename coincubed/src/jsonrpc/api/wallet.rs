use crate::commands::CoinStatus;
use crate::{
    jsonrpc::rpc::{Error, Params},
    DaemonControl,
};
use miniscript::bitcoin;
use std::{convert::TryInto, str::FromStr};

pub(super) fn list_coins(
    control: &DaemonControl,
    params: Option<Params>,
) -> Result<serde_json::Value, Error> {
    let statuses_arg = params
        .as_ref()
        .and_then(|p| p.get(0, "statuses"))
        .and_then(|statuses| statuses.as_array());
    let statuses: Vec<CoinStatus> = if let Some(statuses_arg) = statuses_arg {
        statuses_arg
            .iter()
            .map(|status_arg| {
                status_arg
                    .as_str()
                    .and_then(CoinStatus::from_arg)
                    .ok_or_else(|| {
                        Error::invalid_params(format!(
                            "Invalid value {} in 'statuses' parameter.",
                            status_arg
                        ))
                    })
            })
            .collect::<Result<Vec<CoinStatus>, Error>>()?
    } else {
        Vec::new()
    };
    let outpoints_arg = params
        .as_ref()
        .and_then(|p| p.get(1, "outpoints"))
        .and_then(|op| op.as_array());
    let outpoints: Vec<bitcoin::OutPoint> = if let Some(outpoints_arg) = outpoints_arg {
        outpoints_arg
            .iter()
            .map(|op_arg| {
                op_arg
                    .as_str()
                    .and_then(|op| bitcoin::OutPoint::from_str(op).ok())
                    .ok_or_else(|| {
                        Error::invalid_params(format!(
                            "Invalid value {} in 'outpoints' parameter.",
                            op_arg
                        ))
                    })
            })
            .collect::<Result<Vec<bitcoin::OutPoint>, Error>>()?
    } else {
        Vec::new()
    };
    let res = control.list_coins(&statuses, &outpoints);
    Ok(serde_json::json!(&res))
}

fn get_opt_u32<Q>(params: &Option<Params>, index: usize, name: &Q) -> Result<Option<u32>, Error>
where
    String: std::borrow::Borrow<Q>,
    Q: ?Sized + Ord + Eq + std::hash::Hash + std::fmt::Display,
{
    Ok(
        if let Some(i) = params.as_ref().and_then(|p| p.get(index, name)) {
            Some(i.as_u64().and_then(|i| i.try_into().ok()).ok_or_else(|| {
                Error::invalid_params(format!("Invalid value for '{}': {}", name, i))
            })?)
        } else {
            None
        },
    )
}

pub(super) fn list_addresses(
    control: &DaemonControl,
    params: Option<Params>,
) -> Result<serde_json::Value, Error> {
    let start_index = get_opt_u32(&params, 0, "start_index")?;
    let count = get_opt_u32(&params, 1, "count")?;

    let res = &control.list_addresses(start_index, count)?;
    Ok(serde_json::json!(&res))
}

pub(super) fn list_revealed_addresses(
    control: &DaemonControl,
    params: Params,
) -> Result<serde_json::Value, Error> {
    let is_change = params
        .get(0, "is_change")
        .ok_or_else(|| Error::invalid_params("Missing 'is_change' parameter."))?
        .as_bool()
        .ok_or_else(|| Error::invalid_params("Invalid 'is_change' parameter."))?;
    let exclude_used = params
        .get(1, "exclude_used")
        .ok_or_else(|| Error::invalid_params("Missing 'exclude_used' parameter."))?
        .as_bool()
        .ok_or_else(|| Error::invalid_params("Invalid 'exclude_used' parameter."))?;
    let limit = params
        .get(2, "limit")
        .ok_or_else(|| Error::invalid_params("Missing 'limit' parameter."))?
        .as_u64()
        .and_then(|l| l.try_into().ok())
        .ok_or_else(|| Error::invalid_params("Invalid 'limit' parameter."))?;
    // A missing value and `null` are both mapped to `None`.
    let start_index = if let Some(ind) = params.get(3, "start_index") {
        if ind.as_null().is_some() {
            None
        } else {
            let ind_u32: u32 = ind
                .as_u64()
                .and_then(|ind_u64| ind_u64.try_into().ok())
                .ok_or_else(|| Error::invalid_params("Invalid 'start_index' parameter."))?;
            Some(ind_u32)
        }
    } else {
        None
    };

    let res = &control.list_revealed_addresses(
        is_change,
        exclude_used,
        limit,
        start_index.map(|ind| ind.into()),
    )?;
    Ok(serde_json::json!(&res))
}

pub(super) fn update_deriv_indexes(
    control: &DaemonControl,
    params: Params,
) -> Result<serde_json::Value, Error> {
    let receive = params.get(0, "receive");
    let change = params.get(1, "change");

    if receive.is_none() && change.is_none() {
        return Err(Error::invalid_params(
            "Missing 'receive' or 'change' parameter",
        ));
    }

    let receive = match receive {
        Some(i) => {
            let res = i.as_i64().ok_or(Error::invalid_params(
                "Invalid value for 'receive' param".to_string(),
            ))?;
            let res = res
                .try_into()
                .map_err(|_| Error::invalid_params("Invalid value for 'receive' param"))?;
            Some(res)
        }
        None => None,
    };

    let change = match change {
        Some(i) => {
            let res = i.as_i64().ok_or(Error::invalid_params(
                "Invalid value for 'change' param".to_string(),
            ))?;
            let res = res
                .try_into()
                .map_err(|_| Error::invalid_params("Invalid value for 'change' param"))?;
            Some(res)
        }
        None => None,
    };

    Ok(serde_json::json!(
        control.update_deriv_indexes(receive, change)?
    ))
}

pub(super) fn list_confirmed(
    control: &DaemonControl,
    params: Params,
) -> Result<serde_json::Value, Error> {
    let start: u32 = params
        .get(0, "start")
        .ok_or_else(|| Error::invalid_params("Missing 'start' parameter."))?
        .as_i64()
        .and_then(|i| i.try_into().ok())
        .ok_or_else(|| Error::invalid_params("Invalid 'start' parameter."))?;

    let end: u32 = params
        .get(1, "end")
        .ok_or_else(|| Error::invalid_params("Missing 'end' parameter."))?
        .as_i64()
        .and_then(|i| i.try_into().ok())
        .ok_or_else(|| Error::invalid_params("Invalid 'end' parameter."))?;

    let limit: u64 = params
        .get(2, "limit")
        .ok_or_else(|| Error::invalid_params("Missing 'limit' parameter."))?
        .as_i64()
        .and_then(|i| i.try_into().ok())
        .ok_or_else(|| Error::invalid_params("Invalid 'limit' parameter."))?;

    Ok(serde_json::json!(
        &control.list_confirmed_transactions(start, end, limit)
    ))
}

pub(super) fn list_transactions(
    control: &DaemonControl,
    params: Params,
) -> Result<serde_json::Value, Error> {
    let txids: Vec<bitcoin::Txid> = params
        .get(0, "txids")
        .ok_or_else(|| Error::invalid_params("Missing 'txids' parameter."))?
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .map(|entry| entry.as_str().and_then(|e| bitcoin::Txid::from_str(e).ok()))
                .collect()
        })
        .ok_or_else(|| Error::invalid_params("Invalid 'txids' parameter."))?;
    Ok(serde_json::json!(&control.list_transactions(&txids)))
}

pub(super) fn start_rescan(
    control: &mut DaemonControl,
    params: Params,
) -> Result<serde_json::Value, Error> {
    let timestamp: u32 = params
        .get(0, "timestamp")
        .ok_or_else(|| Error::invalid_params("Missing 'timestamp' parameter."))?
        .as_u64()
        .and_then(|t| t.try_into().ok())
        .ok_or_else(|| Error::invalid_params("Invalid 'timestamp' parameter."))?;
    control.start_rescan(timestamp)?;

    Ok(serde_json::json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn optional_address_bounds_preserve_named_and_positional_semantics() {
        assert_eq!(get_opt_u32(&None, 0, "start_index").unwrap(), None);
        for value in [json!([]), json!({})] {
            let params = Some(serde_json::from_value::<Params>(value).unwrap());
            assert_eq!(get_opt_u32(&params, 0, "start_index").unwrap(), None);
        }
        for value in [json!([u32::MAX]), json!({"start_index": u32::MAX})] {
            let params = Some(serde_json::from_value::<Params>(value).unwrap());
            assert_eq!(
                get_opt_u32(&params, 0, "start_index").unwrap(),
                Some(u32::MAX)
            );
        }
        for value in [
            json!(-1),
            json!(u64::from(u32::MAX) + 1),
            json!(1.5),
            json!("1"),
            json!(null),
        ] {
            for supplied in [json!([value]), json!({"start_index": value})] {
                let params = Some(serde_json::from_value::<Params>(supplied).unwrap());
                assert!(get_opt_u32(&params, 0, "start_index").is_err());
            }
        }
    }
}
