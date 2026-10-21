//! Disposable SQLite wallet for desktop MCP tests; never built into production.
use crate::{
    config::{BitcoinConfig, Config},
    database::{
        sqlite::{FreshDbOptions, SqliteDb},
        Coin,
    },
    datadir::DataDirectory,
    testutils::DummyBitcoind,
    DaemonHandle,
};
use miniscript::bitcoin::{self, bip32::ChildNumber, hashes::Hash, secp256k1::Secp256k1};
use std::time::Duration;
pub fn wallet(
    root: &std::path::Path,
    descriptor: coincube_core::descriptors::CoincubeDescriptor,
) -> (Config, DaemonHandle) {
    let secp = Secp256k1::verification_only();
    let database = SqliteDb::new(
        root.join("wallet.sqlite3"),
        Some(FreshDbOptions::new(
            bitcoin::Network::Bitcoin.into(),
            descriptor.clone(),
        )),
        &secp,
    )
    .unwrap();
    let mut connection = database.connection().unwrap();
    let script = descriptor
        .receive_descriptor()
        .derive(0.into(), &secp)
        .script_pubkey();
    let transaction = bitcoin::Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![bitcoin::TxIn {
            previous_output: bitcoin::OutPoint {
                txid: bitcoin::Txid::all_zeros(),
                vout: 1,
            },
            ..Default::default()
        }],
        output: vec![bitcoin::TxOut {
            value: bitcoin::Amount::from_sat(55_000),
            script_pubkey: script,
        }],
    };
    let outpoint = bitcoin::OutPoint {
        txid: transaction.compute_txid(),
        vout: 0,
    };
    connection.new_txs(&[transaction]);
    connection.new_unspent_coins(&[Coin {
        outpoint,
        is_immature: false,
        block_info: None,
        amount: bitcoin::Amount::from_sat(55_000),
        derivation_index: ChildNumber::from(0),
        is_change: false,
        spend_txid: None,
        spend_block: None,
        is_from_self: false,
    }]);
    connection.confirm_coins(&[(outpoint, 1, 1)]);
    connection.update_tip(&DummyBitcoind::new().tip);
    drop(connection);
    let config = Config::new(
        BitcoinConfig::new(bitcoin::Network::Bitcoin.into(), Duration::from_secs(3600)),
        None,
        log::LevelFilter::Warn,
        descriptor.clone(),
        DataDirectory::new(root.join("daemon")),
    );
    let handle = DaemonHandle::start(
        config.clone(),
        Some(DummyBitcoind::new()),
        Some(database),
        false,
    )
    .unwrap();
    (config, handle)
}
