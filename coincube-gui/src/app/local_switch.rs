//! When a Vault may move from COINCUBE | Connect to the local node.
//!
//! Two things can make a switch to the local node lose what the user sees:
//!
//! * **A scan in flight.** Stopping the Connect backend discards a running
//!   address scan (`coincubed`'s Esplora full scan is all-or-nothing), and the
//!   restarted backend starts over. An automatic switch is therefore deferred
//!   until the scan has finished, and a manual one asks first.
//! * **A pruned node.** A pruned node only keeps blocks from its prune height
//!   on, and cannot rescan below it. Switching a Vault whose coins are older
//!   than that would show it empty, with no way for the node to recover them.
//!
//! Everything here is a pure decision over values the app already has, so it
//! is testable without a daemon or a node.

use std::convert::TryFrom;
use std::fmt;

/// What the local node's `getblockchaininfo` says about pruning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodePruning {
    /// The node keeps every block.
    Unpruned,
    /// The node keeps blocks only from `prune_height` on
    /// (`getblockchaininfo.pruneheight`: the lowest height still stored).
    Pruned { prune_height: u64 },
}

impl NodePruning {
    /// Read `pruned` / `pruneheight` from a `getblockchaininfo` result.
    ///
    /// `None` when the answer does not say: a missing `pruned`, or a pruned
    /// node that does not report its height. Not knowing is not the same as
    /// unpruned, so the caller must not switch on it.
    pub fn from_blockchain_info(result: &serde_json::Value) -> Option<Self> {
        match result["pruned"].as_bool()? {
            false => Some(Self::Unpruned),
            true => result["pruneheight"]
                .as_u64()
                .map(|prune_height| Self::Pruned { prune_height }),
        }
    }
}

/// How far back this Vault's history reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultHistory {
    /// The lowest block height of any coin the Vault has ever had, spent coins
    /// included.
    From(u32),
    /// No coin with a block height. `rescan_owed` is whether the Vault still
    /// carries a recorded rescan obligation (`WalletSettings::pending_rescan`):
    /// a restored Vault whose history has not been found yet, so how far back
    /// it reaches is unknown.
    NoConfirmedCoins { rescan_owed: bool },
}

impl VaultHistory {
    /// Build from the block heights of **all** the Vault's coins: unconfirmed,
    /// confirmed, spending and spent. Spent coins have to be included — a
    /// Vault whose history is entirely spent still needs those blocks for its
    /// transaction list, and would otherwise look like one with no history.
    pub fn from_coin_heights(
        heights: impl IntoIterator<Item = Option<i32>>,
        rescan_owed: bool,
    ) -> Self {
        heights
            .into_iter()
            .flatten()
            .filter_map(|h| u32::try_from(h).ok())
            .min()
            .map(Self::From)
            .unwrap_or(Self::NoConfirmedCoins { rescan_owed })
    }
}

/// Why a pruned node cannot serve this Vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrunedHistory {
    /// The Vault has coins from `earliest`, below the node's prune height.
    CoinsBelowPrune { prune_height: u64, earliest: u32 },
    /// The Vault's history has not been found yet (a restore still owing its
    /// rescan), so it may lie anywhere below the prune height.
    HistoryUnknown { prune_height: u64 },
}

impl fmt::Display for PrunedHistory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CoinsBelowPrune {
                prune_height,
                earliest,
            } => write!(
                f,
                "Your local node keeps blocks only from height {prune_height}, but this Vault \
                 has coins from block {earliest}. Switching would hide them. Keep using \
                 COINCUBE | Connect, or re-sync the node without pruning."
            ),
            Self::HistoryUnknown { prune_height } => write!(
                f,
                "Your local node keeps blocks only from height {prune_height}, and this \
                 restored Vault's history hasn't been found yet, so it may be older than \
                 that. Switching could hide it. Keep using COINCUBE | Connect until the \
                 Vault shows its coins, or re-sync the node without pruning."
            ),
        }
    }
}

/// Whether a node with `pruning` can show this Vault's coins.
///
/// `Ok(())` for an unpruned node whatever the history, and for a pruned one
/// that still has every block the Vault needs. A pruned node whose prune height
/// is above the Vault's earliest coin, or a pruned node and a Vault whose
/// history is unknown, cannot.
pub fn pruned_node_serves(
    pruning: NodePruning,
    history: VaultHistory,
) -> Result<(), PrunedHistory> {
    let NodePruning::Pruned { prune_height } = pruning else {
        return Ok(());
    };
    match history {
        VaultHistory::From(earliest) if prune_height > u64::from(earliest) => {
            Err(PrunedHistory::CoinsBelowPrune {
                prune_height,
                earliest,
            })
        }
        VaultHistory::From(_) => Ok(()),
        VaultHistory::NoConfirmedCoins { rescan_owed: true } => {
            Err(PrunedHistory::HistoryUnknown { prune_height })
        }
        VaultHistory::NoConfirmedCoins { rescan_owed: false } => Ok(()),
    }
}

/// The scan a switch would discard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunningScan {
    /// The wallet is catching up with the chain — for a Connect Vault this
    /// includes its startup address scan.
    WalletSync,
    /// A rescan the user (or a restore) started is still running.
    Rescan,
}

/// The scan in flight, if any. A rescan is reported first since it is the
/// longer of the two to redo.
pub fn running_scan(wallet_is_syncing: bool, rescan_pending: bool) -> Option<RunningScan> {
    if rescan_pending {
        Some(RunningScan::Rescan)
    } else if wallet_is_syncing {
        Some(RunningScan::WalletSync)
    } else {
        None
    }
}

/// Why an automatic switch to the local node is not happening yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchHold {
    /// Deferred until the scan finishes; retried on every node probe.
    Scanning(RunningScan),
    /// The node's pruning, or the Vault's history on a pruned node, has not
    /// been read yet. Retried on every node probe.
    Unknown,
    /// The node is pruned past this Vault's history. Not retried until either
    /// side changes.
    Pruned(PrunedHistory),
}

/// Whether an automatic switch to an already-synced local node may go ahead
/// now, and if not, why.
///
/// `history` is only needed for a pruned node; `None` means it could not be
/// read on this probe.
pub fn auto_switch_hold(
    scan: Option<RunningScan>,
    pruning: Option<NodePruning>,
    history: Option<VaultHistory>,
) -> Option<SwitchHold> {
    if let Some(scan) = scan {
        return Some(SwitchHold::Scanning(scan));
    }
    match pruning {
        None => Some(SwitchHold::Unknown),
        Some(NodePruning::Unpruned) => None,
        Some(pruned @ NodePruning::Pruned { .. }) => match history {
            None => Some(SwitchHold::Unknown),
            Some(history) => pruned_node_serves(pruned, history)
                .err()
                .map(SwitchHold::Pruned),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRUNE: u64 = 969_938;
    const PRUNED: NodePruning = NodePruning::Pruned {
        prune_height: PRUNE,
    };

    #[test]
    fn unpruned_node_serves_any_history() {
        for history in [
            VaultHistory::From(1),
            VaultHistory::NoConfirmedCoins { rescan_owed: true },
            VaultHistory::NoConfirmedCoins { rescan_owed: false },
        ] {
            assert_eq!(pruned_node_serves(NodePruning::Unpruned, history), Ok(()));
        }
    }

    #[test]
    fn pruned_node_serves_coins_at_or_above_its_prune_height() {
        assert_eq!(
            pruned_node_serves(PRUNED, VaultHistory::From(PRUNE as u32)),
            Ok(())
        );
        assert_eq!(
            pruned_node_serves(PRUNED, VaultHistory::From(PRUNE as u32 + 500)),
            Ok(())
        );
    }

    #[test]
    fn pruned_node_refuses_coins_below_its_prune_height() {
        // The 2026-10-06 incident: coins at 958601 and 960326, prune height 969938.
        let history = VaultHistory::from_coin_heights([Some(960_326), Some(958_601)], false);
        assert_eq!(history, VaultHistory::From(958_601));
        assert_eq!(
            pruned_node_serves(PRUNED, history),
            Err(PrunedHistory::CoinsBelowPrune {
                prune_height: PRUNE,
                earliest: 958_601,
            })
        );
        // One block short is still short.
        assert!(pruned_node_serves(PRUNED, VaultHistory::From(PRUNE as u32 - 1)).is_err());
    }

    #[test]
    fn spent_only_history_still_counts() {
        // Every coin spent: the heights still come from the daemon's full
        // coin list, so the history is known and dated.
        let history = VaultHistory::from_coin_heights([Some(958_601), None], true);
        assert_eq!(history, VaultHistory::From(958_601));
        assert!(matches!(
            pruned_node_serves(PRUNED, history),
            Err(PrunedHistory::CoinsBelowPrune { .. })
        ));
        // ... and spent coins above the prune height are fine.
        let history = VaultHistory::from_coin_heights([Some(970_000)], false);
        assert_eq!(pruned_node_serves(PRUNED, history), Ok(()));
    }

    #[test]
    fn no_coins_with_a_pending_rescan_is_unknown_history() {
        // Unconfirmed coins have no height and date nothing.
        let history = VaultHistory::from_coin_heights([None], true);
        assert_eq!(
            history,
            VaultHistory::NoConfirmedCoins { rescan_owed: true }
        );
        assert_eq!(
            pruned_node_serves(PRUNED, history),
            Err(PrunedHistory::HistoryUnknown {
                prune_height: PRUNE
            })
        );
    }

    #[test]
    fn no_coins_and_no_obligation_is_a_new_vault() {
        let history = VaultHistory::from_coin_heights(std::iter::empty(), false);
        assert_eq!(
            history,
            VaultHistory::NoConfirmedCoins { rescan_owed: false }
        );
        assert_eq!(pruned_node_serves(PRUNED, history), Ok(()));
    }

    #[test]
    fn pruning_is_read_from_blockchain_info() {
        let read = |v: serde_json::Value| NodePruning::from_blockchain_info(&v);
        assert_eq!(
            read(serde_json::json!({"pruned": false})),
            Some(NodePruning::Unpruned)
        );
        assert_eq!(
            read(serde_json::json!({"pruned": true, "pruneheight": PRUNE})),
            Some(PRUNED)
        );
        // Not saying is not "unpruned".
        assert_eq!(read(serde_json::json!({"pruned": true})), None);
        assert_eq!(read(serde_json::json!({})), None);
    }

    #[test]
    fn running_scan_reports_a_rescan_first() {
        assert_eq!(running_scan(false, false), None);
        assert_eq!(running_scan(true, false), Some(RunningScan::WalletSync));
        assert_eq!(running_scan(false, true), Some(RunningScan::Rescan));
        assert_eq!(running_scan(true, true), Some(RunningScan::Rescan));
    }

    #[test]
    fn auto_switch_waits_for_a_running_scan() {
        // Even on an unpruned node: switching would discard the scan.
        assert_eq!(
            auto_switch_hold(
                Some(RunningScan::WalletSync),
                Some(NodePruning::Unpruned),
                None
            ),
            Some(SwitchHold::Scanning(RunningScan::WalletSync))
        );
        assert_eq!(
            auto_switch_hold(Some(RunningScan::Rescan), Some(PRUNED), None),
            Some(SwitchHold::Scanning(RunningScan::Rescan))
        );
        // Once the scan is over, an unpruned node is switched to.
        assert_eq!(
            auto_switch_hold(None, Some(NodePruning::Unpruned), None),
            None
        );
    }

    #[test]
    fn auto_switch_needs_pruning_and_history_known() {
        assert_eq!(
            auto_switch_hold(None, None, None),
            Some(SwitchHold::Unknown)
        );
        assert_eq!(
            auto_switch_hold(None, Some(PRUNED), None),
            Some(SwitchHold::Unknown)
        );
    }

    #[test]
    fn auto_switch_refuses_a_node_pruned_past_the_vault() {
        assert_eq!(
            auto_switch_hold(None, Some(PRUNED), Some(VaultHistory::From(958_601))),
            Some(SwitchHold::Pruned(PrunedHistory::CoinsBelowPrune {
                prune_height: PRUNE,
                earliest: 958_601,
            }))
        );
        assert_eq!(
            auto_switch_hold(None, Some(PRUNED), Some(VaultHistory::From(970_000))),
            None
        );
    }

    #[test]
    fn refusal_copy_names_both_heights() {
        let copy = PrunedHistory::CoinsBelowPrune {
            prune_height: PRUNE,
            earliest: 958_601,
        }
        .to_string();
        assert!(copy.contains("969938"), "{}", copy);
        assert!(copy.contains("958601"), "{}", copy);
    }
}
