use std::convert::{TryFrom, TryInto};

use bdk_electrum::bdk_chain::{
    bitcoin, local_chain::CheckPoint, BlockId, ConfirmationTimeHeightAnchor, TxGraph,
};

use crate::bitcoin::{BlockChainTip, BlockInfo};

pub fn height_u32_from_i32(height: i32) -> u32 {
    height.try_into().expect("height must fit into u32")
}

pub fn height_i32_from_u32(height: u32) -> i32 {
    height.try_into().expect("height must fit into i32")
}

/// A block height reported by the Electrum server, or `None` if it does not fit
/// into our `i32` heights. The value comes off the wire, so it is checked
/// rather than trusted: a panic here would be under the backend lock (#616).
pub fn height_i32_from_usize(height: usize) -> Option<i32> {
    height.try_into().ok()
}

/// Refuse a chain update whose tip height does not fit into our `i32` heights,
/// before any of it is applied (#616, #621). BDK takes the tip height from the
/// server as a `u32`; applied, it would make every later read of the wallet tip
/// panic under the backend lock. The rest of the update is at or below its tip.
///
/// The refusal fails the poll, which the poller logs only at debug level, so it
/// is logged here at warn: a server that keeps reporting such a tip stalls the
/// sync, and that must be diagnosable at the default log level. `backend` names
/// the backend kind only, never its address.
pub fn check_chain_update_height(backend: &str, chain_update: &CheckPoint) -> Result<(), u64> {
    let height = chain_update.height();
    if i32::try_from(height).is_ok() {
        return Ok(());
    }
    log::warn!(
        "Refused the {} chain update: the server reported block height {}, which is out of range. \
         The wallet stays at its last tip and the next poll retries.",
        backend,
        height
    );
    Err(height.into())
}

/// What [`check_graph_update_anchors`] found out of range in a confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorOutOfRange {
    /// A block height that does not fit into our `i32` heights.
    Height(u64),
    /// A block time that does not fit into our `u32` times.
    Time(u64),
}

/// Refuse a graph update with a confirmation that does not fit our types, before
/// any of it is applied (#621): anchored at a height that does not fit into our
/// `i32` heights, or with a block time that does not fit into a `u32`. Esplora
/// takes both from the server per transaction (`block_height: u32`,
/// `block_time: u64`), independently of the chain tip; applied, reading that
/// transaction or its coins would panic under the backend lock. Logged at warn
/// for the same reason as [`check_chain_update_height`].
pub fn check_graph_update_anchors(
    backend: &str,
    graph_update: &TxGraph<ConfirmationTimeHeightAnchor>,
) -> Result<(), AnchorOutOfRange> {
    for (anchor, _) in graph_update.all_anchors() {
        for height in [anchor.confirmation_height, anchor.anchor_block.height] {
            if i32::try_from(height).is_err() {
                log::warn!(
                    "Refused the {} graph update: the server reported a confirmation at block \
                     height {}, which is out of range. The wallet stays as it was and the next \
                     poll retries.",
                    backend,
                    height
                );
                return Err(AnchorOutOfRange::Height(height.into()));
            }
        }
        if u32::try_from(anchor.confirmation_time).is_err() {
            log::warn!(
                "Refused the {} graph update: the server reported a confirmation with block \
                 time {}, which is out of range. The wallet stays as it was and the next poll \
                 retries.",
                backend,
                anchor.confirmation_time
            );
            return Err(AnchorOutOfRange::Time(anchor.confirmation_time));
        }
    }
    Ok(())
}

pub fn height_usize_from_i32(height: i32) -> usize {
    height.try_into().expect("height must fit into usize")
}

pub fn block_id_from_tip(tip: BlockChainTip) -> BlockId {
    BlockId {
        height: height_u32_from_i32(tip.height),
        hash: tip.hash,
    }
}

pub fn tip_from_block_id(id: BlockId) -> BlockChainTip {
    BlockChainTip {
        height: height_i32_from_u32(id.height),
        hash: id.hash,
    }
}

pub fn block_info_from_anchor(anchor: ConfirmationTimeHeightAnchor) -> BlockInfo {
    BlockInfo {
        height: height_i32_from_u32(anchor.confirmation_height),
        time: anchor
            .confirmation_time
            .try_into()
            .expect("u32 by consensus"),
    }
}

/// Get the transaction's outpoints.
pub fn outpoints_from_tx(tx: &bitcoin::Transaction) -> Vec<bitcoin::OutPoint> {
    let txid = tx.compute_txid();
    (0..tx.output.len())
        .map(|i| {
            bitcoin::OutPoint::new(txid, i.try_into().expect("num tx outputs must fit in u32"))
        })
        .collect::<Vec<_>>()
}
