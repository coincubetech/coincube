//! Bounded, deterministic depth-first candidate discovery. A candidate is only
//! a structural dependency, not evidence that its root is Bitcoin-exclusive.
use super::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct OwnedLink {
    pub transaction: Vec<u8>,
    pub parent_input: Option<usize>,
}
#[derive(Debug)]
pub struct Candidate {
    pub links: Vec<OwnedLink>,
    pub dependency: CoinbaseDependency,
}
#[derive(Debug)]
pub enum Step {
    /// The transport must enforce this remaining byte allowance while reading.
    Fetch { txid: Txid, max_bytes: usize },
    /// Reverify retained links after persistence. Qualify the root separately.
    /// Advancing permanently skips this root: stop on transient qualification
    /// failures instead of treating unavailable observations as disqualification.
    Candidate(Candidate),
    /// No further structural root candidates within the fully visited graph.
    /// This is never proof of replay safety or permission to spend.
    Complete,
}
struct Node {
    raw: Vec<u8>,
    transaction: Transaction,
}
struct Frame {
    outpoint: OutPoint,
    next_input: usize,
}
/// Requests each distinct transaction at most once and shares count/byte limits
/// across all explored branches, including rejected candidates. Inputs are
/// visited in transaction order. Any structural or budget error is terminal.
/// The caller supplies transport cancellation and a whole-search deadline.
pub struct Search {
    selected: OutPoint,
    nodes: BTreeMap<Txid, Node>,
    stack: Vec<Frame>,
    exhausted: BTreeSet<Txid>,
    pending: Option<Txid>,
    bytes: usize,
    error: Option<Error>,
}
impl Search {
    pub fn new(selected: OutPoint) -> Result<Self, Error> {
        if selected.is_null() {
            return Err(Error::InvalidSelection);
        }
        Ok(Self {
            selected,
            nodes: BTreeMap::new(),
            stack: vec![Frame {
                outpoint: selected,
                next_input: 0,
            }],
            exhausted: BTreeSet::new(),
            pending: None,
            bytes: 0,
            error: None,
        })
    }
    pub fn step(&mut self) -> Result<Step, Error> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let result = self.step_inner();
        if let Err(error) = result {
            self.error = Some(error);
        }
        result
    }
    fn step_inner(&mut self) -> Result<Step, Error> {
        loop {
            let Some(frame) = self.stack.last_mut() else {
                return Ok(Step::Complete);
            };
            let txid = frame.outpoint.txid;
            let Some(node) = self.nodes.get(&txid) else {
                if self.nodes.len() >= MAX_PATH_TRANSACTIONS {
                    return Err(Error::PathLimit);
                }
                let remaining = MAX_PATH_BYTES
                    .saturating_sub(self.bytes)
                    .min(MAX_TRANSACTION_BYTES);
                if remaining == 0 {
                    return Err(Error::ByteLimit);
                }
                self.pending = Some(txid);
                return Ok(Step::Fetch {
                    txid,
                    max_bytes: remaining,
                });
            };
            if node
                .transaction
                .output
                .get(frame.outpoint.vout as usize)
                .is_none()
            {
                return Err(Error::InvalidOutput);
            }
            if node.transaction.is_coinbase() {
                let links: Vec<_> = self
                    .stack
                    .iter()
                    .enumerate()
                    .map(|(index, frame)| OwnedLink {
                        transaction: self.nodes[&frame.outpoint.txid].raw.clone(),
                        parent_input: if index + 1 == self.stack.len() {
                            None
                        } else {
                            Some(frame.next_input - 1)
                        },
                    })
                    .collect();
                let borrowed: Vec<_> = links
                    .iter()
                    .map(|link| Link {
                        transaction: &link.transaction,
                        parent_input: link.parent_input,
                    })
                    .collect();
                let dependency = verify(self.selected, &borrowed)?;
                self.exhausted.insert(txid);
                self.stack.pop();
                return Ok(Step::Candidate(Candidate { links, dependency }));
            }
            if node.transaction.input.is_empty() {
                return Err(Error::InvalidInput);
            }
            if frame.next_input == node.transaction.input.len() {
                self.exhausted.insert(txid);
                self.stack.pop();
                continue;
            }
            let child = node.transaction.input[frame.next_input].previous_output;
            frame.next_input += 1;
            if child.is_null() {
                return Err(Error::InvalidInput);
            }
            if self
                .stack
                .iter()
                .any(|frame| frame.outpoint.txid == child.txid)
            {
                return Err(Error::RepeatedTransaction);
            }
            if self.exhausted.contains(&child.txid) {
                if self.nodes[&child.txid]
                    .transaction
                    .output
                    .get(child.vout as usize)
                    .is_none()
                {
                    return Err(Error::InvalidOutput);
                }
                continue;
            }
            if self.stack.len() >= MAX_PATH_TRANSACTIONS {
                return Err(Error::PathLimit);
            }
            self.stack.push(Frame {
                outpoint: child,
                next_input: 0,
            });
        }
    }
    /// Supply exactly the requested txid's bytes. Wrong identity, trailing data,
    /// excess bytes, and unsolicited responses permanently fail this search.
    pub fn provide(&mut self, raw: Vec<u8>) -> Result<(), Error> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let result = self.provide_inner(raw);
        if let Err(error) = result {
            self.error = Some(error);
        }
        result
    }
    fn provide_inner(&mut self, raw: Vec<u8>) -> Result<(), Error> {
        let expected = self.pending.ok_or(Error::WrongTransaction)?;
        if raw.len() > MAX_TRANSACTION_BYTES
            || raw.len() > MAX_PATH_BYTES.saturating_sub(self.bytes)
        {
            return Err(Error::ByteLimit);
        }
        let transaction: Transaction = deserialize(&raw).map_err(|_| Error::InvalidTransaction)?;
        if transaction.compute_txid() != expected {
            return Err(Error::WrongTransaction);
        }
        self.bytes += raw.len();
        self.nodes.insert(expected, Node { raw, transaction });
        self.pending = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniscript::bitcoin::{
        absolute, consensus::serialize, transaction, Amount, ScriptBuf, TxIn, TxOut,
    };

    fn transaction(parents: &[OutPoint], tag: u8) -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: parents
                .iter()
                .map(|parent| TxIn {
                    previous_output: *parent,
                    script_sig: ScriptBuf::from_bytes(vec![tag]),
                    ..TxIn::default()
                })
                .collect(),
            output: vec![TxOut {
                value: Amount::from_sat(1000),
                script_pubkey: ScriptBuf::new(),
            }],
        }
    }
    fn out(tx: &Transaction) -> OutPoint {
        OutPoint::new(tx.compute_txid(), 0)
    }
    fn graph(txs: &[Transaction]) -> BTreeMap<Txid, Vec<u8>> {
        txs.iter()
            .map(|tx| (tx.compute_txid(), serialize(tx)))
            .collect()
    }
    fn candidates(
        search: &mut Search,
        graph: &BTreeMap<Txid, Vec<u8>>,
    ) -> Result<(Vec<Candidate>, Vec<Txid>), Error> {
        let mut candidates = Vec::new();
        let mut requests = Vec::new();
        loop {
            match search.step()? {
                Step::Fetch { txid, max_bytes } => {
                    assert!(graph[&txid].len() <= max_bytes);
                    requests.push(txid);
                    search.provide(graph[&txid].clone())?;
                }
                Step::Candidate(candidate) => candidates.push(candidate),
                Step::Complete => return Ok((candidates, requests)),
            }
        }
    }
    #[test]
    fn visits_alternate_inputs_and_reuses_exhausted_shared_parents() {
        let a = transaction(&[OutPoint::null()], 1);
        let b = transaction(&[OutPoint::null()], 2);
        let mut first = transaction(&[out(&a)], 3);
        first.output.push(first.output[0].clone());
        let other_output = OutPoint::new(first.compute_txid(), 1);
        let second = transaction(&[other_output, out(&b)], 4);
        let selected = transaction(&[out(&first), out(&second)], 5);
        let graph = graph(&[
            a.clone(),
            b.clone(),
            first.clone(),
            second.clone(),
            selected.clone(),
        ]);
        let mut search = Search::new(out(&selected)).unwrap();
        let (candidates, requests) = candidates(&mut search, &graph).unwrap();
        assert_eq!(
            requests,
            vec![
                selected.compute_txid(),
                first.compute_txid(),
                a.compute_txid(),
                second.compute_txid(),
                b.compute_txid()
            ]
        );
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].dependency.root(), out(&a));
        assert_eq!(candidates[1].dependency.root(), out(&b));
        assert_eq!(
            candidates[1]
                .links
                .iter()
                .map(|link| link.parent_input)
                .collect::<Vec<_>>(),
            vec![Some(1), Some(1), None]
        );
        for candidate in candidates {
            let links: Vec<_> = candidate
                .links
                .iter()
                .map(|link| Link {
                    transaction: &link.transaction,
                    parent_input: link.parent_input,
                })
                .collect();
            assert_eq!(
                verify(out(&selected), &links).unwrap().root(),
                candidate.dependency.root()
            );
        }
        assert!(matches!(search.step().unwrap(), Step::Complete));
    }
    #[test]
    fn invalid_output_of_an_exhausted_parent_still_fails() {
        let root = transaction(&[OutPoint::null()], 1);
        let invalid = OutPoint {
            vout: 1,
            ..out(&root)
        };
        let selected = transaction(&[out(&root), invalid], 2);
        let graph = graph(&[root, selected.clone()]);
        let mut search = Search::new(out(&selected)).unwrap();
        assert_eq!(
            candidates(&mut search, &graph).unwrap_err(),
            Error::InvalidOutput
        );
        assert!(matches!(search.step(), Err(Error::InvalidOutput)));
    }
    #[test]
    fn malformed_unrequested_and_wrong_identity_are_terminal() {
        let root = transaction(&[OutPoint::null()], 1);
        for (request, bytes, expected) in [
            (false, serialize(&root), Error::WrongTransaction),
            (true, vec![0], Error::InvalidTransaction),
            (
                true,
                serialize(&transaction(&[OutPoint::null()], 2)),
                Error::WrongTransaction,
            ),
            (true, vec![0; MAX_TRANSACTION_BYTES + 1], Error::ByteLimit),
        ] {
            let mut search = Search::new(out(&root)).unwrap();
            if request {
                assert!(matches!(search.step().unwrap(), Step::Fetch { .. }));
            }
            assert_eq!(search.provide(bytes), Err(expected));
            assert!(matches!(search.step(),Err(e) if e == expected));
            assert_eq!(search.provide(serialize(&root)), Err(expected));
        }
        assert!(matches!(
            Search::new(OutPoint::null()),
            Err(Error::InvalidSelection)
        ));
    }
    fn chain(count: usize, size: Option<usize>) -> Vec<Transaction> {
        let mut txs = Vec::new();
        let mut parent = OutPoint::null();
        for _ in 0..count {
            let mut tx = transaction(&[parent], 1);
            if let Some(size) = size {
                tx.output[0].script_pubkey = ScriptBuf::from_bytes(vec![0; size - 100]);
                let used = serialize(&tx).len();
                tx.output[0].script_pubkey =
                    ScriptBuf::from_bytes(vec![0; size - 100 + size - used]);
                assert_eq!(serialize(&tx).len(), size);
            }
            parent = out(&tx);
            txs.push(tx);
        }
        txs
    }
    #[test]
    fn count_limit_is_shared_and_checked_before_an_extra_fetch() {
        for count in [MAX_PATH_TRANSACTIONS, MAX_PATH_TRANSACTIONS + 1] {
            let txs = chain(count, None);
            let selected = out(txs.last().unwrap());
            let mut search = Search::new(selected).unwrap();
            let result = candidates(&mut search, &graph(&txs));
            if count == MAX_PATH_TRANSACTIONS {
                let (found, requests) = result.unwrap();
                assert_eq!(found.len(), 1);
                assert_eq!(found[0].links.len(), count);
                assert_eq!(requests.len(), count);
            } else {
                assert_eq!(result.unwrap_err(), Error::PathLimit);
                assert_eq!(search.nodes.len(), MAX_PATH_TRANSACTIONS);
            }
        }
    }
    #[test]
    fn total_byte_limit_includes_every_fetched_branch() {
        let count = MAX_PATH_BYTES / MAX_TRANSACTION_BYTES;
        for count in [count, count + 1] {
            let txs = chain(count, Some(MAX_TRANSACTION_BYTES));
            let mut search = Search::new(out(txs.last().unwrap())).unwrap();
            let result = candidates(&mut search, &graph(&txs));
            if count * MAX_TRANSACTION_BYTES == MAX_PATH_BYTES {
                assert_eq!(result.unwrap().0.len(), 1);
            } else {
                assert_eq!(result.unwrap_err(), Error::ByteLimit);
            }
            assert_eq!(search.bytes, MAX_PATH_BYTES);
        }
    }
}
