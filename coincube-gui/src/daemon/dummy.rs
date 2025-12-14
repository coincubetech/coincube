use crate::daemon::DaemonError;
use coincube_core::miniscript::bitcoin::{
    address, bip32::ChildNumber, psbt::Psbt, Address, Network, OutPoint, Txid,
};
use coincubed::bip329::Labels;
use coincubed::commands::{CoinStatus, LabelItem, UpdateDerivIndexesResult};
use coincubed::config::Config;
use std::collections::{HashMap, HashSet};

// Ensure this struct is visible where you need it
#[derive(Debug, Clone)]
pub struct DummyDaemon;

#[async_trait::async_trait]
impl super::Daemon for DummyDaemon {
    /// Reports which daemon backend this dummy implementation represents.
    ///
    /// # Returns
    ///
    /// The `RemoteBackend` variant of `DaemonBackend`, indicating a remote/external backend.
    ///
    /// # Examples
    ///
    /// ```
    /// let d = DummyDaemon;
    /// assert_eq!(d.backend(), super::DaemonBackend::RemoteBackend);
    /// ```
    fn backend(&self) -> super::DaemonBackend {
        // Return a variant that makes sense for a dummy, usually Remote or External
        super::DaemonBackend::RemoteBackend
    }

    /// Access the daemon's configuration if available.
    ///
    /// The dummy implementation does not provide a configuration and always returns `None`.
    ///
    /// # Returns
    ///
    /// `Some(&Config)` if the daemon exposes a configuration, `None` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// let d = DummyDaemon;
    /// assert!(d.config().is_none());
    /// ```
    fn config(&self) -> Option<&Config> {
        None
    }

    /// Checks whether the daemon is reachable; in this dummy implementation this always succeeds.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the daemon is considered alive, `Err(DaemonError)` on failure.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use coincubed::daemon::DummyDaemon;
    /// # use coincube_core::Network;
    /// # // construct a CoincubeDirectory as appropriate for your environment
    /// # let datadir = /* CoincubeDirectory */ unimplemented!();
    /// let daemon = DummyDaemon;
    /// // call from an async context
    /// futures::executor::block_on(async {
    ///     daemon.is_alive(&datadir, Network::Regtest).await.unwrap();
    /// });
    /// ```
    async fn is_alive(
        &self,
        _datadir: &crate::dir::CoincubeDirectory,
        _network: Network,
    ) -> Result<(), DaemonError> {
        // You might want this to return Ok(()) to simulate a running daemon
        Ok(())
    }

    /// Stops the daemon.
    ///
    /// This dummy implementation performs no action and always succeeds.
    ///
    /// # Examples
    ///
    /// ```
    /// use futures::executor::block_on;
    /// use coincube_gui::daemon::dummy::DummyDaemon;
    ///
    /// let d = DummyDaemon;
    /// block_on(d.stop()).unwrap();
    /// ```
    async fn stop(&self) -> Result<(), DaemonError> {
        Ok(())
    }

    /// Fetches general information about the daemon and network state.
    ///
    /// # Returns
    ///
    /// `GetInfoResult` containing daemon and network information on success, or a `DaemonError` on failure.
    ///
    /// # Examples
    ///
    /// ```
    /// # use coincube_gui::daemon::dummy::DummyDaemon;
    /// # use coincube_gui::daemon::DaemonError;
    /// #[tokio::test]
    /// async fn example_get_info() {
    ///     let d = DummyDaemon;
    ///     let res = d.get_info().await;
    ///     assert!(matches!(res, Err(DaemonError::NotImplemented)));
    /// }
    /// ```
    async fn get_info(&self) -> Result<super::model::GetInfoResult, DaemonError> {
        Err(DaemonError::NotImplemented)
    }

    /// Request a fresh receiving address from the daemon.
    ///
    /// The returned address is managed by the daemon and intended for receiving incoming funds.
    ///
    /// # Returns
    ///
    /// `Ok(GetAddressResult)` containing the newly created address details on success, `Err(DaemonError)` on failure.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(daemon: &impl super::Daemon) {
    /// let addr = daemon.get_new_address().await.unwrap();
    /// // use `addr` (a `super::model::GetAddressResult`) to display or persist the address
    /// # }
    /// ```
    async fn get_new_address(&self) -> Result<super::model::GetAddressResult, DaemonError> {
        Err(DaemonError::NotImplemented)
    }

    /// Lists revealed addresses from the daemon using the provided filters.
    ///
    /// - `is_change`: if `true`, list change addresses; otherwise list receiving addresses.
    /// - `exclude_used`: if `true`, omit addresses already marked as used.
    /// - `limit`: maximum number of addresses to return.
    /// - `start_index`: optional derivation index to start listing from.
    ///
    /// # Returns
    ///
    /// `Ok(ListRevealedAddressesResult)` with the matching revealed addresses on success, `Err(DaemonError)` on failure.
    ///
    /// # Examples
    ///
    /// ```
    /// let daemon = DummyDaemon;
    /// let _ = futures::executor::block_on(daemon.list_revealed_addresses(false, false, 10, None));
    /// ```
    async fn list_revealed_addresses(
        &self,
        _is_change: bool,
        _exclude_used: bool,
        _limit: usize,
        _start_index: Option<ChildNumber>,
    ) -> Result<super::model::ListRevealedAddressesResult, DaemonError> {
        Err(DaemonError::NotImplemented)
    }

    /// Updates the wallet derivation indexes for the receive and change key chains.
    ///
    /// The provided `receive` and `change` values, when present, should replace the current
    /// derivation indexes for their respective chains and return the resulting indexes.
    ///
    /// # Returns
    ///
    /// `Ok(UpdateDerivIndexesResult)` with the updated indexes on success, `Err(DaemonError::NotImplemented)` for this dummy implementation.
    ///
    /// # Examples
    ///
    /// ```
    /// # use crate::daemon::{DaemonError};
    /// # use coincube_gui::daemon::dummy::DummyDaemon;
    /// #[tokio::test]
    /// async fn example_update_deriv_indexes() {
    ///     let d = DummyDaemon;
    ///     let res = d.update_deriv_indexes(Some(0), Some(0)).await;
    ///     assert!(matches!(res, Err(DaemonError::NotImplemented)));
    /// }
    /// ```
    async fn update_deriv_indexes(
        &self,
        _receive: Option<u32>,
        _change: Option<u32>,
    ) -> Result<UpdateDerivIndexesResult, DaemonError> {
        Err(DaemonError::NotImplemented)
    }

    /// Lists wallet coins filtered by coin statuses and/or specific outpoints.
    ///
    /// `statuses` narrows results to coins whose status matches any provided value.
    /// `outpoints` restricts results to the specified transaction outpoints; when empty, no outpoint filtering is applied.
    ///
    /// # Returns
    ///
    /// A `ListCoinsResult` containing the matching coins.
    ///
    /// # Examples
    ///
    /// ```
    /// # use coincube_core::model::CoinStatus;
    /// # use coincubed::types::OutPoint;
    /// # use futures::executor::block_on;
    /// let daemon = crate::daemon::dummy::DummyDaemon;
    /// let res = block_on(daemon.list_coins(&[CoinStatus::Confirmed], &[]));
    /// // In a real daemon, `res` would contain matched coins or an error.
    /// ```
    async fn list_coins(
        &self,
        _statuses: &[CoinStatus],
        _outpoints: &[OutPoint],
    ) -> Result<super::model::ListCoinsResult, DaemonError> {
        Err(DaemonError::NotImplemented)
    }

    /// Lists spending transactions known to the daemon.
    ///
    ///
    /// # Returns
    /// A `ListSpendResult` containing the daemon's known spend transactions and any associated metadata; on this dummy implementation, the call is unimplemented and will return a `DaemonError::NotImplemented`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use coincube_gui::daemon::DummyDaemon;
    /// # async fn _example() {
    /// let daemon = DummyDaemon{};
    /// let result = futures::executor::block_on(daemon.list_spend_txs());
    /// match result {
    ///     Ok(list) => { /* handle list */ }
    ///     Err(err) => { /* handle error */ }
    /// }
    /// # }
    /// ```
    async fn list_spend_txs(&self) -> Result<super::model::ListSpendResult, DaemonError> {
        Err(DaemonError::NotImplemented)
    }

    /// Builds a spend transaction that consumes the given outpoints and pays the specified destination amounts,
    /// optionally using the provided feerate (satoshis per vbyte) and change address.
    ///
    /// The `destinations` map keys are destination addresses and values are amounts in satoshis.
    ///
    /// # Returns
    ///
    /// `CreateSpendResult` containing the created PSBT and metadata on success, or a `DaemonError` on failure.
    ///
    /// # Examples
    ///
    /// ```
    /// # use coincube_gui::daemon::DummyDaemon;
    /// # use coincube_core::types::{OutPoint, Address};
    /// # use std::collections::HashMap;
    /// # async fn example() {
    /// let daemon = DummyDaemon;
    /// let coins: Vec<OutPoint> = Vec::new();
    /// let destinations: HashMap<Address<_>, u64> = HashMap::new();
    /// let _ = daemon.create_spend_tx(&coins, &destinations, 1, None).await;
    /// # }
    /// ```
    async fn create_spend_tx(
        &self,
        _coins_outpoints: &[OutPoint],
        _destinations: &HashMap<Address<address::NetworkUnchecked>, u64>,
        _feerate_vb: u64,
        _change_address: Option<Address<address::NetworkUnchecked>>,
    ) -> Result<super::model::CreateSpendResult, DaemonError> {
        Err(DaemonError::NotImplemented)
    }

    /// Creates a replacement or cancellation PSBT for a given transaction.
    ///
    /// Produces a PSBT intended either to replace the transaction identified by `txid` (fee-bump)
    /// or to cancel it, according to `is_cancel`. When supplied, `feerate_vb` specifies the
    /// target fee rate in satoshis per vbyte.
    ///
    /// # Parameters
    ///
    /// - `txid` — Transaction id to replace or cancel.
    /// - `is_cancel` — If `true`, create a cancellation replacement; if `false`, create a fee-bumping replacement.
    /// - `feerate_vb` — Optional target fee rate in sat/vB.
    ///
    /// # Returns
    ///
    /// `CreateSpendResult` on success, `DaemonError` on failure.
    ///
    /// # Examples
    ///
    /// ```
    /// # use coincubed::daemon::DummyDaemon;
    /// # use coincube_core::Txid;
    /// # tokio_test::block_on(async {
    /// let daemon = DummyDaemon;
    /// let txid = Txid::all_zeros(); // placeholder Txid for example
    /// let res = daemon.rbf_psbt(&txid, false, Some(1)).await;
    /// assert!(res.is_err());
    /// # });
    /// ```
    async fn rbf_psbt(
        &self,
        _txid: &Txid,
        _is_cancel: bool,
        _feerate_vb: Option<u64>,
    ) -> Result<super::model::CreateSpendResult, DaemonError> {
        Err(DaemonError::NotImplemented)
    }

    /// Updates the given partially signed Bitcoin transaction (PSBT) in the daemon's spend-tx store.
    ///
    /// # Parameters
    ///
    /// - `psbt`: the PSBT to update; implementations should replace or merge any existing spend transaction data for the same txid.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the daemon accepted and stored the PSBT, `Err(DaemonError::NotImplemented)` if the operation is not available or another `DaemonError` occurs.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use coincubed::Psbt;
    /// let daemon = DummyDaemon;
    /// let psbt = Psbt::default();
    /// // Update the spend transaction (may return NotImplemented for the dummy daemon)
    /// let _ = futures::executor::block_on(daemon.update_spend_tx(&psbt));
    /// ```
    async fn update_spend_tx(&self, _psbt: &Psbt) -> Result<(), DaemonError> {
        Err(DaemonError::NotImplemented)
    }

    /// Deletes a pending spend transaction identified by `txid`.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the transaction was deleted successfully, `Err(DaemonError::NotImplemented)` if the operation is not supported by this daemon implementation.
    ///
    /// # Examples
    ///
    /// ```
    /// // Example usage (async context)
    /// # use coincubed::Txid;
    /// # async fn example(daemon: &impl super::Daemon, txid: Txid) {
    /// let _ = daemon.delete_spend_tx(&txid).await;
    /// # }
    /// ```
    async fn delete_spend_tx(&self, _txid: &Txid) -> Result<(), DaemonError> {
        Err(DaemonError::NotImplemented)
    }

    /// Broadcasts a previously prepared spend transaction to the network.
    ///
    /// This dummy implementation does not perform network operations and always returns
    /// `DaemonError::NotImplemented`.
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, `Err(DaemonError::NotImplemented)` in this implementation.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use coincube_core::Txid;
    /// # use coincubed::daemon::DummyDaemon;
    /// # async {
    /// let dummy = DummyDaemon;
    /// let txid = Txid::default();
    /// let _ = dummy.broadcast_spend_tx(&txid).await;
    /// # };
    /// ```
    async fn broadcast_spend_tx(&self, _txid: &Txid) -> Result<(), DaemonError> {
        Err(DaemonError::NotImplemented)
    }

    /// Starts a rescan of the daemon's data from the specified block height.
    ///
    /// `t` is the block height from which to begin rescanning.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the rescan was started successfully, `Err(DaemonError::NotImplemented)` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// # use futures::executor::block_on;
    /// # use coincubed::daemon::DummyDaemon; // adjust path as needed in real code
    /// let d = DummyDaemon;
    /// let result = block_on(d.start_rescan(0));
    /// assert!(result.is_err());
    /// ```
    async fn start_rescan(&self, _t: u32) -> Result<(), DaemonError> {
        Err(DaemonError::NotImplemented)
    }

    /// List confirmed transactions whose confirmation heights lie between `start` and `end`, limited to `limit`.
    ///
    /// # Returns
    ///
    /// A `Result` containing a `super::model::ListTransactionsResult` with the transactions that match the requested range and limit, or a `DaemonError` on failure.
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::daemon::dummy::DummyDaemon;
    /// let d = DummyDaemon;
    /// let res = futures::executor::block_on(d.list_confirmed_txs(0, 100, 10));
    /// assert!(res.is_err()); // dummy implementation returns NotImplemented
    /// ```
    async fn list_confirmed_txs(
        &self,
        _start: u32,
        _end: u32,
        _limit: u64,
    ) -> Result<super::model::ListTransactionsResult, DaemonError> {
        Err(DaemonError::NotImplemented)
    }

    /// Creates a recovery PSBT that spends the provided outpoints to the given external address.
    ///
    /// This dummy implementation does not perform any recovery and always returns
    /// `Err(DaemonError::NotImplemented)`.
    ///
    /// # Returns
    ///
    /// `Ok(Psbt)` on success; for this implementation, always `Err(DaemonError::NotImplemented)`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use coincube_gui::daemon::dummy::DummyDaemon;
    /// # use coincubed::daemon::DaemonError;
    /// # use futures::executor;
    /// let daemon = DummyDaemon;
    /// let result = executor::block_on(daemon.create_recovery(
    ///     /* address */ unimplemented!(),
    ///     /* coins_outpoints */ &[],
    ///     /* feerate_vb */ 1,
    ///     /* sequence */ None,
    /// ));
    /// assert!(matches!(result, Err(DaemonError::NotImplemented)));
    /// ```
    async fn create_recovery(
        &self,
        _address: Address<address::NetworkUnchecked>,
        _coins_outpoints: &[OutPoint],
        _feerate_vb: u64,
        _sequence: Option<u16>,
    ) -> Result<Psbt, DaemonError> {
        Err(DaemonError::NotImplemented)
    }

    /// Fetches transaction details for the given transaction IDs.
    ///
    /// Returns a structure containing the matching transactions and pagination metadata on success,
    /// or a `DaemonError` on failure.
    ///
    /// # Examples
    ///
    /// ```
    /// use futures::executor::block_on;
    /// use crate::daemon::dummy::DummyDaemon;
    ///
    /// let daemon = DummyDaemon;
    /// let result = block_on(daemon.list_txs(&[]));
    /// assert!(result.is_err());
    /// ```
    async fn list_txs(
        &self,
        _txid: &[Txid],
    ) -> Result<super::model::ListTransactionsResult, DaemonError> {
        Err(DaemonError::NotImplemented)
    }

    /// Returns an empty mapping of label identifiers to label strings.
    ///
    /// Ignores the provided set of `LabelItem`s and always yields an empty `HashMap`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::HashSet;
    /// use futures::executor::block_on;
    /// let daemon = crate::daemon::dummy::DummyDaemon {};
    /// let labels: HashSet<crate::coincubed::LabelItem> = HashSet::new();
    /// let map = block_on(daemon.get_labels(&labels)).unwrap();
    /// assert!(map.is_empty());
    /// ```
    async fn get_labels(
        &self,
        _labels: &HashSet<LabelItem>,
    ) -> Result<HashMap<String, String>, DaemonError> {
        // Return empty map to prevent iteration errors if called
        Ok(HashMap::new())
    }

    /// Accepts a set of label updates and does nothing.
    ///
    /// This dummy implementation ignores the provided label map and always succeeds.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::collections::HashMap;
    /// # use coincube_core::labels::LabelItem;
    /// # use coincubed::daemon::dummy::DummyDaemon;
    /// # futures::executor::block_on(async {
    /// let daemon = DummyDaemon {};
    /// let labels: HashMap<LabelItem, Option<String>> = HashMap::new();
    /// daemon.update_labels(&labels).await.unwrap();
    /// # });
    /// ```
    async fn update_labels(
        &self,
        _labels: &HashMap<LabelItem, Option<String>>,
    ) -> Result<(), DaemonError> {
        Ok(())
    }

    /// Retrieves labels using BIP-329 pagination.
    ///
    /// # Returns
    ///
    /// `Labels` mapped for the requested page when available, or an error describing why labels could not be fetched.
    ///
    /// # Examples
    ///
    /// ```
    /// // Example usage (async context)
    /// // let daemon = DummyDaemon;
    /// // let res = daemon.get_labels_bip329(0, 100).await;
    /// // assert!(matches!(res, Err(crate::daemon::DaemonError::NotImplemented)));
    /// ```
    async fn get_labels_bip329(&self, _offset: u32, _limit: u32) -> Result<Labels, DaemonError> {
        Err(DaemonError::NotImplemented)
    }
}