//! Test-only foreign wallets of every Split step-1 shape, with scan reports
//! of the same coins on both chains.

use std::str::FromStr;

use coincube_core::{
    chain::ChainId,
    miniscript::bitcoin::{
        self, absolute,
        bip32::{DerivationPath, Xpriv, Xpub},
        hashes::Hash,
        secp256k1::Secp256k1,
        transaction, Amount, BlockHash, Network, OutPoint, Transaction, TxIn, TxOut, Txid,
    },
};

use super::{
    foreign_scan::{Branch, BranchCoverage, DiscoveredCoin, ScanDescriptor, ScanReport},
    foreign_split_inventory::SplitInventory,
};

pub const FORK: u64 = 900;
pub const GENERATION: u64 = 5;
pub const BITCOIN_TIP_HEIGHT: u32 = 960;
pub const BTCB2_TIP_HEIGHT: u32 = 950;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Wpkh,
    ShWpkh,
    Pkh,
    WshSortedMulti,
    WshMulti,
}

pub const SHAPES: [Shape; 5] = [
    Shape::Wpkh,
    Shape::ShWpkh,
    Shape::Pkh,
    Shape::WshSortedMulti,
    Shape::WshMulti,
];

pub struct Wallet {
    pub external: ScanDescriptor,
    pub internal: ScanDescriptor,
    /// Enough keys to satisfy every input.
    pub signers: Vec<Xpriv>,
}

pub fn master(seed: u8) -> Xpriv {
    Xpriv::new_master(Network::Bitcoin, &[seed; 32]).unwrap()
}

fn account(master: &Xpriv, path: &str) -> String {
    let secp = Secp256k1::new();
    let child = master
        .derive_priv(&secp, &DerivationPath::from_str(path).unwrap())
        .unwrap();
    format!(
        "[{}/{}]{}",
        master.fingerprint(&secp),
        path.trim_start_matches("m/"),
        Xpub::from_priv(&secp, &child)
    )
}

pub fn wallet(shape: Shape) -> Wallet {
    let (template, signers) = match shape {
        Shape::Wpkh => (
            format!("wpkh({}/{{b}}/*)", account(&master(1), "m/84'/0'/0'")),
            vec![master(1)],
        ),
        Shape::ShWpkh => (
            format!("sh(wpkh({}/{{b}}/*))", account(&master(1), "m/49'/0'/0'")),
            vec![master(1)],
        ),
        Shape::Pkh => (
            format!("pkh({}/{{b}}/*)", account(&master(1), "m/44'/0'/0'")),
            vec![master(1)],
        ),
        Shape::WshSortedMulti | Shape::WshMulti => {
            let keys: Vec<_> = [1, 2, 3]
                .iter()
                .map(|seed| format!("{}/{{b}}/*", account(&master(*seed), "m/48'/0'/0'/2'")))
                .collect();
            let name = if shape == Shape::WshMulti {
                "multi"
            } else {
                "sortedmulti"
            };
            (
                format!("wsh({name}(2,{}))", keys.join(",")),
                vec![master(1), master(2)],
            )
        }
    };
    let parse = |branch, step: u32| {
        ScanDescriptor::parse(branch, &template.replace("{b}", &step.to_string())).unwrap()
    };
    Wallet {
        external: parse(Branch::External, 0),
        internal: parse(Branch::Internal, 1),
        signers,
    }
}

pub fn block_hash(height: u64) -> BlockHash {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(&height.to_le_bytes());
    BlockHash::from_byte_array(bytes)
}

/// A coin paying `wallet`'s address at `branch`/`index`, confirmed at
/// `height` (unconfirmed when `None`).
pub fn coin(
    wallet: &Wallet,
    branch: Branch,
    index: u32,
    sats: u64,
    height: Option<u32>,
) -> DiscoveredCoin {
    let descriptor = match branch {
        Branch::External => &wallet.external,
        Branch::Internal => &wallet.internal,
    };
    let previous = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(
                Txid::from_byte_array([index as u8 + 1; 32]),
                sats as u32,
            ),
            ..TxIn::default()
        }],
        output: vec![
            TxOut {
                value: Amount::from_sat(1_000),
                script_pubkey: bitcoin::ScriptBuf::new_op_return([index as u8]),
            },
            TxOut {
                value: Amount::from_sat(sats),
                script_pubkey: descriptor.script(index).unwrap(),
            },
        ],
    };
    DiscoveredCoin {
        branch,
        index,
        outpoint: OutPoint::new(previous.compute_txid(), 1),
        output: previous.output[1].clone(),
        previous,
        confirmed: height.is_some(),
        block_height: height,
        block_hash: height.map(|h| block_hash(u64::from(h))),
    }
}

pub fn walk(end_exclusive: u32, last_used: Option<u32>) -> Vec<BranchCoverage> {
    [Branch::External, Branch::Internal]
        .iter()
        .map(|branch| BranchCoverage {
            branch: *branch,
            start: 0,
            end_exclusive,
            last_used,
        })
        .collect()
}

pub fn report(chain: ChainId, coins: Vec<DiscoveredCoin>) -> ScanReport {
    let (tip, height) = match chain {
        ChainId::Bitcoin => (
            block_hash(u64::from(BITCOIN_TIP_HEIGHT)),
            BITCOIN_TIP_HEIGHT,
        ),
        _ => (block_hash(u64::from(BTCB2_TIP_HEIGHT)), BTCB2_TIP_HEIGHT),
    };
    let report = ScanReport::for_test(chain, GENERATION, tip, coins)
        .with_coverage(walk(30, Some(3)))
        .with_tip_height(height);
    if chain == ChainId::BitcoinBlake2b {
        report.with_fork_height(Some(FORK))
    } else {
        report
    }
}

/// The two pre-fork coins of `wallet` on both chains, joined.
pub fn shared_coins(wallet: &Wallet) -> Vec<DiscoveredCoin> {
    vec![
        coin(wallet, Branch::External, 0, 100_000, Some(880)),
        coin(wallet, Branch::Internal, 3, 50_000, Some(890)),
    ]
}

pub fn inventory(wallet: &Wallet) -> SplitInventory {
    let coins = shared_coins(wallet);
    SplitInventory::join(
        &report(ChainId::BitcoinBlake2b, coins.clone()),
        &report(ChainId::Bitcoin, coins),
        GENERATION,
        true,
    )
    .unwrap()
}
