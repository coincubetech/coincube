use crate::commands::LabelItem;
use crate::{
    jsonrpc::rpc::{Error, Params},
    DaemonControl,
};
use std::{
    collections::{HashMap, HashSet},
    convert::TryInto,
};

pub(super) fn update_labels(
    control: &DaemonControl,
    params: Params,
) -> Result<serde_json::Value, Error> {
    let mut items = HashMap::new();
    for (item, value) in params
        .get(0, "labels")
        .ok_or_else(|| Error::invalid_params("Missing 'labels' parameter."))?
        .as_object()
        .ok_or_else(|| Error::invalid_params("Invalid 'labels' parameter."))?
        .iter()
    {
        let value = value.as_str().map(|s| s.to_string());
        if let Some(value) = &value {
            if value.len() > 100 {
                return Err(Error::invalid_params(format!(
                    "Invalid 'labels.{}' value length: must be less or equal than 100 characters",
                    item
                )));
            }
        }
        let item =
            LabelItem::from_str(item, control.config.bitcoin_config.network).ok_or_else(|| {
                Error::invalid_params(format!(
                    "Invalid 'labels.{}' parameter: must be an address, a txid or an outpoint",
                    item
                ))
            })?;
        items.insert(item, value);
    }

    control.update_labels(&items);
    Ok(serde_json::json!({}))
}

pub(super) fn get_labels(
    control: &DaemonControl,
    params: Params,
) -> Result<serde_json::Value, Error> {
    let mut items = HashSet::new();
    for item in params
        .get(0, "items")
        .ok_or_else(|| Error::invalid_params("Missing 'items' parameter."))?
        .as_array()
        .ok_or_else(|| Error::invalid_params("Invalid 'items' parameter."))?
        .iter()
    {
        let item = item.as_str().ok_or_else(|| {
            Error::invalid_params(format!(
                "Invalid item {} format: must be an address, a txid or an outpoint",
                item
            ))
        })?;

        let item =
            LabelItem::from_str(item, control.config.bitcoin_config.network).ok_or_else(|| {
                Error::invalid_params(format!(
                    "Invalid item {} format: must be an address, a txid or an outpoint",
                    item
                ))
            })?;
        items.insert(item);
    }

    Ok(serde_json::json!(control.get_labels(&items)))
}

pub(super) fn get_labels_bip329(
    control: &DaemonControl,
    params: Params,
) -> Result<serde_json::Value, Error> {
    let offset: u32 = params
        .get(0, "offset")
        .ok_or_else(|| Error::invalid_params("Missing 'offset' parameter."))?
        .as_u64()
        .and_then(|t| t.try_into().ok())
        .ok_or_else(|| Error::invalid_params("Invalid 'offset' parameter."))?;
    let limit: u32 = params
        .get(1, "limit")
        .ok_or_else(|| Error::invalid_params("Missing 'limit' parameter."))?
        .as_u64()
        .and_then(|t| t.try_into().ok())
        .ok_or_else(|| Error::invalid_params("Invalid 'limit' parameter."))?;
    Ok(serde_json::json!(control.get_labels_bip329(offset, limit)))
}
