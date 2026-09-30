//! Structural ancestry checks only. This does not establish chain inclusion,
//! coinbase uniqueness, ownership, maturity, or Bitcoin-only spendability.
//! A verified dependency must never by itself authorize an input poison.
pub mod retained;
pub mod search;

use std::collections::BTreeSet;

use miniscript::bitcoin::{consensus::deserialize, OutPoint, Transaction, Txid};

pub const MAX_PATH_TRANSACTIONS: usize = 64;
pub const MAX_TRANSACTION_BYTES: usize = 400_000;
pub const MAX_PATH_BYTES: usize = 4_000_000;

/// Ordered from the selected output's transaction towards a coinbase. Every
/// nonterminal link identifies the input whose parent is the following link.
#[derive(Debug)]
pub struct Link<'a> {
    pub transaction: &'a [u8],
    pub parent_input: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidSelection,
    PathLimit,
    ByteLimit,
    InvalidTransaction,
    WrongTransaction,
    InvalidOutput,
    InvalidInput,
    RepeatedTransaction,
    IncompletePath,
    InvalidTerminal,
    InvalidCoinbaseHeight,
}

/// A dependency path, NOT chain-exclusivity evidence. Fields cannot be supplied
/// through deserialization; callers must re-verify persisted raw links.
#[derive(Debug)]
pub struct CoinbaseDependency {
    selected: OutPoint,
    root: OutPoint,
    coinbase: Transaction,
    txids: Vec<Txid>,
}
impl CoinbaseDependency {
    pub fn selected(&self) -> OutPoint {
        self.selected
    }
    pub fn root(&self) -> OutPoint {
        self.root
    }
    pub fn coinbase(&self) -> &Transaction {
        &self.coinbase
    }
    pub fn txids(&self) -> &[Txid] {
        &self.txids
    }
    /// Check the exact BIP34 height prefix and coinbase script-size range.
    /// The caller must separately establish that BIP34 is active, this height
    /// is canonical, and no historical duplicate can supply the same outpoint.
    /// This check does not establish any of those facts.
    pub fn check_height_commitment(&self, height: u32) -> Result<(), Error> {
        // The node's contextual height is a signed int, never a wrapping u32.
        if height > i32::MAX as u32 {
            return Err(Error::InvalidCoinbaseHeight);
        }
        let script = &self.coinbase.input[0].script_sig;
        let expected = miniscript::bitcoin::script::Builder::new()
            .push_int(i64::from(height))
            .into_script();
        if !(2..=100).contains(&script.len()) || !script.as_bytes().starts_with(expected.as_bytes())
        {
            return Err(Error::InvalidCoinbaseHeight);
        }
        Ok(())
    }
}

/// Verify exact transaction IDs and dependency edges, ending at a structurally identified
/// coinbase. Its script, consensus validity and chain inclusion are not checked.
/// One proven dependency input is enough for the structural path;
/// this function makes no claim that that input is unavailable on another chain.
/// Empty, excessive, malformed, incomplete or inconsistent data fails closed.
pub fn verify(selected: OutPoint, links: &[Link<'_>]) -> Result<CoinbaseDependency, Error> {
    if selected.is_null() {
        return Err(Error::InvalidSelection);
    }
    if links.is_empty() || links.len() > MAX_PATH_TRANSACTIONS {
        return Err(Error::PathLimit);
    }
    let mut bytes = 0usize;
    for link in links {
        bytes = bytes
            .checked_add(link.transaction.len())
            .ok_or(Error::ByteLimit)?;
        if link.transaction.len() > MAX_TRANSACTION_BYTES || bytes > MAX_PATH_BYTES {
            return Err(Error::ByteLimit);
        }
    }
    let mut expected = selected;
    let mut seen = BTreeSet::new();
    let mut txids = Vec::with_capacity(links.len());
    for (index, link) in links.iter().enumerate() {
        // deserialize rejects trailing bytes, unlike partial decoding.
        let transaction: Transaction =
            deserialize(link.transaction).map_err(|_| Error::InvalidTransaction)?;
        let txid = transaction.compute_txid();
        if !seen.insert(txid) {
            return Err(Error::RepeatedTransaction);
        }
        if txid != expected.txid {
            return Err(Error::WrongTransaction);
        }
        if transaction.output.get(expected.vout as usize).is_none() {
            return Err(Error::InvalidOutput);
        }
        txids.push(txid);
        if transaction.is_coinbase() {
            if link.parent_input.is_some() || index + 1 != links.len() {
                return Err(Error::InvalidTerminal);
            }
            return Ok(CoinbaseDependency {
                selected,
                root: expected,
                coinbase: transaction,
                txids,
            });
        }
        let input = link.parent_input.ok_or(Error::IncompletePath)?;
        let parent = transaction
            .input
            .get(input)
            .ok_or(Error::InvalidInput)?
            .previous_output;
        if parent.is_null() {
            return Err(Error::InvalidInput);
        }
        expected = parent;
    }
    Err(Error::IncompletePath)
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniscript::bitcoin::{
        absolute, consensus::serialize, transaction, Amount, ScriptBuf, TxIn, TxOut, Witness,
    };

    pub(super) fn tx(parent: OutPoint) -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: parent,
                ..TxIn::default()
            }],
            output: vec![TxOut {
                value: Amount::from_sat(1000),
                script_pubkey: ScriptBuf::new(),
            }],
        }
    }
    pub(super) fn out(tx: &Transaction) -> OutPoint {
        OutPoint::new(tx.compute_txid(), 0)
    }
    fn result(selected: OutPoint, links: &[Link<'_>]) -> Error {
        verify(selected, links).unwrap_err()
    }

    #[test]
    fn direct_and_multihop_paths_bind_every_output() {
        let root = tx(OutPoint::null());
        let parent = tx(out(&root));
        let child = tx(out(&parent));
        let raw = [serialize(&child), serialize(&parent), serialize(&root)];
        let links = [
            Link {
                transaction: &raw[0],
                parent_input: Some(0),
            },
            Link {
                transaction: &raw[1],
                parent_input: Some(0),
            },
            Link {
                transaction: &raw[2],
                parent_input: None,
            },
        ];
        let proof = verify(out(&child), &links).unwrap();
        assert_eq!(proof.selected(), out(&child));
        assert_eq!(proof.root(), out(&root));
        assert_eq!(proof.coinbase(), &root);
        assert_eq!(
            proof.txids(),
            &[
                child.compute_txid(),
                parent.compute_txid(),
                root.compute_txid()
            ]
        );
        assert!(verify(out(&root), &links[2..]).is_ok());
        assert_eq!(
            result(OutPoint::new(child.compute_txid(), 1), &links),
            Error::InvalidOutput
        );
        assert_eq!(result(out(&child), &links[..2]), Error::IncompletePath);
    }

    #[test]
    fn substitutions_bad_edges_and_trailing_bytes_are_refused() {
        let root = tx(OutPoint::null());
        let child = tx(out(&root));
        let raw_root = serialize(&root);
        let raw_child = serialize(&child);
        let mut trailing = raw_root.clone();
        trailing.push(0);
        assert_eq!(
            result(
                out(&root),
                &[Link {
                    transaction: &trailing,
                    parent_input: None
                }]
            ),
            Error::InvalidTransaction
        );
        assert_eq!(
            result(
                out(&child),
                &[Link {
                    transaction: &raw_root,
                    parent_input: None
                }]
            ),
            Error::WrongTransaction
        );
        assert_eq!(
            result(
                out(&child),
                &[Link {
                    transaction: &raw_child,
                    parent_input: Some(1)
                }]
            ),
            Error::InvalidInput
        );
        assert_eq!(
            result(
                out(&child),
                &[Link {
                    transaction: &raw_child,
                    parent_input: None
                }]
            ),
            Error::IncompletePath
        );
        let bad = tx(OutPoint::new(root.compute_txid(), 1));
        let raw_bad = serialize(&bad);
        assert_eq!(
            result(
                out(&bad),
                &[
                    Link {
                        transaction: &raw_bad,
                        parent_input: Some(0)
                    },
                    Link {
                        transaction: &raw_root,
                        parent_input: None
                    }
                ]
            ),
            Error::InvalidOutput
        );
        assert_eq!(
            result(
                out(&root),
                &[Link {
                    transaction: &raw_root,
                    parent_input: Some(0)
                }]
            ),
            Error::InvalidTerminal
        );
    }

    #[test]
    fn witness_changes_do_not_create_different_dependency_roots() {
        let root = tx(OutPoint::null());
        let mut other = root.clone();
        other.input[0].witness = Witness::from_slice(&[vec![1; 32]]);
        assert_ne!(root.compute_wtxid(), other.compute_wtxid());
        let raw = serialize(&other);
        let proof = verify(
            out(&root),
            &[Link {
                transaction: &raw,
                parent_input: None,
            }],
        )
        .unwrap();
        assert_eq!(proof.root(), out(&root));
        // Successful structural verification cannot establish chain exclusivity.
    }

    #[test]
    fn selected_nonzero_input_and_malformed_path_endings() {
        let root = tx(OutPoint::null());
        let parent = tx(out(&root));
        let mut child = tx(out(&parent));
        child.input.push(TxIn {
            previous_output: out(&root),
            ..TxIn::default()
        });
        let raw = serialize(&child);
        let raw_root = serialize(&root);
        let proof = verify(
            out(&child),
            &[
                Link {
                    transaction: &raw,
                    parent_input: Some(1),
                },
                Link {
                    transaction: &raw_root,
                    parent_input: None,
                },
            ],
        )
        .unwrap();
        assert_eq!(proof.root(), out(&root));
        assert_eq!(proof.txids().len(), 2);
        assert_eq!(
            result(
                out(&child),
                &[
                    Link {
                        transaction: &raw,
                        parent_input: Some(0)
                    },
                    Link {
                        transaction: &raw,
                        parent_input: Some(0)
                    },
                ]
            ),
            Error::RepeatedTransaction
        );
        assert_eq!(
            result(
                out(&root),
                &[
                    Link {
                        transaction: &raw_root,
                        parent_input: None
                    },
                    Link {
                        transaction: &raw,
                        parent_input: Some(0)
                    },
                ]
            ),
            Error::InvalidTerminal
        );
        child.input[1].previous_output = OutPoint::null();
        let malformed = serialize(&child);
        assert_eq!(
            result(
                out(&child),
                &[Link {
                    transaction: &malformed,
                    parent_input: Some(1)
                }]
            ),
            Error::InvalidInput
        );
    }

    #[test]
    fn exact_depth_limit_is_accepted() {
        let mut transactions = vec![tx(OutPoint::null())];
        for _ in 1..MAX_PATH_TRANSACTIONS {
            transactions.push(tx(out(transactions.last().unwrap())));
        }
        let selected = out(transactions.last().unwrap());
        let raw: Vec<_> = transactions.iter().rev().map(serialize).collect();
        let links: Vec<_> = raw
            .iter()
            .enumerate()
            .map(|(i, bytes)| Link {
                transaction: bytes,
                parent_input: (i + 1 < raw.len()).then_some(0),
            })
            .collect();
        assert_eq!(
            verify(selected, &links).unwrap().txids().len(),
            MAX_PATH_TRANSACTIONS
        );
    }

    #[test]
    fn height_commitment_matches_consensus_prefix_not_numeric_equivalence() {
        fn check(script: Vec<u8>, height: u32) -> Result<(), Error> {
            let mut root = tx(OutPoint::null());
            root.input[0].script_sig = ScriptBuf::from_bytes(script);
            let raw = serialize(&root);
            verify(
                out(&root),
                &[Link {
                    transaction: &raw,
                    parent_input: None,
                }],
            )
            .unwrap()
            .check_height_commitment(height)
        }
        for height in [
            0,
            1,
            16,
            17,
            127,
            128,
            32_767,
            32_768,
            961_640,
            i32::MAX as u32,
        ] {
            let mut prefix = miniscript::bitcoin::script::Builder::new()
                .push_int(i64::from(height))
                .into_script()
                .into_bytes();
            prefix.push(0); // arbitrary miner suffix; also makes tiny prefixes >= 2 bytes
            assert_eq!(check(prefix.clone(), height), Ok(()));
            assert_eq!(check(prefix, height + 1), Err(Error::InvalidCoinbaseHeight));
        }
        // Literal vectors are independent of the builder used by the checker.
        for (height, prefix) in [
            (127, vec![0x01, 0x7f]),
            (128, vec![0x02, 0x80, 0x00]),
            (32_767, vec![0x02, 0xff, 0x7f]),
            (32_768, vec![0x03, 0x00, 0x80, 0x00]),
            (i32::MAX as u32, vec![0x04, 0xff, 0xff, 0xff, 0x7f]),
        ] {
            assert_eq!(check(prefix, height), Ok(()));
        }
        assert_eq!(check(vec![1, 1], 1), Err(Error::InvalidCoinbaseHeight)); // OP_1 required
        assert_eq!(
            check(vec![0x4c, 1, 17], 17),
            Err(Error::InvalidCoinbaseHeight)
        ); // nonminimal PUSHDATA1
        assert_eq!(check(vec![2, 17, 0], 17), Err(Error::InvalidCoinbaseHeight)); // redundant numeric zero
        assert_eq!(check(vec![1, 0x91], 17), Err(Error::InvalidCoinbaseHeight)); // negative script number
        assert_eq!(check(vec![1], 17), Err(Error::InvalidCoinbaseHeight)); // truncated
        assert_eq!(check(vec![0x51], 1), Err(Error::InvalidCoinbaseHeight)); // coinbase too short
        let mut maximum = vec![0; 100];
        maximum[0] = 0x51;
        assert_eq!(check(maximum.clone(), 1), Ok(()));
        maximum.push(0);
        assert_eq!(check(maximum, 1), Err(Error::InvalidCoinbaseHeight));
        assert_eq!(
            check(vec![0, 0], u32::MAX),
            Err(Error::InvalidCoinbaseHeight)
        );
    }

    #[test]
    fn bounds_are_checked_before_decoding() {
        let root = tx(OutPoint::null());
        let selected = out(&root);
        assert_eq!(result(OutPoint::null(), &[]), Error::InvalidSelection);
        assert_eq!(result(selected, &[]), Error::PathLimit);
        let links: Vec<_> = (0..=MAX_PATH_TRANSACTIONS)
            .map(|_| Link {
                transaction: &[],
                parent_input: None,
            })
            .collect();
        assert_eq!(result(selected, &links), Error::PathLimit);
        let bytes = vec![0; MAX_TRANSACTION_BYTES + 1];
        assert_eq!(
            result(
                selected,
                &[Link {
                    transaction: &bytes,
                    parent_input: None
                }]
            ),
            Error::ByteLimit
        );
        let bytes = vec![0; MAX_TRANSACTION_BYTES];
        let links: Vec<_> = (0..11)
            .map(|_| Link {
                transaction: &bytes,
                parent_input: None,
            })
            .collect();
        assert_eq!(result(selected, &links), Error::ByteLimit);
    }
}
