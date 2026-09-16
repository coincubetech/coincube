use bip329::Label;
use coincube_core::chain::ChainId;
use coincube_core::descriptors::CoincubeDescriptor;

use std::{convert::TryFrom, str::FromStr};

use miniscript::bitcoin::{
    self,
    address::{self, NetworkUnchecked},
    bip32,
    consensus::encode,
    psbt::Psbt,
    Address, OutPoint, Txid,
};

// Due to limitations of Sqlite's ALTER TABLE command and in order not to recreate
// tables during migration:
// - Columns `num_inputs` and `num_outputs` of the transactions table remain nullable.
// - There is no CHECK constraint to prevent both `is_immature` and `is_from_self`
//   being true in the coins table.
pub const SCHEMA: &str = "\
CREATE TABLE version (
    version INTEGER NOT NULL
);

/* About the chain we follow. 'network' is the *encoding* (address bytes, HRP, descriptor and
 * PSBT rules) as bitcoin::Network spells it; 'chain' is the *identity* as ChainId spells it
 * (its directory string). For the Bitcoin family the two are the same string; a Bitcoin Blake2b
 * database carries 'bitcoin'/'bitcoin-blake2b', so it can never be mistaken for a mainnet one.
 * Rebuilt (not ALTERed) by the v8 -> v9 migration so both columns are genuinely NOT NULL and the
 * column order is identical between fresh and migrated databases. */
CREATE TABLE tip (
    network TEXT NOT NULL,
    chain TEXT NOT NULL,
    blockheight INTEGER,
    blockhash BLOB
);

/* This stores metadata about our wallet. We only support single wallet for
 * now (and the foreseeable future).
 *
 * The 'timestamp' field is the creation date of the wallet. We guarantee to have seen all
 * information related to our descriptor(s) that occurred after this date.
 * The optional 'rescan_timestamp' field is a the timestamp we need to rescan the chain
 * for events related to our descriptor(s) from.
 */
CREATE TABLE wallets (
    id INTEGER PRIMARY KEY NOT NULL,
    timestamp INTEGER NOT NULL,
    main_descriptor TEXT NOT NULL,
    deposit_derivation_index INTEGER NOT NULL,
    change_derivation_index INTEGER NOT NULL,
    rescan_timestamp INTEGER,
    last_poll_timestamp INTEGER
);

/* Our (U)TxOs.
 *
 * The 'spend_block_height' and 'spend_block.time' are only present if the spending
 * transaction for this coin exists and was confirmed.
 *
 * The 'is_immature' field is for coinbase deposits that are not yet buried under 100
 * blocks. Note coinbase deposits can't technically be unconfirmed but we keep them
 * as such until they become mature.
 *
 * The `is_from_self` field indicates if the coin is the output of a transaction whose
 * inputs are all from the same wallet as the coin. For an unconfirmed coin, this also
 * means that all unconfirmed ancestors, if any, are from self.
 */
CREATE TABLE coins (
    id INTEGER PRIMARY KEY NOT NULL,
    wallet_id INTEGER NOT NULL,
    blockheight INTEGER,
    blocktime INTEGER,
    txid BLOB NOT NULL,
    vout INTEGER NOT NULL,
    amount_sat INTEGER NOT NULL,
    derivation_index INTEGER NOT NULL,
    is_change BOOLEAN NOT NULL CHECK (is_change IN (0,1)),
    spend_txid BLOB,
    spend_block_height INTEGER,
    spend_block_time INTEGER,
    is_immature BOOLEAN NOT NULL CHECK (is_immature IN (0,1)),
    is_from_self BOOLEAN NOT NULL DEFAULT 0 CHECK (is_from_self IN (0,1)),
    UNIQUE (txid, vout),
    FOREIGN KEY (wallet_id) REFERENCES wallets (id)
        ON UPDATE RESTRICT
        ON DELETE RESTRICT,
    FOREIGN KEY (txid) REFERENCES transactions (txid)
        ON UPDATE RESTRICT
        ON DELETE RESTRICT,
    FOREIGN KEY (spend_txid) REFERENCES transactions (txid)
        ON UPDATE RESTRICT
        ON DELETE RESTRICT
);

/* A mapping from descriptor address to derivation index. Necessary until
 * we can get the derivation index from the parent descriptor from bitcoind.
 */
CREATE TABLE addresses (
    receive_address TEXT NOT NULL UNIQUE,
    change_address TEXT NOT NULL UNIQUE,
    derivation_index INTEGER NOT NULL UNIQUE
);

/* Transactions for all wallets. */
CREATE TABLE transactions (
    id INTEGER PRIMARY KEY NOT NULL,
    txid BLOB UNIQUE NOT NULL,
    tx BLOB UNIQUE NOT NULL,
    num_inputs INTEGER CHECK (num_inputs IS NULL OR num_inputs > 0),
    num_outputs INTEGER CHECK (num_outputs IS NULL OR num_outputs > 0),
    is_coinbase BOOLEAN NOT NULL DEFAULT 0 CHECK (is_coinbase IN (0,1))
);

/* Transactions we created that spend some of our coins. */
CREATE TABLE spend_transactions (
    id INTEGER PRIMARY KEY NOT NULL,
    psbt BLOB UNIQUE NOT NULL,
    txid BLOB UNIQUE NOT NULL,
    updated_at INTEGER
);

/* Labels applied on addresses (0), outpoints (1), txids (2) */
CREATE TABLE labels (
    id INTEGER PRIMARY KEY NOT NULL,
    wallet_id INTEGER NOT NULL,
    item_kind INTEGER NOT NULL CHECK (item_kind IN (0,1,2)),
    item TEXT UNIQUE NOT NULL,
    value TEXT NOT NULL
);
";

/// A row in the "tip" table (database version 9 layout: `network, chain, blockheight,
/// blockhash`). Only ever decoded from a database whose version the startup preflight and
/// `sanity_check` have already established; the version-aware preflight in
/// [`super::preflight`] reads older layouts with explicit columns instead of this decoder.
#[derive(Clone, Debug)]
pub struct DbTip {
    /// The encoding, `chain.bitcoin_network()`.
    pub network: bitcoin::Network,
    /// The identity.
    pub chain: ChainId,
    pub block_height: Option<i32>,
    pub block_hash: Option<bitcoin::BlockHash>,
}

/// A stored value this decoder cannot make sense of, surfaced as a typed conversion failure at
/// the column it came from rather than a panic.
fn insane_column(index: usize, what: &str, value: impl std::fmt::Debug) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        format!("Insane database: can't parse {} {:?}", what, value).into(),
    )
}

impl TryFrom<&rusqlite::Row<'_>> for DbTip {
    type Error = rusqlite::Error;

    fn try_from(row: &rusqlite::Row) -> Result<Self, Self::Error> {
        let network: String = row.get(0)?;
        let network = bitcoin::Network::from_str(&network)
            .map_err(|_| insane_column(0, "network string", &network))?;

        let chain: String = row.get(1)?;
        let chain = ChainId::from_dir_name(&chain)
            .ok_or_else(|| insane_column(1, "chain string", &chain))?;
        if chain.bitcoin_network() != network {
            return Err(insane_column(
                1,
                "chain/network pair",
                (chain.dir_name(), network.to_string()),
            ));
        }

        let block_height: Option<i32> = row.get(2)?;
        let block_hash: Option<Vec<u8>> = row.get(3)?;
        let block_hash: Option<bitcoin::BlockHash> = block_hash
            .map(|h| encode::deserialize(&h).map_err(|_| insane_column(3, "block hash", &h)))
            .transpose()?;

        Ok(DbTip {
            network,
            chain,
            block_height,
            block_hash,
        })
    }
}

/// A row in the "wallets" table.
#[derive(Clone, Debug)]
pub struct DbWallet {
    #[allow(dead_code)]
    pub id: i64,
    pub timestamp: u32,
    pub main_descriptor: CoincubeDescriptor,
    pub deposit_derivation_index: bip32::ChildNumber,
    pub change_derivation_index: bip32::ChildNumber,
    pub rescan_timestamp: Option<u32>,
    pub last_poll_timestamp: Option<u32>,
}

impl TryFrom<&rusqlite::Row<'_>> for DbWallet {
    type Error = rusqlite::Error;

    fn try_from(row: &rusqlite::Row) -> Result<Self, Self::Error> {
        let id = row.get(0)?;
        let timestamp = row.get(1)?;

        let desc_str: String = row.get(2)?;
        let main_descriptor = CoincubeDescriptor::from_str(&desc_str)
            .expect("Insane database: can't parse deposit descriptor");

        let der_idx: u32 = row.get(3)?;
        let deposit_derivation_index = bip32::ChildNumber::from(der_idx);
        let der_idx: u32 = row.get(4)?;
        let change_derivation_index = bip32::ChildNumber::from(der_idx);

        let rescan_timestamp = row.get(5)?;
        let last_poll_timestamp = row.get(6)?;

        Ok(DbWallet {
            id,
            timestamp,
            main_descriptor,
            deposit_derivation_index,
            change_derivation_index,
            rescan_timestamp,
            last_poll_timestamp,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DbBlockInfo {
    pub height: i32,
    pub time: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DbCoin {
    pub id: i64,
    pub wallet_id: i64,
    /// Whether this coin was created by a yet-to-be-mature coinbase transaction.
    pub is_immature: bool,
    pub outpoint: bitcoin::OutPoint,
    pub block_info: Option<DbBlockInfo>,
    pub amount: bitcoin::Amount,
    pub derivation_index: bip32::ChildNumber,
    pub is_change: bool,
    pub spend_txid: Option<bitcoin::Txid>,
    pub spend_block: Option<DbBlockInfo>,
    /// A coin is from self if it is the output of a transaction whose
    /// inputs are all from this wallet. For unconfirmed coins, we
    /// further require that all unconfirmed ancestors, if any, also
    /// be from self, as otherwise they will depend on an unconfirmed
    /// external transaction.
    pub is_from_self: bool,
}

impl TryFrom<&rusqlite::Row<'_>> for DbCoin {
    type Error = rusqlite::Error;

    fn try_from(row: &rusqlite::Row) -> Result<Self, Self::Error> {
        let id = row.get(0)?;
        let wallet_id = row.get(1)?;

        let block_height: Option<i32> = row.get(2)?;
        let block_time: Option<u32> = row.get(3)?;
        assert_eq!(block_height.is_none(), block_time.is_none());
        let block_info = block_height.map(|height| DbBlockInfo {
            height,
            time: block_time.expect("Must be there if height is"),
        });
        let txid: Vec<u8> = row.get(4)?;
        let txid: bitcoin::Txid = encode::deserialize(&txid).expect("We only store valid txids");
        let vout = row.get(5)?;
        let outpoint = bitcoin::OutPoint { txid, vout };

        let amount: i64 = row.get(6)?;
        let amount_u64 = u64::try_from(amount).expect("Insane database: negative amount_sat");
        let amount = bitcoin::Amount::from_sat(amount_u64);
        let der_idx: u32 = row.get(7)?;
        let derivation_index = bip32::ChildNumber::from(der_idx);
        let is_change: bool = row.get(8)?;

        let spend_txid: Option<Vec<u8>> = row.get(9)?;
        let spend_txid =
            spend_txid.map(|txid| encode::deserialize(&txid).expect("We only store valid txids"));
        let spend_height: Option<i32> = row.get(10)?;
        let spend_time: Option<u32> = row.get(11)?;
        assert_eq!(spend_height.is_none(), spend_time.is_none());
        let spend_block = spend_height.map(|height| DbBlockInfo {
            height,
            time: spend_time.expect("Must be there if height is"),
        });

        let is_immature: bool = row.get(12)?;
        let is_from_self: bool = row.get(13)?;

        Ok(DbCoin {
            id,
            wallet_id,
            is_immature,
            outpoint,
            block_info,
            amount,
            derivation_index,
            is_change,
            spend_txid,
            spend_block,
            is_from_self,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbAddress {
    pub receive_address: bitcoin::Address<address::NetworkUnchecked>,
    pub change_address: bitcoin::Address<address::NetworkUnchecked>,
    pub derivation_index: bip32::ChildNumber,
}

impl TryFrom<&rusqlite::Row<'_>> for DbAddress {
    type Error = rusqlite::Error;

    fn try_from(row: &rusqlite::Row) -> Result<Self, Self::Error> {
        let receive_address: String = row.get(0)?;
        let receive_address =
            bitcoin::Address::from_str(&receive_address).expect("We only store valid addresses");

        let change_address: String = row.get(1)?;
        let change_address =
            bitcoin::Address::from_str(&change_address).expect("We only store valid addresses");

        let derivation_index: u32 = row.get(2)?;
        let derivation_index = bip32::ChildNumber::from(derivation_index);
        assert!(derivation_index.is_normal());

        Ok(DbAddress {
            receive_address,
            change_address,
            derivation_index,
        })
    }
}

/// A row in the "spend_transactions" table
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbSpendTransaction {
    pub id: i64,
    pub psbt: Psbt,
    pub txid: bitcoin::Txid,
    pub updated_at: Option<u32>,
}

impl TryFrom<&rusqlite::Row<'_>> for DbSpendTransaction {
    type Error = rusqlite::Error;

    fn try_from(row: &rusqlite::Row) -> Result<Self, Self::Error> {
        let id: i64 = row.get(0)?;

        let psbt: Vec<u8> = row.get(1)?;
        let psbt = Psbt::deserialize(&psbt).expect("We only store valid PSBTs");

        let txid: Vec<u8> = row.get(2)?;
        let txid: bitcoin::Txid = encode::deserialize(&txid).expect("We only store valid txids");
        assert_eq!(txid, psbt.unsigned_tx.compute_txid());

        let updated_at = row.get(3)?;

        Ok(DbSpendTransaction {
            id,
            psbt,
            txid,
            updated_at,
        })
    }
}

/// A row in the "labels" table
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbLabel {
    pub id: i64,
    pub wallet_id: i64,
    pub item_kind: DbLabelledKind,
    pub item: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[repr(i64)]
pub enum DbLabelledKind {
    Address = 0,
    OutPoint = 1,
    Txid = 2,
}

impl From<i64> for DbLabelledKind {
    fn from(value: i64) -> Self {
        if value == 0 {
            Self::Address
        } else if value == 1 {
            Self::OutPoint
        } else {
            assert_eq!(value, 2);
            Self::Txid
        }
    }
}

impl From<DbLabel> for Label {
    fn from(value: DbLabel) -> Self {
        let mut ref_ = value.item;
        if value.item_kind == DbLabelledKind::Txid {
            let frontward: Txid = bitcoin::consensus::encode::deserialize_hex(&ref_).unwrap();
            ref_ = frontward.to_string();
        }
        let label = if value.value.is_empty() {
            None
        } else {
            Some(value.value)
        };
        match value.item_kind {
            DbLabelledKind::Address => Label::Address(bip329::AddressRecord {
                ref_: Address::<NetworkUnchecked>::from_str(&ref_)
                    .expect("db contains valid adresses"),
                label,
            }),
            DbLabelledKind::OutPoint => Label::Output(bip329::OutputRecord {
                ref_: OutPoint::from_str(&ref_).expect(" db contais valid outpoints"),
                label,
                spendable: true,
            }),
            DbLabelledKind::Txid => Label::Transaction(bip329::TransactionRecord {
                ref_: bitcoin::consensus::encode::deserialize_hex(&ref_)
                    .expect("db contains valid txid"),
                label,
                // FIXME: "Optional key origin information referencing the wallet associated with the label"
                origin: None,
            }),
        }
    }
}

impl TryFrom<&rusqlite::Row<'_>> for DbLabel {
    type Error = rusqlite::Error;

    fn try_from(row: &rusqlite::Row) -> Result<Self, Self::Error> {
        let id: i64 = row.get(0)?;
        let wallet_id: i64 = row.get(1)?;
        let item_kind: i64 = row.get(2)?;
        let item: String = row.get(3)?;
        let value: String = row.get(4)?;

        Ok(DbLabel {
            id,
            wallet_id,
            item_kind: item_kind.into(),
            item,
            value,
        })
    }
}

/// A transaction together with its block info.
#[derive(Clone, Debug, PartialEq)]
pub struct DbWalletTransaction {
    pub transaction: bitcoin::Transaction,
    pub block_info: Option<DbBlockInfo>,
}

impl TryFrom<&rusqlite::Row<'_>> for DbWalletTransaction {
    type Error = rusqlite::Error;

    fn try_from(row: &rusqlite::Row) -> Result<Self, Self::Error> {
        let transaction: Vec<u8> = row.get(0)?;
        let transaction: bitcoin::Transaction =
            bitcoin::consensus::deserialize(&transaction).expect("We only store valid txs");
        let block_height: Option<i32> = row.get(1)?;
        let block_time: Option<u32> = row.get(2)?;
        assert_eq!(block_height.is_none(), block_time.is_none());
        let block_info = block_height.map(|height| DbBlockInfo {
            height,
            time: block_time.expect("Must be there if height is"),
        });

        Ok(DbWalletTransaction {
            transaction,
            block_info,
        })
    }
}
