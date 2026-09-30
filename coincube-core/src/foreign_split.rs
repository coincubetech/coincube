//! Split step 1: the Bitcoin-side poison self-transfer of a foreign (non-Cube)
//! wallet. Construction, deterministic reconstruction and finalization only.
//!
//! Step 1 spends every selected pre-fork coin of the foreign wallet on Bitcoin
//! to one fresh address of the same wallet, with the OP_RETURN poison shared
//! with Claim ([`crate::split_poison`]). It is the Bitcoin half of #568's
//! primary poison split; the BTCB2 half (step 2) spends the same original
//! outpoints into the target Cube once step 1 is confirmed.
//!
//! Supported source shapes, matching [`crate::unified_foreign`]: `pkh`, `wpkh`,
//! `sh(wpkh)`, `wsh(multi)` and `wsh(sortedmulti)`. Taproot (`tr`) is scan-only
//! in Split and is refused here, as is every other shape. Signatures must be
//! ordinary ECDSA `SIGHASH_ALL`, either implicit or an explicit `0x01`
//! (#585, owner decision F1); every other sighash type is refused.
//!
//! What this module does NOT do: it reads no chain, proves no coin is unspent
//! or that an address is fresh, checks no RDTS deployment or expiry margin,
//! runs no mempool preflight, reserves nothing, persists nothing and grants no
//! broadcast or step-2 authority. Callers must supply authenticated two-chain
//! observations and the observed fork height, and establish all of the above
//! separately before any signature is requested or broadcast.
//!
//! Only a Bitcoin OP_RETURN poison is built. Input poison waits for the #547
//! ancestry gate (owner decision D2).

use std::{collections::BTreeSet, fmt};

use miniscript::{
    bitcoin::{
        self,
        absolute::LockTime,
        hashes::Hash,
        psbt::Psbt,
        secp256k1,
        sighash::{EcdsaSighashType, Prevouts, SighashCache},
        transaction, Amount, BlockHash, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut,
        Txid,
    },
    descriptor::{DefiniteDescriptorKey, ShInner, Wildcard, WshInner},
    interpreter::{Interpreter, KeySigPair, SatisfiedConstraint},
    psbt::PsbtExt,
    Descriptor, DescriptorPublicKey, ForEachKey, Terminal,
};

use crate::{
    chain::ChainId,
    claim::BlockRef,
    spend::{self, InputAuthError},
    split_poison::{split_poison_fork_marker, split_poison_script},
};

/// Which of the foreign wallet's two descriptors a script belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SplitBranch {
    External,
    Internal,
}

/// Why a coin cannot be in step 1. Only a coin confirmed in the same block
/// below the fork height on both chains is shared history that step 1 can
/// separate (owner decision D10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotSplittable {
    /// No Bitcoin confirmation: BTCB2-only, spent on Bitcoin, or unconfirmed.
    NoBitcoinConfirmation,
    /// No BTCB2 confirmation: Bitcoin-only, spent on BTCB2, or unconfirmed.
    NoBtcb2Confirmation,
    /// Confirmed at or after the fork height.
    PostFork,
    /// The chains name different confirming blocks.
    ChainsDisagree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Step 1 runs on Bitcoin mainnet or testnet4 only.
    UnsupportedChain(ChainId),
    /// A `tr` source descriptor: Taproot is scan-only in Split.
    Taproot,
    /// Not one of the supported shapes, or keys that cannot be derived
    /// unambiguously (multipath, hardened steps or hardened wildcard).
    UnsupportedDescriptor,
    /// A coin on the internal branch without an internal descriptor.
    MissingInternal,
    /// The internal (change) descriptor is not the external descriptor's
    /// wallet: it must differ only in each key's final, branch step.
    UnrelatedInternal,
    Empty,
    DuplicateInput(OutPoint),
    /// A hardened (or out-of-range) derivation index.
    InvalidIndex,
    NotSplittable {
        outpoint: OutPoint,
        reason: NotSplittable,
    },
    InputAuthentication {
        outpoint: OutPoint,
        reason: InputAuthError,
    },
    /// The authenticated previous output is not the stated descriptor script.
    ScriptMismatch(OutPoint),
    /// The destination is one of the spent scripts (including every fixed,
    /// non-ranged descriptor, which has only one address).
    DestinationNotFresh,
    Economics,
    /// Not a block-height locktime at or below the observed Bitcoin tip.
    Locktime,
    /// A recorded transaction is not an owned step-1 construction.
    Recorded(&'static str),
    /// Step 2 runs on Bitcoin Blake2b mainnet or testnet4 only.
    NotBitcoinBlake2b(ChainId),
    /// Step 2's coins are not exactly step 1's claimed prevouts.
    ClaimedMismatch,
    /// Step 2's target is not a P2WSH or P2TR script (the only Cube Vault
    /// address types), or is one of the foreign wallet's own scripts within
    /// the scanner's gap of a claimed coin's index.
    InvalidTarget,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedChain(chain) => {
                write!(f, "Split step 1 runs on Bitcoin only, not {chain:?}")
            }
            Self::Taproot => f.write_str("Taproot wallets can be scanned but not split"),
            Self::UnsupportedDescriptor => f.write_str("Unsupported foreign wallet descriptor"),
            Self::MissingInternal => f.write_str("The wallet's change descriptor is required"),
            Self::UnrelatedInternal => {
                f.write_str("The change descriptor does not belong to the same wallet")
            }
            Self::Empty => f.write_str("No coins selected"),
            Self::DuplicateInput(outpoint) => write!(f, "Coin {outpoint} selected twice"),
            Self::InvalidIndex => f.write_str("Hardened or invalid derivation index"),
            Self::NotSplittable { outpoint, reason } => {
                write!(f, "Coin {outpoint} cannot be split: {reason:?}")
            }
            Self::InputAuthentication { outpoint, reason } => {
                write!(f, "Coin {outpoint} is not authenticated: {reason}")
            }
            Self::ScriptMismatch(outpoint) => {
                write!(f, "Coin {outpoint} does not pay the stated wallet address")
            }
            Self::DestinationNotFresh => {
                f.write_str("Step 1 needs a fresh address of the same wallet")
            }
            Self::Economics => f.write_str("Fee or amount outside the allowed bounds"),
            Self::Locktime => f.write_str("Locktime must be a block height not above the tip"),
            Self::Recorded(reason) => f.write_str(reason),
            Self::NotBitcoinBlake2b(chain) => {
                write!(
                    f,
                    "Split step 2 runs on Bitcoin Blake2b only, not {chain:?}"
                )
            }
            Self::ClaimedMismatch => {
                f.write_str("Step 2 must spend exactly the coins step 1 claimed")
            }
            Self::InvalidTarget => f.write_str("Step 2 target address is not usable"),
        }
    }
}
impl std::error::Error for Error {}

/// The foreign wallet's public descriptors, checked against the Split matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitSource {
    external: Descriptor<DescriptorPublicKey>,
    internal: Option<Descriptor<DescriptorPublicKey>>,
}

impl SplitSource {
    pub fn new(
        external: Descriptor<DescriptorPublicKey>,
        internal: Option<Descriptor<DescriptorPublicKey>>,
    ) -> Result<Self, Error> {
        check_shape(&external)?;
        if let Some(internal) = &internal {
            check_shape(internal)?;
            if !same_wallet(&external, internal) {
                return Err(Error::UnrelatedInternal);
            }
        }
        Ok(Self { external, internal })
    }

    pub fn external(&self) -> &Descriptor<DescriptorPublicKey> {
        &self.external
    }

    pub fn internal(&self) -> Option<&Descriptor<DescriptorPublicKey>> {
        self.internal.as_ref()
    }

    /// The source identity (owner decision D9): SHA-256 over each
    /// descriptor's canonical text with its checksum, tagged by branch and
    /// length-prefixed so the pair cannot be re-split. It names the wallet
    /// in the Split journal and in the target Cube's `split_from`; it is not
    /// a secret and not evidence of ownership.
    pub fn digest(&self) -> bitcoin::hashes::sha256::Hash {
        use bitcoin::hashes::HashEngine;
        let mut engine = bitcoin::hashes::sha256::Hash::engine();
        for (tag, descriptor) in [(0_u8, Some(&self.external)), (1, self.internal.as_ref())] {
            engine.input(&[tag]);
            match descriptor {
                Some(descriptor) => {
                    let text = descriptor.to_string();
                    engine.input(&(text.len() as u64).to_be_bytes());
                    engine.input(text.as_bytes());
                }
                None => engine.input(&0_u64.to_be_bytes()),
            }
        }
        bitcoin::hashes::sha256::Hash::from_engine(engine)
    }

    fn derive(
        &self,
        branch: SplitBranch,
        index: u32,
    ) -> Result<Descriptor<DefiniteDescriptorKey>, Error> {
        if index >= (1 << 31) {
            return Err(Error::InvalidIndex);
        }
        let descriptor = match branch {
            SplitBranch::External => &self.external,
            SplitBranch::Internal => self.internal.as_ref().ok_or(Error::MissingInternal)?,
        };
        let definite = descriptor
            .at_derivation_index(index)
            .map_err(|_| Error::UnsupportedDescriptor)?;
        // Checked at construction; kept as a local refusal.
        if matches!(definite, Descriptor::Tr(_)) {
            return Err(Error::Taproot);
        }
        Ok(definite)
    }
}

fn keys(descriptor: &Descriptor<DescriptorPublicKey>) -> Vec<DescriptorPublicKey> {
    let mut keys = Vec::new();
    descriptor.for_each_key(|key| {
        keys.push(key.clone());
        true
    });
    keys
}

/// Whether `internal` is `external` with only each extended key's final
/// derivation step changed (the receive/change branch), the same way on every
/// key, and everything else, including the script structure, key order and
/// origins, identical.
fn same_wallet(
    external: &Descriptor<DescriptorPublicKey>,
    internal: &Descriptor<DescriptorPublicKey>,
) -> bool {
    let (outer, inner) = (keys(external), keys(internal));
    if outer.len() != inner.len() {
        return false;
    }
    let paired = outer.iter().zip(&inner).all(|pair| match pair {
        (DescriptorPublicKey::Single(a), DescriptorPublicKey::Single(b)) => a == b,
        (DescriptorPublicKey::XPub(a), DescriptorPublicKey::XPub(b)) => {
            let (pa, pb) = (a.derivation_path.as_ref(), b.derivation_path.as_ref());
            a.origin == b.origin
                && a.xkey == b.xkey
                && a.wildcard == b.wildcard
                && !pa.is_empty()
                && pa.len() == pb.len()
                && pa[..pa.len() - 1] == pb[..pb.len() - 1]
        }
        _ => false,
    });
    if !paired {
        return false;
    }
    // Every extended key moves between the same pair of branch steps (for
    // example `/0` to `/1`): one branch step for the whole wallet (#568 I1).
    let branch_steps: BTreeSet<_> = outer
        .iter()
        .zip(&inner)
        .filter_map(|pair| match pair {
            (DescriptorPublicKey::XPub(a), DescriptorPublicKey::XPub(b)) => Some((
                a.derivation_path.as_ref().last().copied(),
                b.derivation_path.as_ref().last().copied(),
            )),
            _ => None,
        })
        .collect();
    if branch_steps.len() > 1 {
        return false;
    }
    // Same structure: substitute the paired keys textually and compare.
    // Placeholders keep one substitution from matching another's output.
    let body = |d: &Descriptor<DescriptorPublicKey>| {
        let text = d.to_string();
        text.split('#').next().unwrap_or_default().to_owned()
    };
    let mut renamed = body(external);
    for (index, key) in outer.iter().enumerate() {
        renamed = renamed.replace(&key.to_string(), &format!("\u{0}{index}\u{0}"));
    }
    for (index, key) in inner.iter().enumerate() {
        renamed = renamed.replace(&format!("\u{0}{index}\u{0}"), &key.to_string());
    }
    renamed == body(internal)
}

fn check_shape(descriptor: &Descriptor<DescriptorPublicKey>) -> Result<(), Error> {
    let supported = match descriptor {
        Descriptor::Tr(_) => return Err(Error::Taproot),
        Descriptor::Pkh(_) | Descriptor::Wpkh(_) => true,
        Descriptor::Sh(sh) => matches!(sh.as_inner(), ShInner::Wpkh(_)),
        Descriptor::Wsh(wsh) => match wsh.as_inner() {
            WshInner::SortedMulti(_) => true,
            WshInner::Ms(ms) => matches!(ms.as_inner(), Terminal::Multi(_)),
        },
        Descriptor::Bare(_) => false,
    };
    let keys_ok = descriptor.for_each_key(|key| match key {
        DescriptorPublicKey::Single(_) => true,
        DescriptorPublicKey::XPub(xpub) => {
            xpub.wildcard != Wildcard::Hardened
                && xpub
                    .derivation_path
                    .as_ref()
                    .iter()
                    .all(|n| !n.is_hardened())
        }
        DescriptorPublicKey::MultiXPub(_) => false,
    });
    if !supported || !keys_ok || descriptor.sanity_check().is_err() {
        return Err(Error::UnsupportedDescriptor);
    }
    Ok(())
}

/// One foreign coin and its confirming block as observed on each chain.
/// Both observations must come from authenticated, chain-bound scans; this
/// module only checks that they agree and precede the fork.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitCoin {
    pub outpoint: OutPoint,
    pub branch: SplitBranch,
    pub index: u32,
    /// The complete previous transaction; its txid authenticates the prevout.
    pub previous: Transaction,
    pub bitcoin_block: Option<BlockRef>,
    pub btcb2_block: Option<BlockRef>,
}

impl SplitCoin {
    fn splittable(&self, fork_height: u64) -> Result<(), NotSplittable> {
        let bitcoin = self
            .bitcoin_block
            .ok_or(NotSplittable::NoBitcoinConfirmation)?;
        let btcb2 = self.btcb2_block.ok_or(NotSplittable::NoBtcb2Confirmation)?;
        if bitcoin.height >= fork_height || btcb2.height >= fork_height {
            return Err(NotSplittable::PostFork);
        }
        if bitcoin != btcb2 {
            return Err(NotSplittable::ChainsDisagree);
        }
        Ok(())
    }
}

/// Everything that identifies one step 1 apart from its fee and locktime.
/// The caller records these to reconstruct the step later.
#[derive(Debug, Clone, Copy)]
pub struct SplitInputs<'a> {
    pub chain: ChainId,
    pub source: &'a SplitSource,
    pub coins: &'a [SplitCoin],
    /// From the authenticated BTCB2 network anchor; never a constant.
    pub fork_height: u64,
    /// Receive (external) index of a fresh address of the same foreign
    /// wallet. Only the receive branch is allowed: it is the branch whose
    /// freshness the two-chain inventory proves (`FreshIndex::Proven`). This
    /// module only refuses a destination that is one of the spent scripts.
    pub destination: u32,
}

/// An unsigned step 1 with no public-field or deserialization bypass. It
/// certifies only the construction checks, not live eligibility.
#[derive(Debug, Clone)]
pub struct SplitStep1 {
    psbt: Psbt,
    chain: ChainId,
    source: SplitSource,
    /// Branch and index of each transaction input, in input order.
    inputs: Vec<(SplitBranch, u32)>,
    destination: u32,
    fork_marker: BlockHash,
    maximum_signed_vbytes: u64,
    /// Sum of the authenticated spent outputs.
    total: u64,
}

impl SplitStep1 {
    pub fn psbt(&self) -> &Psbt {
        &self.psbt
    }
    pub fn chain(&self) -> ChainId {
        self.chain
    }
    pub fn source(&self) -> &SplitSource {
        &self.source
    }
    /// Receive index of the destination.
    pub fn destination(&self) -> u32 {
        self.destination
    }
    /// The caller-supplied fork label in the poison payload. Not chain evidence.
    pub fn fork_marker(&self) -> BlockHash {
        self.fork_marker
    }
    /// The unsigned transaction's txid. It equals the broadcast txid only when
    /// every input is native segwit; P2PKH and P2SH-P2WPKH scriptSigs change
    /// it, so track the finalized transaction's own txid on chain.
    pub fn txid(&self) -> Txid {
        self.psbt.unsigned_tx.compute_txid()
    }
    /// The original outpoints step 2 must spend on BTCB2: every input.
    pub fn claimed_prevouts(&self) -> Vec<OutPoint> {
        self.psbt
            .unsigned_tx
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect()
    }
    /// Worst-case signed size the fee was charged for. The signed transaction
    /// is never larger.
    pub fn maximum_signed_vbytes(&self) -> u64 {
        self.maximum_signed_vbytes
    }
    pub fn fee(&self) -> Amount {
        let created: u64 = self
            .psbt
            .unsigned_tx
            .output
            .iter()
            .map(|output| output.value.to_sat())
            .sum();
        // Economics were checked at construction; this cannot underflow.
        Amount::from_sat(self.total - created)
    }
}

struct Selected {
    outpoint: OutPoint,
    branch: SplitBranch,
    index: u32,
    previous: Transaction,
    output: TxOut,
    definite: Descriptor<DefiniteDescriptorKey>,
}

struct Plan {
    selected: Vec<Selected>,
    destination: Descriptor<DefiniteDescriptorKey>,
    poison: ScriptBuf,
    total: u64,
    maximum_signed_vbytes: u64,
}

fn plan(inputs: &SplitInputs<'_>, fork_marker: BlockHash) -> Result<Plan, Error> {
    if !matches!(inputs.chain, ChainId::Bitcoin | ChainId::Testnet4) {
        return Err(Error::UnsupportedChain(inputs.chain));
    }
    let Selection {
        selected,
        outpoints,
        total,
    } = select(inputs.source, inputs.coins, inputs.fork_height)?;
    let destination = inputs
        .source
        .derive(SplitBranch::External, inputs.destination)?;
    let destination_script = destination.script_pubkey();
    if selected
        .iter()
        .any(|input| input.output.script_pubkey == destination_script)
    {
        return Err(Error::DestinationNotFresh);
    }
    let poison = split_poison_script(inputs.chain, fork_marker, &outpoints)
        .ok_or(Error::UnsupportedChain(inputs.chain))?;
    let maximum_signed_vbytes = maximum_signed_vbytes(
        &selected,
        step1_outputs(poison.clone(), destination_script, Amount::ZERO),
    )?;
    Ok(Plan {
        selected,
        destination,
        poison,
        total,
        maximum_signed_vbytes,
    })
}

struct Selection {
    /// Ordered by outpoint.
    selected: Vec<Selected>,
    outpoints: BTreeSet<OutPoint>,
    total: u64,
}

/// Authenticate every coin against its stated derivation and require it to be
/// shared pre-fork history on both chains. Shared by both steps: step 2 spends
/// the same original outpoints on BTCB2.
fn select(source: &SplitSource, coins: &[SplitCoin], fork_height: u64) -> Result<Selection, Error> {
    if coins.is_empty() {
        return Err(Error::Empty);
    }
    let mut coins: Vec<_> = coins.iter().collect();
    coins.sort_by_key(|coin| coin.outpoint);
    let mut outpoints = BTreeSet::new();
    let mut selected = Vec::with_capacity(coins.len());
    let mut total = 0u64;
    for coin in coins {
        if !outpoints.insert(coin.outpoint) {
            return Err(Error::DuplicateInput(coin.outpoint));
        }
        coin.splittable(fork_height)
            .map_err(|reason| Error::NotSplittable {
                outpoint: coin.outpoint,
                reason,
            })?;
        let definite = source.derive(coin.branch, coin.index)?;
        let output =
            spend::authenticate_previous_output(&coin.outpoint, Some(&coin.previous), None)
                .map_err(|reason| Error::InputAuthentication {
                    outpoint: coin.outpoint,
                    reason,
                })?;
        if definite.script_pubkey() != output.script_pubkey {
            return Err(Error::ScriptMismatch(coin.outpoint));
        }
        total = total
            .checked_add(output.value.to_sat())
            .filter(|total| *total <= Amount::MAX_MONEY.to_sat())
            .ok_or(Error::Economics)?;
        selected.push(Selected {
            outpoint: coin.outpoint,
            branch: coin.branch,
            index: coin.index,
            previous: coin.previous.clone(),
            output,
            definite,
        });
    }
    Ok(Selection {
        selected,
        outpoints,
        total,
    })
}

fn is_segwit(descriptor: &Descriptor<DefiniteDescriptorKey>) -> bool {
    !matches!(descriptor, Descriptor::Pkh(_))
}

/// Worst-case signed virtual size of a transaction spending `selected` to
/// `outputs` (a poison output is charged in full). Same method as the BTCB2
/// sweep review.
fn maximum_signed_vbytes(selected: &[Selected], outputs: Vec<TxOut>) -> Result<u64, Error> {
    let mut satisfaction = 0u64;
    for input in selected {
        satisfaction = satisfaction
            .checked_add(
                input
                    .definite
                    .max_weight_to_satisfy()
                    .map_err(|_| Error::UnsupportedDescriptor)?
                    .to_wu(),
            )
            .ok_or(Error::Economics)?;
    }
    let unsigned = unsigned_transaction(selected, outputs, LockTime::ZERO);
    // `max_weight_to_satisfy` measures from an input already carrying its
    // empty witness-stack byte; the unsigned serialization has no witness
    // section. A witness transaction needs marker+flag and that byte for every
    // input, including legacy inputs in a mixed transaction.
    let witness_overhead = if selected.iter().any(|input| is_segwit(&input.definite)) {
        2 + selected.len() as u64
    } else {
        0
    };
    unsigned
        .weight()
        .to_wu()
        .checked_add(satisfaction)
        .and_then(|weight| weight.checked_add(witness_overhead))
        .and_then(|weight| weight.checked_add(3))
        .map(|weight| weight / 4)
        .ok_or(Error::Economics)
}

/// Step 1's outputs: the value-0 poison, then the fresh destination.
fn step1_outputs(poison: ScriptBuf, destination: ScriptBuf, value: Amount) -> Vec<TxOut> {
    vec![
        TxOut {
            value: Amount::ZERO,
            script_pubkey: poison,
        },
        TxOut {
            value,
            script_pubkey: destination,
        },
    ]
}

fn unsigned_transaction(
    selected: &[Selected],
    output: Vec<TxOut>,
    locktime: LockTime,
) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: locktime,
        input: selected
            .iter()
            .map(|input| TxIn {
                previous_output: input.outpoint,
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                ..TxIn::default()
            })
            .collect(),
        output,
    }
}

/// Fee bounds shared by construction, reconstruction and finalization: at
/// least 1 sat/vB at the worst-case size, at most `MAX_FEERATE` there and
/// `MAX_FEE`, and a destination above both the wallet's dust floor and Core's
/// relay dust threshold for its script (546 sats for P2PKH, 540 for P2SH).
fn check_economics(
    total: u64,
    destination: u64,
    destination_script: &bitcoin::Script,
    maximum_signed_vbytes: u64,
) -> Result<(), Error> {
    let fee = total.checked_sub(destination).ok_or(Error::Economics)?;
    let ceiling = maximum_signed_vbytes
        .checked_mul(spend::MAX_FEERATE)
        .ok_or(Error::Economics)?
        .min(spend::MAX_FEE.to_sat());
    let dust = spend::DUST_OUTPUT_SATS.max(destination_script.minimal_non_dust().to_sat());
    if destination < dust || fee < maximum_signed_vbytes || fee > ceiling {
        return Err(Error::Economics);
    }
    Ok(())
}

/// Build the unsigned Bitcoin step 1 spending exactly `inputs.coins` to the
/// fresh destination, with the OP_RETURN poison as output 0.
///
/// The fee is `feerate_vb` times the worst-case signed size; the poison output
/// is charged before any signature exists. Inputs are ordered by outpoint and
/// the result is deterministic. `fork_marker` is labeling, not chain evidence.
/// `locktime` is the caller's anti-fee-sniping choice; it must be a block
/// height no greater than `bitcoin_tip_height`, the observed Bitcoin tip, so
/// the transaction is final for the next block.
pub fn create_split_step1(
    inputs: &SplitInputs<'_>,
    feerate_vb: u64,
    locktime: LockTime,
    bitcoin_tip_height: u32,
    fork_marker: BlockHash,
) -> Result<SplitStep1, Error> {
    check_locktime(locktime, bitcoin_tip_height)?;
    let plan = plan(inputs, fork_marker)?;
    if !(1..=spend::MAX_FEERATE).contains(&feerate_vb) {
        return Err(Error::Economics);
    }
    let fee = plan
        .maximum_signed_vbytes
        .checked_mul(feerate_vb)
        .ok_or(Error::Economics)?;
    let value = plan.total.checked_sub(fee).ok_or(Error::Economics)?;
    check_economics(
        plan.total,
        value,
        &plan.destination.script_pubkey(),
        plan.maximum_signed_vbytes,
    )?;
    build(inputs, plan, Amount::from_sat(value), locktime, fork_marker)
}

/// Block-height locktimes only (a time-based value cannot be checked against
/// a height observation), and never above the tip: Core treats a transaction
/// as final for the next block only when its height locktime is below it.
fn check_locktime(locktime: LockTime, bitcoin_tip_height: u32) -> Result<(), Error> {
    match locktime {
        LockTime::Blocks(height) if height.to_consensus_u32() <= bitcoin_tip_height => Ok(()),
        _ => Err(Error::Locktime),
    }
}

fn build(
    inputs: &SplitInputs<'_>,
    plan: Plan,
    value: Amount,
    locktime: LockTime,
    fork_marker: BlockHash,
) -> Result<SplitStep1, Error> {
    let tx = unsigned_transaction(
        &plan.selected,
        step1_outputs(plan.poison.clone(), plan.destination.script_pubkey(), value),
        locktime,
    );
    let mut psbt = psbt_for(tx, &plan.selected)?;
    // Marks output 1 as the wallet's own to signers.
    psbt.update_output_with_descriptor(1, &plan.destination)
        .map_err(|_| Error::UnsupportedDescriptor)?;
    Ok(SplitStep1 {
        psbt,
        chain: inputs.chain,
        source: inputs.source.clone(),
        inputs: plan
            .selected
            .iter()
            .map(|input| (input.branch, input.index))
            .collect(),
        destination: inputs.destination,
        fork_marker,
        maximum_signed_vbytes: plan.maximum_signed_vbytes,
        total: plan.total,
    })
}

/// The unsigned PSBT with each input's full previous transaction, its
/// witness UTXO when segwit, and the descriptor's scripts and key origins.
fn psbt_for(tx: Transaction, selected: &[Selected]) -> Result<Psbt, Error> {
    let mut psbt = Psbt::from_unsigned_tx(tx).map_err(|_| Error::Economics)?;
    for (index, input) in selected.iter().enumerate() {
        psbt.inputs[index].non_witness_utxo = Some(input.previous.clone());
        // BIP 174: witness_utxo only for segwit spends. Legacy P2PKH signers
        // use the full previous transaction.
        if is_segwit(&input.definite) {
            psbt.inputs[index].witness_utxo = Some(input.output.clone());
        }
        psbt.update_input_with_descriptor(index, &input.definite)
            .map_err(|_| Error::UnsupportedDescriptor)?;
    }
    Ok(psbt)
}

/// Rebuild an exact recorded step 1 from freshly authenticated coins. All
/// scripts, inputs and PSBT metadata are reconstructed, never restored from
/// the record; only the destination amount (so the original fee estimate need
/// not survive a restart), the locktime and the poison's fork label are read
/// from it, and the whole transaction must then match. Economics are checked
/// again, and the recorded locktime must still be a block height at or below
/// `bitcoin_tip_height`, the currently observed Bitcoin tip. The caller binds
/// the fork label, destination and txid to its intent. This reserves nothing
/// and authorizes no submission.
pub fn reconstruct_split_step1(
    inputs: &SplitInputs<'_>,
    recorded: &Transaction,
    bitcoin_tip_height: u32,
) -> Result<SplitStep1, Error> {
    if recorded.output.len() != 2 {
        return Err(Error::Recorded("Recorded step 1 must have two outputs"));
    }
    let fork_marker = split_poison_fork_marker(&recorded.output[0].script_pubkey)
        .ok_or(Error::Recorded("Recorded poison payload is invalid"))?;
    let plan = plan(inputs, fork_marker)?;
    let value = recorded.output[1].value;
    check_locktime(recorded.lock_time, bitcoin_tip_height)?;
    check_economics(
        plan.total,
        value.to_sat(),
        &plan.destination.script_pubkey(),
        plan.maximum_signed_vbytes,
    )?;
    let rebuilt = build(inputs, plan, value, recorded.lock_time, fork_marker)?;
    if rebuilt.psbt.unsigned_tx != *recorded {
        return Err(Error::Recorded(
            "Recorded transaction differs from the owned step-1 construction",
        ));
    }
    Ok(rebuilt)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeError {
    /// The signed PSBT is not the exact construction plus signatures.
    ConstructionChanged,
    /// A sighash other than implicit or explicit `SIGHASH_ALL`.
    UnsupportedSighash,
    InputAuthentication,
    InvalidSignature {
        input: usize,
    },
    Economics,
    /// Not enough valid signatures to satisfy every input.
    Unsatisfied,
    InvalidWitness,
    /// Step 2: the supplied coins or source are not the construction's.
    CoinsChanged,
}
impl fmt::Display for FinalizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Split finalization refused: {self:?}")
    }
}
impl std::error::Error for FinalizeError {}

/// A finalized step 1 with verified witnesses. Cryptographic evidence only: it
/// does not prove relay acceptance, RDTS activity, inclusion, confirmation
/// depth or reorg safety, and it grants no step-2 authority.
#[derive(Debug)]
pub struct VerifiedSplitStep1 {
    transaction: Transaction,
    chain: ChainId,
    construction_txid: Txid,
    fee: Amount,
    signatures_per_input: Vec<usize>,
}
impl VerifiedSplitStep1 {
    pub fn transaction(&self) -> &Transaction {
        &self.transaction
    }
    pub fn chain(&self) -> ChainId {
        self.chain
    }
    /// The unsigned construction's txid (see [`SplitStep1::txid`]); track
    /// `transaction().compute_txid()` on chain.
    pub fn construction_txid(&self) -> Txid {
        self.construction_txid
    }
    pub fn fee(&self) -> Amount {
        self.fee
    }
    pub fn vsize(&self) -> usize {
        self.transaction.vsize()
    }
    pub fn signatures_per_input(&self) -> &[usize] {
        &self.signatures_per_input
    }
}

/// Accept only an unfinalized partial-signature PSBT of the exact opaque
/// construction. Everything except `partial_sigs` and an absent or explicit
/// `SIGHASH_ALL` request must be identical; signers that strip metadata must
/// merge their signatures back into the exact PSBT. Every supplied signature
/// is verified, not only those the final witness keeps. Miniscript's finalizer
/// and interpreter then build and replay the actual witness.
pub fn finalize_split_step1<C: secp256k1::Verification>(
    construction: &SplitStep1,
    signed: &Psbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedSplitStep1, FinalizeError> {
    let original = &construction.psbt;
    check_signed_construction(original, signed)?;
    let prevouts =
        verify_partial_signatures(&construction.source, &construction.inputs, signed, secp)?;
    let total = prevouts
        .iter()
        .try_fold(0u64, |sum, output| sum.checked_add(output.value.to_sat()))
        .ok_or(FinalizeError::Economics)?;
    let value = signed.unsigned_tx.output[1].value.to_sat();
    check_economics(
        total,
        value,
        &signed.unsigned_tx.output[1].script_pubkey,
        construction.maximum_signed_vbytes,
    )
    .map_err(|_| FinalizeError::Economics)?;
    let (transaction, signatures_per_input) =
        finalize_and_replay(original, signed, &prevouts, secp)?;
    Ok(VerifiedSplitStep1 {
        construction_txid: original.unsigned_tx.compute_txid(),
        transaction,
        chain: construction.chain,
        fee: Amount::from_sat(total - value),
        signatures_per_input,
    })
}

/// The signed PSBT must be the exact construction plus `partial_sigs`, with
/// every input request absent or `SIGHASH_ALL` and every signature byte
/// `SIGHASH_ALL`. Shared by both steps.
fn check_signed_construction(original: &Psbt, signed: &Psbt) -> Result<(), FinalizeError> {
    if signed.unsigned_tx != original.unsigned_tx
        || signed.inputs.len() != original.inputs.len()
        || signed.outputs.len() != original.outputs.len()
    {
        return Err(FinalizeError::ConstructionChanged);
    }
    let mut normalized = signed.clone();
    for (index, input) in signed.inputs.iter().enumerate() {
        if input
            .sighash_type
            .is_some_and(|s| s.ecdsa_hash_ty() != Ok(EcdsaSighashType::All))
            || input
                .partial_sigs
                .values()
                .any(|s| s.sighash_type != EcdsaSighashType::All)
        {
            return Err(FinalizeError::UnsupportedSighash);
        }
        normalized.inputs[index].partial_sigs.clear();
        normalized.inputs[index].sighash_type = original.inputs[index].sighash_type;
    }
    if normalized != *original {
        return Err(FinalizeError::ConstructionChanged);
    }
    Ok(())
}

/// Authenticate each input's previous output against the construction's own
/// derivation, then verify every supplied partial signature (surplus ones
/// included): by a key the input commits to, compressed on segwit, and valid
/// over the `SIGHASH_ALL` digest. Returns the authenticated previous outputs.
fn verify_partial_signatures<C: secp256k1::Verification>(
    source: &SplitSource,
    derivations: &[(SplitBranch, u32)],
    signed: &Psbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<Vec<TxOut>, FinalizeError> {
    let mut prevouts = Vec::with_capacity(signed.inputs.len());
    let mut cache = SighashCache::new(&signed.unsigned_tx);
    for (index, input) in signed.inputs.iter().enumerate() {
        let output = spend::authenticate_previous_output(
            &signed.unsigned_tx.input[index].previous_output,
            input.non_witness_utxo.as_ref(),
            input.witness_utxo.as_ref(),
        )
        .map_err(|_| FinalizeError::InputAuthentication)?;
        let (branch, derivation) = derivations[index];
        let definite = source
            .derive(branch, derivation)
            .map_err(|_| FinalizeError::InputAuthentication)?;
        if definite.script_pubkey() != output.script_pubkey {
            return Err(FinalizeError::InputAuthentication);
        }
        let digest = sighash_all(&mut cache, index, &definite, input, &output)
            .ok_or(FinalizeError::InvalidSignature { input: index })?;
        let message = secp256k1::Message::from_digest(digest);
        let segwit = is_segwit(&definite);
        for (key, signature) in &input.partial_sigs {
            if !input.bip32_derivation.contains_key(&key.inner)
                || (segwit && !key.compressed)
                || secp
                    .verify_ecdsa(&message, &signature.signature, &key.inner)
                    .is_err()
            {
                return Err(FinalizeError::InvalidSignature { input: index });
            }
        }
        prevouts.push(output);
    }
    Ok(prevouts)
}

/// Miniscript's finalizer and extractor, then an interpreter replay of every
/// retained witness. Returns the transaction and signatures per input.
fn finalize_and_replay<C: secp256k1::Verification>(
    original: &Psbt,
    signed: &Psbt,
    prevouts: &[TxOut],
    secp: &secp256k1::Secp256k1<C>,
) -> Result<(Transaction, Vec<usize>), FinalizeError> {
    let mut finalized = signed.clone();
    finalized
        .finalize_mut(secp)
        .map_err(|_| FinalizeError::Unsatisfied)?;
    // extract() also runs the library interpreter; no unchecked extraction.
    let transaction = finalized
        .extract(secp)
        .map_err(|_| FinalizeError::InvalidWitness)?;
    let signatures_per_input = verify_retained_witness(&transaction, original, prevouts, secp)?;
    Ok((transaction, signatures_per_input))
}

/// The `SIGHASH_ALL` digest for one input of a supported shape, with the
/// scriptCode chosen from the construction's own descriptor.
fn sighash_all(
    cache: &mut SighashCache<&Transaction>,
    index: usize,
    definite: &Descriptor<DefiniteDescriptorKey>,
    input: &bitcoin::psbt::Input,
    output: &TxOut,
) -> Option<[u8; 32]> {
    let all = EcdsaSighashType::All;
    match definite {
        Descriptor::Pkh(_) => cache
            .legacy_signature_hash(index, &output.script_pubkey, all.to_u32())
            .ok()
            .map(|hash| hash.to_byte_array()),
        Descriptor::Wpkh(_) => cache
            .p2wpkh_signature_hash(index, &output.script_pubkey, output.value, all)
            .ok()
            .map(|hash| hash.to_byte_array()),
        Descriptor::Sh(_) => {
            let redeem = input.redeem_script.as_ref()?;
            if !redeem.is_p2wpkh() || redeem.to_p2sh() != output.script_pubkey {
                return None;
            }
            cache
                .p2wpkh_signature_hash(index, redeem, output.value, all)
                .ok()
                .map(|hash| hash.to_byte_array())
        }
        Descriptor::Wsh(_) => {
            let script = input.witness_script.as_ref()?;
            if script.to_p2wsh() != output.script_pubkey {
                return None;
            }
            cache
                .p2wsh_signature_hash(index, script, output.value, all)
                .ok()
                .map(|hash| hash.to_byte_array())
        }
        Descriptor::Bare(_) | Descriptor::Tr(_) => None,
    }
}

/// Replay each finalized input through Miniscript's interpreter and count the
/// `SIGHASH_ALL` signatures by construction keys it actually used.
fn verify_retained_witness<C: secp256k1::Verification>(
    transaction: &Transaction,
    original: &Psbt,
    prevouts: &[TxOut],
    secp: &secp256k1::Secp256k1<C>,
) -> Result<Vec<usize>, FinalizeError> {
    let mut unsigned = transaction.clone();
    for input in &mut unsigned.input {
        input.script_sig = ScriptBuf::new();
        input.witness.clear();
    }
    if unsigned != original.unsigned_tx {
        return Err(FinalizeError::ConstructionChanged);
    }
    let mut counts = Vec::with_capacity(transaction.input.len());
    for (index, input) in transaction.input.iter().enumerate() {
        let interpreter = Interpreter::from_txdata(
            &prevouts[index].script_pubkey,
            &input.script_sig,
            &input.witness,
            input.sequence,
            transaction.lock_time,
        )
        .map_err(|_| FinalizeError::InvalidWitness)?;
        let mut signatures = 0;
        for constraint in interpreter.iter(secp, transaction, index, &Prevouts::All(prevouts)) {
            let pair = match constraint.map_err(|_| FinalizeError::InvalidWitness)? {
                SatisfiedConstraint::PublicKey { key_sig }
                | SatisfiedConstraint::PublicKeyHash { key_sig, .. } => Some(key_sig),
                _ => None,
            };
            if let Some(pair) = pair {
                match pair {
                    KeySigPair::Ecdsa(key, sig)
                        if sig.sighash_type == EcdsaSighashType::All
                            && original.inputs[index]
                                .bip32_derivation
                                .contains_key(&key.inner) =>
                    {
                        signatures += 1
                    }
                    _ => return Err(FinalizeError::InvalidWitness),
                }
            }
        }
        if signatures == 0 {
            return Err(FinalizeError::InvalidWitness);
        }
        counts.push(signatures);
    }
    Ok(counts)
}

mod step2;
pub use step2::{
    create_split_step2, finalize_split_step2, reconstruct_split_step2, SplitStep2,
    SplitStep2Inputs, VerifiedSplitStep2,
};

#[cfg(test)]
#[path = "foreign_split/tests.rs"]
mod tests;
