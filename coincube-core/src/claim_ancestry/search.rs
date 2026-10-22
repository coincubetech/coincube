//! Bounded, deterministic breadth-first candidate discovery. A candidate is only
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
#[derive(Clone, Copy)]
struct Predecessor {
    txid: Txid,
    input: usize,
}
struct Node {
    raw: Vec<u8>,
    transaction: Transaction,
    outpoint: OutPoint,
    predecessor: Option<Predecessor>,
    next_input: usize,
}
#[derive(Clone, Copy)]
struct Pending {
    outpoint: OutPoint,
    predecessor: Option<Predecessor>,
    max_bytes: usize,
}
/// Requests each distinct transaction at most once and shares count/byte limits
/// across all explored branches, including rejected candidates. Fetched nodes
/// expand in discovery order, with inputs visited lazily in transaction order.
/// This prevents one deep input from starving a short sibling path without
/// allocating a frontier proportional to a transaction's input count.
/// Any structural or budget error is terminal. The caller supplies transport
/// cancellation and a whole-search deadline; root qualification stays separate.
pub struct Search {
    selected: OutPoint,
    nodes: BTreeMap<Txid, Node>,
    order: Vec<Txid>,
    cursor: usize,
    edges: BTreeMap<Txid, BTreeSet<Txid>>,
    pending: Option<Pending>,
    ready: Option<Txid>,
    bytes: usize,
    error: Option<Error>,
}
impl Search {
    /// Start a bounded structural search without fetching or qualifying a root.
    pub fn new(selected: OutPoint) -> Result<Self, Error> {
        if selected.is_null() {
            return Err(Error::InvalidSelection);
        }
        Ok(Self {
            selected,
            nodes: BTreeMap::new(),
            order: Vec::new(),
            cursor: 0,
            edges: BTreeMap::new(),
            pending: None,
            ready: None,
            bytes: 0,
            error: None,
        })
    }
    /// Advance to one fetch, structural candidate or complete traversal.
    /// Repeated calls while a fetch is pending repeat the same request. Calling
    /// again after a candidate rejects that root and resumes its parent cursor.
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
    /// Lazily expand fetched nodes in FIFO order and validate every referenced
    /// output, including references whose transaction was already fetched.
    fn step_inner(&mut self) -> Result<Step, Error> {
        if let Some(pending) = self.pending {
            return Ok(Step::Fetch {
                txid: pending.outpoint.txid,
                max_bytes: pending.max_bytes,
            });
        }
        if let Some(txid) = self.ready.take() {
            let node = &self.nodes[&txid];
            Self::validate_output(node, node.outpoint)?;
            if node.transaction.is_coinbase() {
                return self.candidate(txid).map(Step::Candidate);
            }
            if node.transaction.input.is_empty() {
                return Err(Error::InvalidInput);
            }
        }
        if self.nodes.is_empty() {
            return self.request(self.selected, None);
        }
        while let Some(txid) = self.order.get(self.cursor).copied() {
            let node = self.nodes.get_mut(&txid).ok_or(Error::IncompletePath)?;
            if node.transaction.is_coinbase() || node.next_input == node.transaction.input.len() {
                self.cursor += 1;
                continue;
            }
            let input = node.next_input;
            let child = node.transaction.input[input].previous_output;
            node.next_input += 1;
            if child.is_null() {
                return Err(Error::InvalidInput);
            }
            if let Some(existing) = self.nodes.get(&child.txid) {
                Self::validate_output(existing, child)?;
                self.record_edge(txid, child.txid)?;
                continue;
            }
            let step = self.request(child, Some(Predecessor { txid, input }))?;
            self.record_edge(txid, child.txid)?;
            return Ok(step);
        }
        Ok(Step::Complete)
    }
    /// Reserve one outstanding fetch only after checking the unchanged shared
    /// node/byte bounds. The node cap also bounds any reconstructed path depth.
    fn request(
        &mut self,
        outpoint: OutPoint,
        predecessor: Option<Predecessor>,
    ) -> Result<Step, Error> {
        if self.nodes.len() >= MAX_PATH_TRANSACTIONS {
            return Err(Error::PathLimit);
        }
        let max_bytes = MAX_PATH_BYTES
            .saturating_sub(self.bytes)
            .min(MAX_TRANSACTION_BYTES);
        if max_bytes == 0 {
            return Err(Error::ByteLimit);
        }
        self.pending = Some(Pending {
            outpoint,
            predecessor,
            max_bytes,
        });
        Ok(Step::Fetch {
            txid: outpoint.txid,
            max_bytes,
        })
    }
    /// Reused transaction identity does not establish that another referenced
    /// output index exists; check every edge's outpoint independently.
    fn validate_output(node: &Node, outpoint: OutPoint) -> Result<(), Error> {
        node.transaction
            .output
            .get(outpoint.vout as usize)
            .map(|_| ())
            .ok_or(Error::InvalidOutput)
    }
    /// Reject cycles across all explored branches, not just the first-parent
    /// tree. Deduplicated edges keep reachability metadata bounded by the node
    /// cap, even when a transaction contains many repeated input references.
    fn record_edge(&mut self, parent: Txid, child: Txid) -> Result<(), Error> {
        if self
            .edges
            .get(&parent)
            .is_some_and(|children| children.contains(&child))
        {
            return Ok(());
        }
        let mut pending = BTreeSet::from([child]);
        let mut visited = BTreeSet::new();
        while let Some(txid) = pending.pop_first() {
            if txid == parent {
                return Err(Error::RepeatedTransaction);
            }
            if visited.insert(txid) {
                if let Some(children) = self.edges.get(&txid) {
                    pending.extend(children.iter().filter(|id| !visited.contains(*id)).copied());
                }
            }
        }
        self.edges.entry(parent).or_default().insert(child);
        Ok(())
    }
    /// Reconstruct the immutable first-discovery path and pass its exact raw
    /// links through the existing structural verifier before exposing a root.
    fn candidate(&self, root: Txid) -> Result<Candidate, Error> {
        let mut links = Vec::new();
        let mut txid = root;
        let mut parent_input = None;
        loop {
            if links.len() >= MAX_PATH_TRANSACTIONS {
                return Err(Error::PathLimit);
            }
            let node = self.nodes.get(&txid).ok_or(Error::IncompletePath)?;
            links.push(OwnedLink {
                transaction: node.raw.clone(),
                parent_input,
            });
            let Some(predecessor) = node.predecessor else {
                break;
            };
            txid = predecessor.txid;
            parent_input = Some(predecessor.input);
        }
        links.reverse();
        let borrowed: Vec<_> = links
            .iter()
            .map(|link| Link {
                transaction: &link.transaction,
                parent_input: link.parent_input,
            })
            .collect();
        let dependency = verify(self.selected, &borrowed)?;
        Ok(Candidate { links, dependency })
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
    /// Retain one transaction matching the requested identity and its immutable
    /// first predecessor;
    /// defer structural output validation and candidate emission to `step`.
    fn provide_inner(&mut self, raw: Vec<u8>) -> Result<(), Error> {
        let expected = self.pending.ok_or(Error::WrongTransaction)?;
        if raw.len() > MAX_TRANSACTION_BYTES
            || raw.len() > MAX_PATH_BYTES.saturating_sub(self.bytes)
        {
            return Err(Error::ByteLimit);
        }
        let transaction: Transaction = deserialize(&raw).map_err(|_| Error::InvalidTransaction)?;
        if transaction.compute_txid() != expected.outpoint.txid {
            return Err(Error::WrongTransaction);
        }
        self.bytes += raw.len();
        self.nodes.insert(
            expected.outpoint.txid,
            Node {
                raw,
                transaction,
                outpoint: expected.outpoint,
                predecessor: expected.predecessor,
                next_input: 0,
            },
        );
        self.order.push(expected.outpoint.txid);
        self.ready = Some(expected.outpoint.txid);
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
                second.compute_txid(),
                a.compute_txid(),
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
    fn repeated_pending_fetch_is_stable_and_candidate_advances_once() {
        let root = transaction(&[OutPoint::null()], 1);
        let selected = transaction(&[out(&root)], 2);
        let mut search = Search::new(out(&selected)).unwrap();
        for tx in [&selected, &root] {
            let before = search.nodes.len();
            let allowance = MAX_PATH_BYTES
                .saturating_sub(search.bytes)
                .min(MAX_TRANSACTION_BYTES);
            for _ in 0..3 {
                assert!(
                    matches!(search.step().unwrap(), Step::Fetch { txid, max_bytes }
                    if txid == tx.compute_txid() && max_bytes == allowance)
                );
                assert_eq!(search.nodes.len(), before);
                assert_eq!(search.order.len(), before);
            }
            search.provide(serialize(tx)).unwrap();
        }
        let Step::Candidate(candidate) = search.step().unwrap() else {
            panic!("one root expected");
        };
        assert_eq!(candidate.links[0].parent_input, Some(0));
        assert_eq!(candidate.dependency.root(), out(&root));
        assert!(matches!(search.step().unwrap(), Step::Complete));
        assert!(matches!(search.step().unwrap(), Step::Complete));
    }

    #[test]
    fn reused_unexpanded_parent_still_validates_each_output() {
        let root = transaction(&[OutPoint::null()], 1);
        let parent = transaction(&[out(&root)], 2);
        let invalid = OutPoint::new(parent.compute_txid(), 1);
        let selected = transaction(&[out(&parent), invalid], 3);
        let mut search = Search::new(out(&selected)).unwrap();
        for tx in [&selected, &parent] {
            assert!(
                matches!(search.step().unwrap(), Step::Fetch { txid, .. } if txid == tx.compute_txid())
            );
            search.provide(serialize(tx)).unwrap();
        }
        assert!(matches!(search.step(), Err(Error::InvalidOutput)));
        assert_eq!(search.nodes.len(), 2);
        assert!(matches!(search.step(), Err(Error::InvalidOutput)));
    }

    #[test]
    fn first_predecessor_survives_reused_outputs_and_retained_roundtrip() {
        let root = transaction(&[OutPoint::null()], 1);
        let mut parent = transaction(&[out(&root)], 2);
        parent.output.push(parent.output[0].clone());
        let selected = transaction(&[OutPoint::new(parent.compute_txid(), 1), out(&parent)], 3);
        let graph = graph(&[root.clone(), parent.clone(), selected.clone()]);
        let mut search = Search::new(out(&selected)).unwrap();
        let (found, requests) = candidates(&mut search, &graph).unwrap();
        assert_eq!(
            requests,
            vec![
                selected.compute_txid(),
                parent.compute_txid(),
                root.compute_txid()
            ]
        );
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0]
                .links
                .iter()
                .map(|link| link.parent_input)
                .collect::<Vec<_>>(),
            vec![Some(0), Some(0), None]
        );
        assert_eq!(search.nodes[&parent.compute_txid()].outpoint.vout, 1);
        let retained = retained::RetainedPath::new(out(&selected), found[0].links.clone()).unwrap();
        let restored = retained::RetainedPath::decode(out(&selected), &retained.encode()).unwrap();
        assert_eq!(restored.reverify().unwrap().root(), out(&root));
        assert_eq!(restored.reverify().unwrap().selected(), out(&selected));
    }

    #[test]
    fn rejected_short_root_continues_to_an_earlier_deeper_branch() {
        let deep_root = transaction(&[OutPoint::null()], 1);
        let deep = transaction(&[out(&deep_root)], 2);
        let short = transaction(&[OutPoint::null()], 3);
        let selected = transaction(&[out(&deep), out(&short)], 4);
        let mut search = Search::new(out(&selected)).unwrap();
        let (found, requests) = candidates(
            &mut search,
            &graph(&[
                deep_root.clone(),
                deep.clone(),
                short.clone(),
                selected.clone(),
            ]),
        )
        .unwrap();
        assert_eq!(
            requests,
            vec![
                selected.compute_txid(),
                deep.compute_txid(),
                short.compute_txid(),
                deep_root.compute_txid()
            ]
        );
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].dependency.root(), out(&short));
        assert_eq!(found[1].dependency.root(), out(&deep_root));
        assert_eq!(found[0].links[0].parent_input, Some(1));
        assert_eq!(found[1].links[0].parent_input, Some(0));
        assert_eq!(found[1].links[1].parent_input, Some(0));
    }

    #[test]
    fn high_fanin_is_expanded_lazily_and_unused_bad_edges_do_not_preempt_a_root() {
        let root = transaction(&[OutPoint::null()], 1);
        let parents: Vec<_> = (0..8000)
            .map(|vout| OutPoint::new(root.compute_txid(), vout))
            .collect();
        let selected = transaction(&parents, 2);
        assert!(serialize(&selected).len() < MAX_TRANSACTION_BYTES);
        let mut search = Search::new(out(&selected)).unwrap();
        assert!(matches!(search.step().unwrap(), Step::Fetch { .. }));
        search.provide(serialize(&selected)).unwrap();
        assert!(
            matches!(search.step().unwrap(), Step::Fetch { txid, .. } if txid == root.compute_txid())
        );
        assert_eq!(search.nodes.len(), 1);
        assert_eq!(search.order.len(), 1);
        assert_eq!(search.edges.values().map(BTreeSet::len).sum::<usize>(), 1);
        assert_eq!(search.nodes[&selected.compute_txid()].next_input, 1);
        search.provide(serialize(&root)).unwrap();
        assert!(matches!(search.step().unwrap(), Step::Candidate(_)));
        assert_eq!(search.nodes.len(), 2);
        // Continuing after that root is rejected still checks the next vout.
        assert!(matches!(search.step(), Err(Error::InvalidOutput)));
    }

    #[test]
    fn explored_edges_reject_cross_branch_and_self_cycles_without_rejecting_diamonds() {
        // Real transaction identity is enforced separately by provide. Synthetic
        // graph IDs isolate the reachability check without requiring hash cycles.
        let ids: Vec<_> = (1..=4)
            .map(|tag| transaction(&[OutPoint::null()], tag).compute_txid())
            .collect();
        let mut search = Search::new(OutPoint::new(ids[0], 0)).unwrap();
        for (parent, child) in [(0, 1), (0, 2), (1, 3), (2, 3), (1, 2)] {
            search.record_edge(ids[parent], ids[child]).unwrap();
        }
        search.record_edge(ids[1], ids[2]).unwrap();
        let before = search.edges.clone();
        assert_eq!(
            search.record_edge(ids[2], ids[1]),
            Err(Error::RepeatedTransaction)
        );
        assert_eq!(
            search.record_edge(ids[3], ids[0]),
            Err(Error::RepeatedTransaction)
        );
        assert_eq!(
            search.record_edge(ids[3], ids[3]),
            Err(Error::RepeatedTransaction)
        );
        assert_eq!(search.edges, before);
    }

    #[test]
    fn mixed_null_input_and_trailing_bytes_fail_permanently() {
        let root = transaction(&[OutPoint::null()], 1);
        let selected = transaction(&[OutPoint::null(), out(&root)], 2);
        let mut search = Search::new(out(&selected)).unwrap();
        assert!(matches!(search.step().unwrap(), Step::Fetch { .. }));
        search.provide(serialize(&selected)).unwrap();
        assert!(matches!(search.step(), Err(Error::InvalidInput)));
        assert_eq!(search.provide(serialize(&root)), Err(Error::InvalidInput));
        let mut search = Search::new(out(&root)).unwrap();
        assert!(matches!(search.step().unwrap(), Step::Fetch { .. }));
        let mut trailing = serialize(&root);
        trailing.push(0);
        assert_eq!(search.provide(trailing), Err(Error::InvalidTransaction));
        assert!(matches!(search.step(), Err(Error::InvalidTransaction)));
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
    fn finds_short_root_without_exhausting_an_earlier_deep_input() {
        let deep = chain(MAX_PATH_TRANSACTIONS, None);
        let short = transaction(&[OutPoint::null()], 200);
        for short_input in [0, 1] {
            let mut parents = [out(deep.last().unwrap()), out(&short)];
            if short_input == 0 {
                parents.swap(0, 1);
            }
            let selected = transaction(&parents, 201);
            let mut transactions = deep.clone();
            transactions.extend([short.clone(), selected.clone()]);
            let graph = graph(&transactions);
            let mut search = Search::new(out(&selected)).unwrap();
            let mut requests = Vec::new();
            let candidate = loop {
                match search.step().unwrap() {
                    Step::Fetch { txid, .. } => {
                        requests.push(txid);
                        search.provide(graph[&txid].clone()).unwrap();
                    }
                    Step::Candidate(candidate) => break candidate,
                    Step::Complete => panic!("the short structural path must be found"),
                }
            };
            assert_eq!(candidate.dependency.root(), out(&short));
            assert_eq!(candidate.links.len(), 2);
            assert_eq!(candidate.links[0].parent_input, Some(short_input));
            assert_eq!(candidate.links[1].parent_input, None);
            assert_eq!(requests.len(), short_input + 2);
            assert!(search.nodes.len() <= 3);
        }
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
