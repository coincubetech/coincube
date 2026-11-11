use crate::{
    jsonrpc::rpc::{Error, Request, Response},
    DaemonControl,
};

mod spend;
use spend::{
    broadcast_spend, create_recovery, create_spend, delete_spend, list_spendtxs, rbf_psbt,
    update_spend,
};
mod labels;
use labels::{get_labels, get_labels_bip329, update_labels};
mod wallet;
use wallet::{
    list_addresses, list_coins, list_confirmed, list_revealed_addresses, list_transactions,
    start_rescan, update_deriv_indexes,
};

/// Handle an incoming JSONRPC2 request.
pub fn handle_request(control: &mut DaemonControl, req: Request) -> Result<Response, Error> {
    let result = match req.method.as_str() {
        "broadcastspend" => {
            let params = req
                .params
                .ok_or_else(|| Error::invalid_params("Missing 'txid' parameter."))?;
            broadcast_spend(control, params)?
        }
        "createrecovery" => {
            let params = req.params.ok_or_else(|| {
                Error::invalid_params("Missing 'address' and 'feerate' parameters.")
            })?;
            create_recovery(control, params)?
        }
        "createspend" => {
            let params = req.params.ok_or_else(|| {
                Error::invalid_params(
                    "Missing 'outpoints', 'destinations' and 'feerate' parameters.",
                )
            })?;
            create_spend(control, params)?
        }
        "delspendtx" => {
            let params = req
                .params
                .ok_or_else(|| Error::invalid_params("Missing 'txid' parameter."))?;
            delete_spend(control, params)?
        }
        "rbfpsbt" => {
            let params = req.params.ok_or_else(|| {
                Error::invalid_params("Missing 'txid', 'feerate' and 'is_cancel' parameters.")
            })?;
            rbf_psbt(control, params)?
        }
        "getinfo" => serde_json::json!(&control.get_info()),
        "requestsync" => {
            control.request_sync();
            // Fire-and-forget — the daemon kicks the poller and
            // returns immediately. The GUI will see the result via
            // the next state refresh.
            serde_json::json!({})
        }
        "getnewaddress" => serde_json::json!(&control.try_get_new_address()?),
        "updatederivationindexes" => {
            let params = req.params.ok_or_else(|| {
                Error::invalid_params("Missing 'receive' or 'change' parameters.")
            })?;
            update_deriv_indexes(control, params)?
        }
        "listcoins" => {
            let params = req.params;
            list_coins(control, params)?
        }
        "listaddresses" => {
            let params = req.params;
            list_addresses(control, params)?
        }
        "listrevealedaddresses" => {
            let params = req.params.ok_or_else(|| {
                Error::invalid_params(
                    "The 'listrevealedaddresses' command requires 3 parameters: 'is_change', 'exclude_used' and 'limit'",
                )
            })?;
            list_revealed_addresses(control, params)?
        }
        "listconfirmed" => {
            let params = req.params.ok_or_else(|| {
                Error::invalid_params(
                    "The 'listconfirmed' command requires 3 parameters: 'start', 'end' and 'limit'",
                )
            })?;
            list_confirmed(control, params)?
        }
        "listspendtxs" => list_spendtxs(control, req.params)?,
        "listtransactions" => {
            let params = req.params.ok_or_else(|| {
                Error::invalid_params(
                    "The 'listtransactions' command requires 1 parameter: 'txids'",
                )
            })?;
            list_transactions(control, params)?
        }
        "startrescan" => {
            let params = req
                .params
                .ok_or_else(|| Error::invalid_params("Missing 'timestamp' parameter."))?;
            start_rescan(control, params)?
        }
        "stop" => serde_json::json!({}),
        "updatespend" => {
            let params = req
                .params
                .ok_or_else(|| Error::invalid_params("Missing 'psbt' parameter."))?;
            update_spend(control, params)?
        }
        "updatelabels" => {
            let params = req
                .params
                .ok_or_else(|| Error::invalid_params("Missing 'labels' parameter."))?;
            update_labels(control, params)?
        }
        "getlabels" => {
            let params = req
                .params
                .ok_or_else(|| Error::invalid_params("Missing 'items' parameter."))?;
            get_labels(control, params)?
        }
        "getlabelsbip329" => {
            let params = req
                .params
                .ok_or_else(|| Error::invalid_params("Missing 'offset' and 'limit' parameters."))?;
            get_labels_bip329(control, params)?
        }
        _ => {
            return Err(Error::method_not_found());
        }
    };

    Ok(Response::success(req.id, result))
}
