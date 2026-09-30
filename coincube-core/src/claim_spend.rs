//! Unsigned owned-Vault Bitcoin poison self-transfers. No Claim authorization.
//!
//! The caller must reserve an unused change index and obtain current deployment,
//! chain and mempool observations separately. Construction proves neither input
//! exclusivity nor that RDTS is active. Do not sign or broadcast based on this
//! result alone. No seed, wallet storage or network operations occur here.

use std::{collections::BTreeSet, fmt};

use miniscript::bitcoin::{self, absolute::LockTime, bip32::ChildNumber, secp256k1};

use crate::{
    chain::ChainId,
    descriptors::CoincubeDescriptor,
    spend::{
        self, AddrInfo, CandidateCoin, SpendCreationError, SpendOutputAddress, SpendTxFees,
        TxGetter,
    },
    split_poison::{split_poison_fork_marker, split_poison_script},
};

#[derive(Debug)]
pub enum Error {
    InvalidRequest(&'static str),
    Spend(SpendCreationError),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(reason) => f.write_str(reason),
            Self::Spend(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for Error {}
impl From<SpendCreationError> for Error {
    fn from(error: SpendCreationError) -> Self {
        Self::Spend(error)
    }
}

/// Construction result with no public-field or deserialization bypass. This
/// certifies only the unsigned construction checks, not live spend permission.
#[derive(Debug)]
pub struct PoisonSelfTransfer {
    psbt: bitcoin::psbt::Psbt,
    descriptor: CoincubeDescriptor,
    chain: ChainId,
    change_index: ChildNumber,
    warnings: Vec<spend::CreateSpendWarning>,
}
impl PoisonSelfTransfer {
    pub fn psbt(&self) -> &bitcoin::psbt::Psbt {
        &self.psbt
    }
    pub fn descriptor(&self) -> &CoincubeDescriptor {
        &self.descriptor
    }
    pub fn chain(&self) -> ChainId {
        self.chain
    }
    pub fn change_index(&self) -> ChildNumber {
        self.change_index
    }
    pub fn warnings(&self) -> &[spend::CreateSpendWarning] {
        &self.warnings
    }
}

/// Construct a native P2WSH self-sweep of exactly `coins` on Bitcoin/mainnet
/// or Bitcoin/testnet4. The destination is derived from the same descriptor's
/// change branch; arbitrary recipient addresses/scripts cannot be supplied.
/// `change_index` must be reserved as fresh by the wallet controller. This pure
/// function only rejects reuse of a selected source script, not historical use.
///
/// `fork_marker` identifies the intended observed fork anchor in the payload;
/// it is caller-supplied labeling, NOT authenticated chain evidence. The output
/// is a deterministic 90-byte OP_RETURN script with zero value, charged in coin
/// selection before any signature exists. All previous transactions are checked
/// through the ordinary spend builder. Normal Bitcoin SIGHASH_ALL signing is
/// a later step; this function neither signs nor creates split evidence.
#[allow(clippy::too_many_arguments)]
pub fn create_poison_self_transfer(
    chain: ChainId,
    descriptor: &CoincubeDescriptor,
    secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    tx_getter: &mut impl TxGetter,
    coins: &[CandidateCoin],
    change_index: ChildNumber,
    feerate_vb: u64,
    locktime: LockTime,
    fork_marker: bitcoin::BlockHash,
) -> Result<PoisonSelfTransfer, Error> {
    create_self_transfer(
        chain,
        descriptor,
        secp,
        tx_getter,
        coins,
        change_index,
        feerate_vb,
        locktime,
        Some(fork_marker),
    )
}

#[allow(clippy::too_many_arguments)]
fn create_self_transfer(
    chain: ChainId,
    descriptor: &CoincubeDescriptor,
    secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    tx_getter: &mut impl TxGetter,
    coins: &[CandidateCoin],
    change_index: ChildNumber,
    feerate_vb: u64,
    locktime: LockTime,
    fork_marker: Option<bitcoin::BlockHash>,
) -> Result<PoisonSelfTransfer, Error> {
    let network = match chain {
        ChainId::Bitcoin => bitcoin::Network::Bitcoin,
        ChainId::Testnet4 => bitcoin::Network::Testnet4,
        _ => {
            return Err(Error::InvalidRequest(
                "Bitcoin mainnet or testnet4 source required",
            ))
        }
    };
    if descriptor.is_taproot() || change_index.is_hardened() || coins.is_empty() {
        return Err(Error::InvalidRequest(
            "Native P2WSH, nonempty inputs and a normal change index required",
        ));
    }
    let destination = descriptor.change_descriptor().derive(change_index, secp);
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    for coin in coins {
        if coin.outpoint.is_null() || !seen.insert(coin.outpoint) || coin.deriv_index.is_hardened()
        {
            return Err(Error::InvalidRequest(
                "Unique non-null outpoints and normal derivation indices required",
            ));
        }
        total = total
            .checked_add(coin.amount.to_sat())
            .filter(|n| *n <= bitcoin::Amount::MAX_MONEY.to_sat())
            .ok_or(Error::InvalidRequest(
                "Input total exceeds Bitcoin money range",
            ))?;
        let branch = if coin.is_change {
            descriptor.change_descriptor()
        } else {
            descriptor.receive_descriptor()
        };
        if branch.derive(coin.deriv_index, secp).script_pubkey() == destination.script_pubkey() {
            return Err(Error::InvalidRequest(
                "Change must not reuse a selected source script",
            ));
        }
    }
    let poison = fork_marker
        .map(|fork_marker| {
            split_poison_script(chain, fork_marker, &seen).ok_or(Error::InvalidRequest(
                "Bitcoin mainnet or testnet4 source required",
            ))
        })
        .transpose()?;
    let selected: Vec<_> = coins
        .iter()
        .map(|coin| CandidateCoin {
            must_select: true,
            ..*coin
        })
        .collect();
    let result = spend::create_spend_with_poison(
        descriptor,
        secp,
        tx_getter,
        &[],
        &selected,
        SpendTxFees::Regular(feerate_vb),
        SpendOutputAddress {
            addr: destination.address(network),
            info: Some(AddrInfo {
                index: change_index,
                is_change: true,
            }),
        },
        locktime,
        poison,
    )?;
    Ok(PoisonSelfTransfer {
        psbt: result.psbt,
        descriptor: descriptor.clone(),
        chain,
        change_index,
        warnings: result.warnings,
    })
}

/// Unsigned owned construction using one structurally verified ancestry input.
/// This type certifies neither exclusivity nor live eligibility. It deliberately
/// cannot be passed to the OP_RETURN signing workflow as PoisonSelfTransfer.
#[derive(Debug)]
pub struct AncestrySelfTransfer {
    transfer: PoisonSelfTransfer,
    poison_input: bitcoin::OutPoint,
    poison_derivation: (ChildNumber, bool),
    claimed_prevouts: Vec<bitcoin::OutPoint>,
}
impl AncestrySelfTransfer {
    pub fn psbt(&self) -> &bitcoin::psbt::Psbt {
        self.transfer.psbt()
    }
    pub fn descriptor(&self) -> &CoincubeDescriptor {
        self.transfer.descriptor()
    }
    pub fn chain(&self) -> ChainId {
        self.transfer.chain()
    }
    pub fn change_index(&self) -> ChildNumber {
        self.transfer.change_index()
    }
    pub fn warnings(&self) -> &[spend::CreateSpendWarning] {
        self.transfer.warnings()
    }
    pub fn poison_input(&self) -> bitcoin::OutPoint {
        self.poison_input
    }
    /// Owned selected-input index and change branch, authenticated by construction.
    /// Persisted copies are hints: rederive the script before restoring metadata.
    pub fn poison_derivation(&self) -> (ChildNumber, bool) {
        self.poison_derivation
    }
    pub fn claimed_prevouts(&self) -> &[bitcoin::OutPoint] {
        &self.claimed_prevouts
    }
}

/// Build a Bitcoin self-transfer with a designated ancestry input and no
/// OP_RETURN output. All inputs are authenticated against the owned descriptor
/// by the ordinary spend builder. The designated input is excluded from the
/// fork claim set, which must remain nonempty. The caller must establish fresh
/// positive chain exclusivity, maturity, spend policy and reservations before
/// signing; a structural dependency alone is insufficient. Mainnet only, matching
/// the current ancestry qualification contract. No signing or broadcast occurs.
#[allow(clippy::too_many_arguments)]
pub fn create_ancestry_self_transfer(
    chain: ChainId,
    descriptor: &CoincubeDescriptor,
    secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    tx_getter: &mut impl TxGetter,
    coins: &[CandidateCoin],
    change_index: ChildNumber,
    feerate_vb: u64,
    locktime: LockTime,
    dependency: &crate::claim_ancestry::CoinbaseDependency,
) -> Result<AncestrySelfTransfer, Error> {
    let poison_input = dependency.selected();
    if chain != ChainId::Bitcoin
        || coins.len() < 2
        || coins
            .iter()
            .filter(|coin| coin.outpoint == poison_input)
            .count()
            != 1
    {
        return Err(Error::InvalidRequest(
            "Mainnet, one selected ancestry input and nonempty claimed inputs required",
        ));
    }
    let transfer = create_self_transfer(
        chain,
        descriptor,
        secp,
        tx_getter,
        coins,
        change_index,
        feerate_vb,
        locktime,
        None,
    )?;
    let claimed_prevouts = transfer
        .psbt()
        .unsigned_tx
        .input
        .iter()
        .map(|input| input.previous_output)
        .filter(|outpoint| *outpoint != poison_input)
        .collect();
    let selected = coins
        .iter()
        .find(|coin| coin.outpoint == poison_input)
        .ok_or(Error::InvalidRequest("Selected ancestry input is missing"))?;
    Ok(AncestrySelfTransfer {
        transfer,
        poison_input,
        poison_derivation: (selected.deriv_index, selected.is_change),
        claimed_prevouts,
    })
}

/// An unsigned BTCB2 self-sweep bound to the original Bitcoin poison inputs.
/// Construction is not proof of confirmation, exclusivity or broadcast authority.
#[derive(Debug)]
pub struct ClaimForkSweep {
    psbt: bitcoin::psbt::Psbt,
    chain: ChainId,
    bitcoin_step1: bitcoin::Txid,
    descriptor: CoincubeDescriptor,
    change_index: ChildNumber,
    warnings: Vec<spend::CreateSpendWarning>,
}
impl ClaimForkSweep {
    pub fn psbt(&self) -> &bitcoin::psbt::Psbt {
        &self.psbt
    }
    pub fn chain(&self) -> ChainId {
        self.chain
    }
    pub fn bitcoin_step1(&self) -> bitcoin::Txid {
        self.bitcoin_step1
    }
    pub fn descriptor(&self) -> &CoincubeDescriptor {
        &self.descriptor
    }
    pub fn change_index(&self) -> ChildNumber {
        self.change_index
    }
    pub fn warnings(&self) -> &[spend::CreateSpendWarning] {
        &self.warnings
    }
}

/// Build Claim's fork-side sweep of exactly the original shared inputs, not
/// the outputs created by the Bitcoin self-transfer. This entry point uses the
/// OP_RETURN construction, whose inputs are all included in the fork claim.
///
/// `coins` and `tx_getter` must be collected from the authenticated fork wallet.
/// Values/scripts are authenticated again through the ordinary spend builder
/// and compared with the checked Bitcoin construction. The caller reserves the
/// fresh target change index. This function performs no chain queries and does
/// not establish unspentness, confirmation depth, poison validity or permission
/// to sign. The signing coordinator chooses the permitted sighash policy later.
#[allow(clippy::too_many_arguments)]
pub fn create_claim_fork_sweep(
    source: &PoisonSelfTransfer,
    fork_chain: ChainId,
    secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    tx_getter: &mut impl TxGetter,
    coins: &[CandidateCoin],
    change_index: ChildNumber,
    feerate_vb: u64,
    locktime: LockTime,
) -> Result<ClaimForkSweep, Error> {
    create_fork_sweep(
        source,
        None,
        fork_chain,
        secp,
        tx_getter,
        coins,
        change_index,
        feerate_vb,
        locktime,
    )
}

/// Construct only the shared-input fork sweep. The ancestry input is excluded
/// by the opaque Bitcoin construction, never by a caller-supplied exclusion.
/// Fresh qualification and signing permission are still separate requirements.
#[allow(clippy::too_many_arguments)]
pub fn create_ancestry_fork_sweep(
    source: &AncestrySelfTransfer,
    fork_chain: ChainId,
    secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    tx_getter: &mut impl TxGetter,
    coins: &[CandidateCoin],
    change_index: ChildNumber,
    feerate_vb: u64,
    locktime: LockTime,
) -> Result<ClaimForkSweep, Error> {
    create_fork_sweep(
        &source.transfer,
        Some(source.poison_input),
        fork_chain,
        secp,
        tx_getter,
        coins,
        change_index,
        feerate_vb,
        locktime,
    )
}

#[allow(clippy::too_many_arguments)]
fn create_fork_sweep(
    source: &PoisonSelfTransfer,
    excluded_input: Option<bitcoin::OutPoint>,
    fork_chain: ChainId,
    secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    tx_getter: &mut impl TxGetter,
    coins: &[CandidateCoin],
    change_index: ChildNumber,
    feerate_vb: u64,
    locktime: LockTime,
) -> Result<ClaimForkSweep, Error> {
    if !matches!(
        (source.chain(), fork_chain),
        (ChainId::Bitcoin, ChainId::BitcoinBlake2b)
            | (ChainId::Testnet4, ChainId::BitcoinBlake2bTestnet4)
    ) || change_index.is_hardened()
    {
        return Err(Error::InvalidRequest(
            "Matching fork chain and normal change index required",
        ));
    }
    let original = source.psbt();
    let selected: BTreeSet<_> = coins.iter().map(|coin| coin.outpoint).collect();
    let claimed: BTreeSet<_> = original
        .unsigned_tx
        .input
        .iter()
        .map(|input| input.previous_output)
        .filter(|outpoint| Some(*outpoint) != excluded_input)
        .collect();
    if coins.is_empty() || selected.len() != coins.len() || selected != claimed {
        return Err(Error::InvalidRequest(
            "Fork sweep must spend exactly the original claimed inputs",
        ));
    }
    let descriptor = source.descriptor();
    let destination = descriptor.change_descriptor().derive(change_index, secp);
    for coin in coins {
        let source_index = original
            .unsigned_tx
            .input
            .iter()
            .position(|input| input.previous_output == coin.outpoint)
            .ok_or(Error::InvalidRequest("Unclaimed fork input"))?;
        let prevout =
            original.inputs[source_index]
                .witness_utxo
                .as_ref()
                .ok_or(Error::InvalidRequest(
                    "Missing authenticated source prevout",
                ))?;
        if coin.deriv_index.is_hardened() || coin.amount != prevout.value {
            return Err(Error::InvalidRequest(
                "Fork input metadata differs from Bitcoin construction",
            ));
        }
        let branch = if coin.is_change {
            descriptor.change_descriptor()
        } else {
            descriptor.receive_descriptor()
        };
        if branch.derive(coin.deriv_index, secp).script_pubkey() != prevout.script_pubkey
            || destination.script_pubkey() == prevout.script_pubkey
        {
            return Err(Error::InvalidRequest(
                "Fork input ownership or fresh destination mismatch",
            ));
        }
    }
    let selected: Vec<_> = coins
        .iter()
        .map(|coin| CandidateCoin {
            must_select: true,
            ..*coin
        })
        .collect();
    let built = spend::create_spend(
        descriptor,
        secp,
        tx_getter,
        &[],
        &selected,
        SpendTxFees::Regular(feerate_vb),
        SpendOutputAddress {
            addr: destination.address(fork_chain.bitcoin_network()),
            info: Some(AddrInfo {
                index: change_index,
                is_change: true,
            }),
        },
        locktime,
    )?;
    Ok(ClaimForkSweep {
        psbt: built.psbt,
        chain: fork_chain,
        bitcoin_step1: original.unsigned_tx.compute_txid(),
        descriptor: descriptor.clone(),
        change_index,
        warnings: built.warnings,
    })
}

/// Recover an exact recorded fork sweep from authenticated wallet inputs.
/// This rebuilds all PSBT signing metadata rather than trusting persisted PSBT
/// fields. It reserves no address and grants no signing or broadcast authority;
/// callers must still bind the result to the journal and obtain fresh checks.
/// Only the recorded output amount is retained, allowing recovery without the
/// original fee estimate. Actual economics are reverified before returning.
#[allow(clippy::too_many_arguments)]
pub fn reconstruct_claim_fork_sweep(
    source: &PoisonSelfTransfer,
    fork_chain: ChainId,
    secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    tx_getter: &mut impl TxGetter,
    coins: &[CandidateCoin],
    change_index: ChildNumber,
    recorded: &bitcoin::Transaction,
) -> Result<ClaimForkSweep, Error> {
    reconstruct_fork_sweep(
        source,
        None,
        fork_chain,
        secp,
        tx_getter,
        coins,
        change_index,
        recorded,
    )
}

/// Rebuild only the recorded shared-input sweep, retaining the ancestry
/// construction's bound exclusion. This restores no live eligibility.
#[allow(clippy::too_many_arguments)]
pub fn reconstruct_ancestry_fork_sweep(
    source: &AncestrySelfTransfer,
    fork_chain: ChainId,
    secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    tx_getter: &mut impl TxGetter,
    coins: &[CandidateCoin],
    change_index: ChildNumber,
    recorded: &bitcoin::Transaction,
) -> Result<ClaimForkSweep, Error> {
    reconstruct_fork_sweep(
        &source.transfer,
        Some(source.poison_input),
        fork_chain,
        secp,
        tx_getter,
        coins,
        change_index,
        recorded,
    )
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_fork_sweep(
    source: &PoisonSelfTransfer,
    excluded_input: Option<bitcoin::OutPoint>,
    fork_chain: ChainId,
    secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    tx_getter: &mut impl TxGetter,
    coins: &[CandidateCoin],
    change_index: ChildNumber,
    recorded: &bitcoin::Transaction,
) -> Result<ClaimForkSweep, Error> {
    if recorded.output.len() != 1 {
        return Err(Error::InvalidRequest(
            "Recorded fork sweep must have one output",
        ));
    }
    let mut rebuilt = create_fork_sweep(
        source,
        excluded_input,
        fork_chain,
        secp,
        tx_getter,
        coins,
        change_index,
        1,
        recorded.lock_time,
    )?;
    rebuilt.psbt.unsigned_tx.output[0].value = recorded.output[0].value;
    if rebuilt.psbt.unsigned_tx != *recorded {
        return Err(Error::InvalidRequest(
            "Recorded transaction differs from the owned fork construction",
        ));
    }
    spend::reverify_spend_before_broadcast(source.descriptor(), &rebuilt.psbt)?;
    Ok(rebuilt)
}

/// Rebuild an exact recorded ancestry self-transfer using newly authenticated
/// owned inputs and a reverified dependency. Only the recorded output amount
/// is retained; all scripts, inputs and PSBT metadata are reconstructed and
/// actual economics checked. The caller must bind the selected dependency,
/// change index and recorded transaction to its intent and freshly qualify the
/// path. Reconstruction neither reserves an address nor authorizes submission.
#[allow(clippy::too_many_arguments)]
pub fn reconstruct_ancestry_self_transfer(
    chain: ChainId,
    descriptor: &CoincubeDescriptor,
    secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    tx_getter: &mut impl TxGetter,
    coins: &[CandidateCoin],
    change_index: ChildNumber,
    dependency: &crate::claim_ancestry::CoinbaseDependency,
    recorded: &bitcoin::Transaction,
) -> Result<AncestrySelfTransfer, Error> {
    if recorded.output.len() != 1 {
        return Err(Error::InvalidRequest(
            "Recorded ancestry transfer must have one output",
        ));
    }
    let mut rebuilt = create_ancestry_self_transfer(
        chain,
        descriptor,
        secp,
        tx_getter,
        coins,
        change_index,
        1,
        recorded.lock_time,
        dependency,
    )?;
    rebuilt.transfer.psbt.unsigned_tx.output[0].value = recorded.output[0].value;
    if rebuilt.psbt().unsigned_tx != *recorded {
        return Err(Error::InvalidRequest(
            "Recorded transaction differs from the owned ancestry construction",
        ));
    }
    spend::reverify_spend_before_broadcast(descriptor, rebuilt.psbt())?;
    Ok(rebuilt)
}

/// Reconstruct an existing unsigned poison transfer from authenticated wallet
/// coins and previous transactions. This does not reserve another change index
/// or authorize a new submission. The caller must bind the result to its journal.
///
/// The ordinary builder rechecks ownership and recreates all signing metadata.
/// Only the recorded change amount is retained (the original fee estimate need
/// not be available after restart); the complete transaction must otherwise
/// match, and its actual economics are checked again before returning.
#[allow(clippy::too_many_arguments)]
pub fn reconstruct_poison_self_transfer(
    chain: ChainId,
    descriptor: &CoincubeDescriptor,
    secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    tx_getter: &mut impl TxGetter,
    coins: &[CandidateCoin],
    change_index: ChildNumber,
    recorded: &bitcoin::Transaction,
) -> Result<PoisonSelfTransfer, Error> {
    if recorded.output.len() != 2 {
        return Err(Error::InvalidRequest(
            "Recorded poison must have two outputs",
        ));
    }
    let marker = split_poison_fork_marker(&recorded.output[0].script_pubkey)
        .ok_or(Error::InvalidRequest("Recorded poison payload is invalid"))?;
    let mut rebuilt = create_poison_self_transfer(
        chain,
        descriptor,
        secp,
        tx_getter,
        coins,
        change_index,
        1,
        recorded.lock_time,
        marker,
    )?;
    // Never restore arbitrary output scripts, input metadata or caller PSBTs.
    // Rebuilding also checks the payload's chain byte and input commitment.
    rebuilt.psbt.unsigned_tx.output[1].value = recorded.output[1].value;
    if rebuilt.psbt.unsigned_tx != *recorded {
        return Err(Error::InvalidRequest(
            "Recorded transaction differs from the owned poison construction",
        ));
    }
    spend::reverify_spend_before_broadcast(descriptor, &rebuilt.psbt)?;
    Ok(rebuilt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::{
        hashes::{sha256, Hash},
        Amount, OutPoint, Transaction, TxIn, TxOut,
    };
    use std::{collections::HashMap, str::FromStr};
    const WSH_DESC: &str = "wsh(or_d(multi(1,[573fb35b/48'/1'/0'/2']tpubDFKp9T7WAYDcENSjoifkrpq1gMDF47KGJcJrpxzX23Qor8wuGbrEVs9utNq1MDS8E2WXJSBk1qoPQLpwyokW7DiUNPwFuxQkL7owNkLAb9W/<0;1>/*,[573fb35c/48'/1'/1'/2']tpubDFGezyzuHJPhdP3jHGW7v7Hwes4Hihqv5W2yyCmRY9VZJCRchETvxrMC8uECeJZdxQ14V4iD4DecoArkUSDwj8ogYE9WEv4MNZr12thNHCs/<0;1>/*),and_v(v:multi(2,[573fb35b/48'/1'/2'/2']tpubDDwxQauiaU964vPzt5Vd7jnDHEUtp2Vc34PaWpEXg5TQ3bRccxnc1MKKh88Hi7xiMeZo9Tm6fBcq4UGXqnDtGUniJLjqAD8SjQ8Eci3aSR7/<0;1>/*,[573fb35c/48'/1'/3'/2']tpubDE37XAVB5CQ1x85md3BQ5uHCoMwT5fgT8X13zzCUQ3x5o2jskYxKjj7Qcxt1Jpj4QB8tqspn2dooPCekRuQDYrDHov7J1ueUNu2wcvgRDxr/<0;1>/*),older(1000))))#fccaqlhh";
    const TR_DESC: &str = "tr(tpubD6NzVbkrYhZ4YdBUPkUhDYj6Sd1QK8vgiCf5RwHnAnSNK5ozemAZzPTYZbgQq4diod7oxFJJYGa8FNRHzRo7URkixzQTuudh38xRRdSc4Hu/<0;1>/*,{and_v(v:multi_a(1,[ffd63c8d/48'/1'/0'/2']tpubDExA3EC3iAsPxPhFn4j6gMiVup6V2eH3qKyk69RcTc9TTNRfFYVPad8bJD5FCHVQxyBT4izKsvr7Btd2R4xmQ1hZkvsqGBaeE82J71uTK4N/<2;3>/*,[da2ee873/48'/1'/0'/2']tpubDEbXY6RbN9mxAvQW797WxReGGkrdyRfdYcehVVaQQcQ3kyfhxSMcnU9qGpUVRHXXALvBtc99jcuxx5tkzcLaJbAukSNpP9h2ti4XFRosv1g/<2;3>/*),older(2)),multi_a(2,[ffd63c8d/48'/1'/0'/2']tpubDExA3EC3iAsPxPhFn4j6gMiVup6V2eH3qKyk69RcTc9TTNRfFYVPad8bJD5FCHVQxyBT4izKsvr7Btd2R4xmQ1hZkvsqGBaeE82J71uTK4N/<0;1>/*,[da2ee873/48'/1'/0'/2']tpubDEbXY6RbN9mxAvQW797WxReGGkrdyRfdYcehVVaQQcQ3kyfhxSMcnU9qGpUVRHXXALvBtc99jcuxx5tkzcLaJbAukSNpP9h2ti4XFRosv1g/<0;1>/*)})";
    struct Getter(HashMap<bitcoin::Txid, Transaction>);
    impl TxGetter for Getter {
        fn get_tx(&mut self, id: &bitcoin::Txid) -> Option<Transaction> {
            self.0.get(id).cloned()
        }
    }
    fn fixture() -> (CoincubeDescriptor, Vec<CandidateCoin>, Getter) {
        let desc = CoincubeDescriptor::from_str(WSH_DESC).unwrap();
        let secp = secp256k1::Secp256k1::verification_only();
        let mut coins = Vec::new();
        let mut txs = HashMap::new();
        for i in 0..2 {
            let index = ChildNumber::from_normal_idx(i).unwrap();
            let tx = Transaction {
                version: bitcoin::transaction::Version::TWO,
                lock_time: LockTime::ZERO,
                input: vec![TxIn::default()],
                output: vec![TxOut {
                    value: Amount::from_sat(100_000),
                    script_pubkey: desc
                        .receive_descriptor()
                        .derive(index, &secp)
                        .script_pubkey(),
                }],
            };
            coins.push(CandidateCoin {
                outpoint: OutPoint::new(tx.compute_txid(), 0),
                amount: tx.output[0].value,
                deriv_index: index,
                is_change: false,
                must_select: false,
                sequence: None,
                ancestor_info: None,
            });
            txs.insert(tx.compute_txid(), tx);
        }
        (desc, coins, Getter(txs))
    }
    fn build(
        chain: ChainId,
        desc: &CoincubeDescriptor,
        coins: &[CandidateCoin],
        getter: &mut Getter,
        index: u32,
        fee: u64,
    ) -> Result<PoisonSelfTransfer, Error> {
        create_poison_self_transfer(
            chain,
            desc,
            &secp256k1::Secp256k1::verification_only(),
            getter,
            coins,
            ChildNumber::from_normal_idx(index).unwrap(),
            fee,
            LockTime::ZERO,
            bitcoin::BlockHash::from_byte_array([42; 32]),
        )
    }
    #[test]
    fn ancestry_construction_owns_every_input_and_excludes_poison_from_fork_claim() {
        let (desc, coins, mut getter) = fixture();
        let selected = coins[0].outpoint;
        let raw = bitcoin::consensus::serialize(getter.0.get(&selected.txid).unwrap());
        let dependency = crate::claim_ancestry::verify(
            selected,
            &[crate::claim_ancestry::Link {
                transaction: &raw,
                parent_input: None,
            }],
        )
        .unwrap();
        let secp = secp256k1::Secp256k1::verification_only();
        let index = ChildNumber::from_normal_idx(10).unwrap();
        let built = create_ancestry_self_transfer(
            ChainId::Bitcoin,
            &desc,
            &secp,
            &mut getter,
            &coins,
            index,
            5,
            LockTime::ZERO,
            &dependency,
        )
        .unwrap();
        assert_eq!(built.poison_input(), selected);
        assert_eq!(
            built.poison_derivation(),
            (coins[0].deriv_index, coins[0].is_change)
        );
        assert_eq!(built.claimed_prevouts(), &[coins[1].outpoint]);
        assert_eq!(built.psbt().unsigned_tx.input.len(), 2);
        assert_eq!(
            built
                .psbt()
                .unsigned_tx
                .input
                .iter()
                .map(|input| input.previous_output)
                .collect::<BTreeSet<_>>(),
            coins.iter().map(|coin| coin.outpoint).collect()
        );
        assert_eq!(built.psbt().unsigned_tx.output.len(), 1);
        assert_eq!(
            built.psbt().unsigned_tx.output[0].script_pubkey,
            desc.change_descriptor()
                .derive(index, &secp)
                .script_pubkey()
        );
        assert!(built
            .psbt()
            .inputs
            .iter()
            .all(|input| input.partial_sigs.is_empty()
                && input.witness_utxo.is_some()
                && input.non_witness_utxo.is_some()));
        assert!(
            200_000 - built.psbt().unsigned_tx.output[0].value.to_sat()
                >= desc.unsigned_tx_max_vbytes(&built.psbt().unsigned_tx, true) * 5
        );
        spend::reverify_spend_before_broadcast(&desc, built.psbt()).unwrap();
        // No fork lookup of the exclusive input is needed or allowed by this
        // construction: its source transaction is absent from the fork getter.
        let removed = getter.0.remove(&selected.txid).unwrap();
        let fork = create_ancestry_fork_sweep(
            &built,
            ChainId::BitcoinBlake2b,
            &secp,
            &mut getter,
            &coins[1..],
            index,
            5,
            LockTime::ZERO,
        )
        .unwrap();
        assert_eq!(
            fork.bitcoin_step1(),
            built.psbt().unsigned_tx.compute_txid()
        );
        assert_eq!(
            fork.psbt()
                .unsigned_tx
                .input
                .iter()
                .map(|input| input.previous_output)
                .collect::<Vec<_>>(),
            built.claimed_prevouts()
        );
        assert_eq!(fork.psbt().unsigned_tx.output.len(), 1);
        spend::reverify_spend_before_broadcast(&desc, fork.psbt()).unwrap();
        for invalid in [
            coins.clone(),
            vec![coins[0]],
            vec![],
            vec![coins[1], coins[1]],
        ] {
            assert!(create_ancestry_fork_sweep(
                &built,
                ChainId::BitcoinBlake2b,
                &secp,
                &mut getter,
                &invalid,
                index,
                5,
                LockTime::ZERO
            )
            .is_err());
        }
        let mut wrong_fork = vec![coins[1]];
        wrong_fork[0].amount = Amount::from_sat(99_999);
        assert!(create_ancestry_fork_sweep(
            &built,
            ChainId::BitcoinBlake2b,
            &secp,
            &mut getter,
            &wrong_fork,
            index,
            5,
            LockTime::ZERO
        )
        .is_err());
        getter.0.insert(selected.txid, removed);
        for chain in [ChainId::Testnet4, ChainId::BitcoinBlake2b] {
            assert!(create_ancestry_self_transfer(
                chain,
                &desc,
                &secp,
                &mut getter,
                &coins,
                index,
                5,
                LockTime::ZERO,
                &dependency
            )
            .is_err());
        }
        for invalid in [vec![coins[0]], vec![coins[1]], vec![coins[0], coins[0]]] {
            assert!(create_ancestry_self_transfer(
                ChainId::Bitcoin,
                &desc,
                &secp,
                &mut getter,
                &invalid,
                index,
                5,
                LockTime::ZERO,
                &dependency
            )
            .is_err());
        }
        let mut reused = coins.clone();
        let mut reused_tx = getter.0.get(&coins[1].outpoint.txid).unwrap().clone();
        reused_tx.output[0].script_pubkey = desc
            .change_descriptor()
            .derive(index, &secp)
            .script_pubkey();
        reused[1].outpoint.txid = reused_tx.compute_txid();
        reused[1].is_change = true;
        reused[1].deriv_index = index;
        getter.0.insert(reused_tx.compute_txid(), reused_tx);
        assert!(matches!(
            create_ancestry_self_transfer(
                ChainId::Bitcoin,
                &desc,
                &secp,
                &mut getter,
                &reused,
                index,
                5,
                LockTime::ZERO,
                &dependency
            ),
            Err(Error::InvalidRequest(
                "Change must not reuse a selected source script"
            ))
        ));
        for change in [true, false] {
            let mut bad = coins.clone();
            if change {
                bad[0].amount = Amount::from_sat(99_999);
            } else {
                bad[1].deriv_index = ChildNumber::from_normal_idx(99).unwrap();
            }
            assert!(matches!(
                create_ancestry_self_transfer(
                    ChainId::Bitcoin,
                    &desc,
                    &secp,
                    &mut getter,
                    &bad,
                    index,
                    5,
                    LockTime::ZERO,
                    &dependency
                ),
                Err(Error::Spend(SpendCreationError::InputAuthentication(..)))
            ));
        }
    }

    #[test]
    fn ancestry_restart_rebuilds_metadata_and_refuses_replacement_or_bad_economics() {
        let (desc, coins, mut getter) = fixture();
        let raw = bitcoin::consensus::serialize(getter.0.get(&coins[0].outpoint.txid).unwrap());
        let dependency = crate::claim_ancestry::verify(
            coins[0].outpoint,
            &[crate::claim_ancestry::Link {
                transaction: &raw,
                parent_input: None,
            }],
        )
        .unwrap();
        let secp = secp256k1::Secp256k1::verification_only();
        let index = ChildNumber::from_normal_idx(10).unwrap();
        let built = create_ancestry_self_transfer(
            ChainId::Bitcoin,
            &desc,
            &secp,
            &mut getter,
            &coins,
            index,
            5,
            LockTime::ZERO,
            &dependency,
        )
        .unwrap();
        let original = built.psbt().unsigned_tx.clone();
        let restored = reconstruct_ancestry_self_transfer(
            ChainId::Bitcoin,
            &desc,
            &secp,
            &mut getter,
            &coins,
            index,
            &dependency,
            &original,
        )
        .unwrap();
        assert_eq!(restored.psbt(), built.psbt());
        assert_eq!(restored.claimed_prevouts(), built.claimed_prevouts());
        let fork = create_ancestry_fork_sweep(
            &restored,
            ChainId::BitcoinBlake2b,
            &secp,
            &mut getter,
            &coins[1..],
            index,
            5,
            LockTime::ZERO,
        )
        .unwrap();
        let fork_tx = fork.psbt().unsigned_tx.clone();
        let fork_restored = reconstruct_ancestry_fork_sweep(
            &restored,
            ChainId::BitcoinBlake2b,
            &secp,
            &mut getter,
            &coins[1..],
            index,
            &fork_tx,
        )
        .unwrap();
        assert_eq!(fork_restored.psbt(), fork.psbt());
        for fork_side in [false, true] {
            let recorded = if fork_side { &fork_tx } else { &original };
            for case in 0..7 {
                let mut bad = recorded.clone();
                match case {
                    0 => bad.output.clear(),
                    1 => bad.output[0].script_pubkey = bitcoin::ScriptBuf::new(),
                    2 => bad.version = bitcoin::transaction::Version::ONE,
                    3 => bad.input[0].sequence = bitcoin::Sequence::MAX,
                    4 => bad.output[0].value = Amount::ZERO,
                    5 => bad.output[0].value = Amount::from_sat(300_000),
                    _ => bad.input[0].previous_output = OutPoint::null(),
                }
                if fork_side {
                    assert!(reconstruct_ancestry_fork_sweep(
                        &restored,
                        ChainId::BitcoinBlake2b,
                        &secp,
                        &mut getter,
                        &coins[1..],
                        index,
                        &bad
                    )
                    .is_err());
                } else {
                    assert!(reconstruct_ancestry_self_transfer(
                        ChainId::Bitcoin,
                        &desc,
                        &secp,
                        &mut getter,
                        &coins,
                        index,
                        &dependency,
                        &bad
                    )
                    .is_err());
                }
            }
        }
        getter.0.clear();
        assert!(matches!(
            reconstruct_ancestry_self_transfer(
                ChainId::Bitcoin,
                &desc,
                &secp,
                &mut getter,
                &coins,
                index,
                &dependency,
                &original
            ),
            Err(Error::Spend(SpendCreationError::InputAuthentication(..)))
        ));
        assert!(matches!(
            reconstruct_ancestry_fork_sweep(
                &restored,
                ChainId::BitcoinBlake2b,
                &secp,
                &mut getter,
                &coins[1..],
                index,
                &fork_tx
            ),
            Err(Error::Spend(SpendCreationError::InputAuthentication(..)))
        ));
    }

    #[test]
    fn exact_owned_sweep_poison_and_fee_weight_on_both_chains() {
        for chain in [ChainId::Bitcoin, ChainId::Testnet4] {
            let (desc, coins, mut getter) = fixture();
            let built = build(chain, &desc, &coins, &mut getter, 10, 5).unwrap();
            let tx = &built.psbt.unsigned_tx;
            assert_eq!(
                tx.input
                    .iter()
                    .map(|i| i.previous_output)
                    .collect::<BTreeSet<_>>(),
                coins.iter().map(|c| c.outpoint).collect()
            );
            assert_eq!(tx.output.len(), 2);
            assert_eq!(tx.output[0].value, Amount::ZERO);
            assert!(tx.output[0].script_pubkey.is_op_return());
            assert_eq!(tx.output[0].script_pubkey.len(), 90);
            let secp = secp256k1::Secp256k1::verification_only();
            let index = ChildNumber::from_normal_idx(10).unwrap();
            let destination = desc.change_descriptor().derive(index, &secp);
            assert_eq!(tx.output[1].script_pubkey, destination.script_pubkey());
            assert!(!built.psbt.outputs[1].bip32_derivation.is_empty());
            assert!(built
                .psbt
                .inputs
                .iter()
                .all(|i| i.non_witness_utxo.is_some()
                    && i.witness_utxo.is_some()
                    && i.partial_sigs.is_empty()));
            let fee = 200_000 - tx.output[1].value.to_sat();
            assert!(fee >= desc.unsigned_tx_max_vbytes(tx, true) * 5);
            spend::reverify_spend_before_broadcast(&desc, &built.psbt).unwrap();
            let selected: Vec<_> = coins
                .iter()
                .map(|c| CandidateCoin {
                    must_select: true,
                    ..*c
                })
                .collect();
            let ordinary = spend::create_spend(
                &desc,
                &secp,
                &mut getter,
                &[],
                &selected,
                SpendTxFees::Regular(5),
                SpendOutputAddress {
                    addr: destination.address(chain.bitcoin_network()),
                    info: Some(AddrInfo {
                        index,
                        is_change: true,
                    }),
                },
                LockTime::ZERO,
            )
            .unwrap();
            assert_eq!(ordinary.psbt.unsigned_tx.output.len(), 1);
            // Full 99-byte poison output is charged; no post-sign fee adjustment.
            assert!(
                ordinary.psbt.unsigned_tx.output[0].value.to_sat() - tx.output[1].value.to_sat()
                    >= 99 * 5
            );
            assert_eq!(
                built.psbt,
                build(chain, &desc, &coins, &mut getter, 10, 5)
                    .unwrap()
                    .psbt
            );
        }
    }
    #[test]
    fn fork_sweep_uses_original_inputs_and_fresh_owned_output_on_matching_chain() {
        let secp = secp256k1::Secp256k1::verification_only();
        for (bitcoin, fork) in [
            (ChainId::Bitcoin, ChainId::BitcoinBlake2b),
            (ChainId::Testnet4, ChainId::BitcoinBlake2bTestnet4),
        ] {
            let (desc, mut coins, mut getter) = fixture();
            let source = build(bitcoin, &desc, &coins, &mut getter, 10, 5).unwrap();
            coins.reverse(); // Membership binds outpoints, not presentation order.
            let index = ChildNumber::from_normal_idx(20).unwrap();
            let sweep = create_claim_fork_sweep(
                &source,
                fork,
                &secp,
                &mut getter,
                &coins,
                index,
                3,
                LockTime::ZERO,
            )
            .unwrap();
            assert_eq!(sweep.chain(), fork);
            assert_eq!(
                sweep.bitcoin_step1(),
                source.psbt().unsigned_tx.compute_txid()
            );
            assert_eq!(sweep.descriptor().to_string(), desc.to_string());
            assert_eq!(sweep.change_index(), index);
            let psbt = sweep.psbt();
            assert_eq!(
                psbt.unsigned_tx
                    .input
                    .iter()
                    .map(|i| i.previous_output)
                    .collect::<BTreeSet<_>>(),
                coins.iter().map(|c| c.outpoint).collect()
            );
            assert!(psbt
                .unsigned_tx
                .input
                .iter()
                .all(|i| i.previous_output.txid != sweep.bitcoin_step1()));
            assert_eq!(psbt.unsigned_tx.output.len(), 1);
            assert_eq!(
                psbt.unsigned_tx.output[0].script_pubkey,
                desc.change_descriptor()
                    .derive(index, &secp)
                    .script_pubkey()
            );
            assert!(!psbt.outputs[0].bip32_derivation.is_empty());
            assert!(psbt.inputs.iter().all(|i| i.partial_sigs.is_empty()
                && i.non_witness_utxo.is_some()
                && i.witness_utxo.is_some()));
            let fee = 200_000 - psbt.unsigned_tx.output[0].value.to_sat();
            assert!(fee >= desc.unsigned_tx_max_vbytes(&psbt.unsigned_tx, true) * 3);
            spend::reverify_spend_before_broadcast(&desc, psbt).unwrap();
        }
    }

    #[test]
    fn fork_sweep_restart_rebuilds_metadata_and_exact_transaction() {
        let secp = secp256k1::Secp256k1::verification_only();
        for (bitcoin, fork) in [
            (ChainId::Bitcoin, ChainId::BitcoinBlake2b),
            (ChainId::Testnet4, ChainId::BitcoinBlake2bTestnet4),
        ] {
            let (desc, coins, mut getter) = fixture();
            let source = build(bitcoin, &desc, &coins, &mut getter, 10, 5).unwrap();
            let index = ChildNumber::from_normal_idx(20).unwrap();
            let original = create_claim_fork_sweep(
                &source,
                fork,
                &secp,
                &mut getter,
                &coins,
                index,
                7,
                LockTime::ZERO,
            )
            .unwrap();
            let recovered = reconstruct_claim_fork_sweep(
                &source,
                fork,
                &secp,
                &mut getter,
                &coins,
                index,
                &original.psbt().unsigned_tx,
            )
            .unwrap();
            assert_eq!(recovered.psbt(), original.psbt());
            assert_eq!(recovered.bitcoin_step1(), original.bitcoin_step1());
            assert_eq!(recovered.chain(), fork);
        }
    }

    #[test]
    fn fork_sweep_restart_rejects_replacement_and_untrusted_economics() {
        let secp = secp256k1::Secp256k1::verification_only();
        let (desc, coins, mut getter) = fixture();
        let source = build(ChainId::Bitcoin, &desc, &coins, &mut getter, 10, 5).unwrap();
        let index = ChildNumber::from_normal_idx(20).unwrap();
        let original = create_claim_fork_sweep(
            &source,
            ChainId::BitcoinBlake2b,
            &secp,
            &mut getter,
            &coins,
            index,
            7,
            LockTime::ZERO,
        )
        .unwrap();
        let tx = &original.psbt().unsigned_tx;
        let mut variants = Vec::new();
        let mut changed = tx.clone();
        changed.output.clear();
        variants.push(changed);
        let mut changed = tx.clone();
        changed.output.push(tx.output[0].clone());
        variants.push(changed);
        let mut changed = tx.clone();
        changed.output[0].script_pubkey = bitcoin::ScriptBuf::new();
        variants.push(changed);
        let mut changed = tx.clone();
        changed.output[0].value = Amount::from_sat(200_001);
        variants.push(changed);
        let mut changed = tx.clone();
        changed.output[0].value = Amount::ZERO;
        variants.push(changed);
        let mut changed = tx.clone();
        changed.input[0].previous_output.vout += 1;
        variants.push(changed);
        let mut changed = tx.clone();
        changed.input[0].witness.push([1]);
        variants.push(changed);
        let mut changed = tx.clone();
        changed.version = bitcoin::transaction::Version::ONE;
        variants.push(changed);
        for changed in variants {
            assert!(reconstruct_claim_fork_sweep(
                &source,
                ChainId::BitcoinBlake2b,
                &secp,
                &mut getter,
                &coins,
                index,
                &changed,
            )
            .is_err());
        }
        assert!(reconstruct_claim_fork_sweep(
            &source,
            ChainId::BitcoinBlake2bTestnet4,
            &secp,
            &mut getter,
            &coins,
            index,
            tx,
        )
        .is_err());
        assert!(reconstruct_claim_fork_sweep(
            &source,
            ChainId::BitcoinBlake2b,
            &secp,
            &mut getter,
            &coins,
            ChildNumber::from_normal_idx(21).unwrap(),
            tx,
        )
        .is_err());
        getter.0.clear();
        assert!(reconstruct_claim_fork_sweep(
            &source,
            ChainId::BitcoinBlake2b,
            &secp,
            &mut getter,
            &coins,
            index,
            tx,
        )
        .is_err());
    }

    #[test]
    fn fork_sweep_rejects_missing_extra_duplicate_and_mutated_coins() {
        let (desc, coins, mut getter) = fixture();
        let source = build(ChainId::Bitcoin, &desc, &coins, &mut getter, 10, 5).unwrap();
        let secp = secp256k1::Secp256k1::verification_only();
        let index = ChildNumber::from_normal_idx(20).unwrap();
        let mut cases = vec![
            vec![],
            vec![coins[0]],
            vec![coins[0], coins[0]],
            vec![coins[0], coins[1], coins[1]],
            vec![coins[0], coins[1], coins[0]],
        ];
        for changed in [
            CandidateCoin {
                amount: Amount::from_sat(100_001),
                ..coins[0]
            },
            CandidateCoin {
                outpoint: OutPoint::new(source.psbt().unsigned_tx.compute_txid(), 1),
                ..coins[0]
            },
            CandidateCoin {
                deriv_index: ChildNumber::from_hardened_idx(0).unwrap(),
                ..coins[0]
            },
            CandidateCoin {
                deriv_index: ChildNumber::from_normal_idx(9).unwrap(),
                ..coins[0]
            },
            CandidateCoin {
                is_change: true,
                ..coins[0]
            },
        ] {
            cases.push(vec![changed, coins[1]]);
        }
        for bad in cases {
            assert!(create_claim_fork_sweep(
                &source,
                ChainId::BitcoinBlake2b,
                &secp,
                &mut getter,
                &bad,
                index,
                3,
                LockTime::ZERO
            )
            .is_err());
        }
        for chain in [
            ChainId::Bitcoin,
            ChainId::Testnet4,
            ChainId::BitcoinBlake2bTestnet4,
            ChainId::Regtest,
        ] {
            assert!(create_claim_fork_sweep(
                &source,
                chain,
                &secp,
                &mut getter,
                &coins,
                index,
                3,
                LockTime::ZERO
            )
            .is_err());
        }
        assert!(create_claim_fork_sweep(
            &source,
            ChainId::BitcoinBlake2b,
            &secp,
            &mut getter,
            &coins,
            ChildNumber::from_hardened_idx(0).unwrap(),
            3,
            LockTime::ZERO
        )
        .is_err());
        assert!(create_claim_fork_sweep(
            &source,
            ChainId::BitcoinBlake2b,
            &secp,
            &mut Getter(HashMap::new()),
            &coins,
            index,
            3,
            LockTime::ZERO
        )
        .is_err());
        // A fork-side previous transaction must authenticate, not just carry
        // the right amount at the requested output index.
        getter.0.get_mut(&coins[0].outpoint.txid).unwrap().output[0].value =
            Amount::from_sat(99_999);
        assert!(create_claim_fork_sweep(
            &source,
            ChainId::BitcoinBlake2b,
            &secp,
            &mut getter,
            &coins,
            index,
            3,
            LockTime::ZERO
        )
        .is_err());
    }

    #[test]
    fn fork_sweep_refuses_reusing_a_selected_change_script() {
        let (desc, mut coins, mut getter) = fixture();
        let secp = secp256k1::Secp256k1::verification_only();
        let mut previous = getter.0.remove(&coins[0].outpoint.txid).unwrap();
        previous.output[0].script_pubkey = desc
            .change_descriptor()
            .derive(coins[0].deriv_index, &secp)
            .script_pubkey();
        coins[0].is_change = true;
        coins[0].outpoint.txid = previous.compute_txid();
        getter.0.insert(previous.compute_txid(), previous);
        let source = build(ChainId::Bitcoin, &desc, &coins, &mut getter, 10, 5).unwrap();
        assert!(create_claim_fork_sweep(
            &source,
            ChainId::BitcoinBlake2b,
            &secp,
            &mut getter,
            &coins,
            coins[0].deriv_index,
            3,
            LockTime::ZERO
        )
        .is_err());
        assert!(create_claim_fork_sweep(
            &source,
            ChainId::BitcoinBlake2b,
            &secp,
            &mut getter,
            &coins,
            ChildNumber::from_normal_idx(20).unwrap(),
            3,
            LockTime::ZERO
        )
        .is_ok());
    }

    #[test]
    fn reconstruction_rechecks_owned_plan_without_changing_its_fee_or_metadata() {
        for chain in [ChainId::Bitcoin, ChainId::Testnet4] {
            let (desc, coins, mut getter) = fixture();
            let built = build(chain, &desc, &coins, &mut getter, 10, 5).unwrap();
            let verify = secp256k1::Secp256k1::verification_only();
            let index = ChildNumber::from_normal_idx(10).unwrap();
            let restore = |tx: &Transaction, getter: &mut Getter| {
                reconstruct_poison_self_transfer(chain, &desc, &verify, getter, &coins, index, tx)
            };
            let restored = restore(&built.psbt.unsigned_tx, &mut getter).unwrap();
            assert_eq!(restored.psbt(), built.psbt());
            assert_eq!(restored.change_index(), index);
            assert_eq!(restored.chain(), chain);
            let mut mutations = Vec::new();
            let mut tx = built.psbt.unsigned_tx.clone();
            tx.output[1].script_pubkey = desc
                .receive_descriptor()
                .derive(index, &verify)
                .script_pubkey();
            mutations.push(tx);
            let mut tx = built.psbt.unsigned_tx.clone();
            tx.output[1].value = Amount::MAX;
            mutations.push(tx);
            let mut tx = built.psbt.unsigned_tx.clone();
            tx.output[1].value = Amount::ZERO;
            mutations.push(tx);
            let mut tx = built.psbt.unsigned_tx.clone();
            tx.output[0].value = Amount::from_sat(1);
            mutations.push(tx);
            let mut tx = built.psbt.unsigned_tx.clone();
            let mut script = tx.output[0].script_pubkey.clone().into_bytes();
            script[51] ^= 1; // recorded input commitment
            tx.output[0].script_pubkey = bitcoin::ScriptBuf::from_bytes(script);
            mutations.push(tx);
            let mut tx = built.psbt.unsigned_tx.clone();
            tx.input[0].previous_output = OutPoint::null();
            mutations.push(tx);
            let mut tx = built.psbt.unsigned_tx.clone();
            tx.input[0].sequence = bitcoin::Sequence(46);
            mutations.push(tx);
            let mut tx = built.psbt.unsigned_tx.clone();
            tx.input[0].witness.push([1]);
            mutations.push(tx);
            let mut tx = built.psbt.unsigned_tx.clone();
            tx.output.pop();
            mutations.push(tx);
            for tx in mutations {
                assert!(
                    restore(&tx, &mut getter).is_err(),
                    "accepted mutated plan: {:?}",
                    tx
                );
            }
            let mut missing = Getter(HashMap::new());
            assert!(restore(&built.psbt.unsigned_tx, &mut missing).is_err());
            assert!(reconstruct_poison_self_transfer(
                chain,
                &desc,
                &verify,
                &mut getter,
                &coins,
                ChildNumber::from_normal_idx(11).unwrap(),
                &built.psbt.unsigned_tx
            )
            .is_err());
        }
    }

    #[test]
    fn invalid_plan_refuses_without_panics() {
        let (desc, coins, mut getter) = fixture();
        for chain in [
            ChainId::BitcoinBlake2b,
            ChainId::BitcoinBlake2bTestnet4,
            ChainId::Testnet,
            ChainId::Signet,
            ChainId::Regtest,
        ] {
            assert!(matches!(
                build(chain, &desc, &coins, &mut getter, 10, 5),
                Err(Error::InvalidRequest(_))
            ));
        }
        assert!(build(ChainId::Bitcoin, &desc, &[], &mut getter, 10, 5).is_err());
        assert!(build(
            ChainId::Bitcoin,
            &desc,
            &[coins[0], coins[0]],
            &mut getter,
            10,
            5
        )
        .is_err());
        let mut reused = coins.clone();
        reused[0].is_change = true;
        reused[0].deriv_index = ChildNumber::from_normal_idx(10).unwrap();
        assert!(matches!(
            build(ChainId::Bitcoin, &desc, &reused, &mut getter, 10, 5),
            Err(Error::InvalidRequest(_))
        ));
        for bad in [
            CandidateCoin {
                outpoint: OutPoint::null(),
                ..coins[0]
            },
            CandidateCoin {
                deriv_index: ChildNumber::from_hardened_idx(1).unwrap(),
                ..coins[0]
            },
            CandidateCoin {
                amount: Amount::MAX,
                ..coins[0]
            },
        ] {
            assert!(matches!(
                build(ChainId::Bitcoin, &desc, &[bad], &mut getter, 10, 5),
                Err(Error::InvalidRequest(_))
            ));
        }
        assert!(create_poison_self_transfer(
            ChainId::Bitcoin,
            &desc,
            &secp256k1::Secp256k1::verification_only(),
            &mut getter,
            &coins,
            ChildNumber::from_hardened_idx(1).unwrap(),
            5,
            LockTime::ZERO,
            bitcoin::BlockHash::from_byte_array([42; 32])
        )
        .is_err());
        let taproot = CoincubeDescriptor::from_str(TR_DESC).unwrap();
        assert!(matches!(
            build(ChainId::Bitcoin, &taproot, &coins, &mut getter, 10, 5),
            Err(Error::InvalidRequest(_))
        ));
        for fee in [0, 1001, u64::MAX] {
            assert!(build(ChainId::Bitcoin, &desc, &coins, &mut getter, 10, fee).is_err());
        }
    }
    #[test]
    fn authenticated_prevouts_cannot_be_replaced_or_misstated() {
        let (desc, coins, mut getter) = fixture();
        let mut wrong = coins.clone();
        wrong[0].amount = Amount::from_sat(99_999);
        assert!(matches!(
            build(ChainId::Bitcoin, &desc, &wrong, &mut getter, 10, 5),
            Err(Error::Spend(SpendCreationError::InputAuthentication(..)))
        ));
        wrong = coins.clone();
        wrong[0].deriv_index = ChildNumber::from_normal_idx(99).unwrap();
        assert!(matches!(
            build(ChainId::Bitcoin, &desc, &wrong, &mut getter, 10, 5),
            Err(Error::Spend(SpendCreationError::InputAuthentication(..)))
        ));
        getter.0.get_mut(&coins[0].outpoint.txid).unwrap().output[0].value = Amount::from_sat(1);
        assert!(matches!(
            build(ChainId::Bitcoin, &desc, &coins, &mut getter, 10, 5),
            Err(Error::Spend(SpendCreationError::InputAuthentication(..)))
        ));
        getter.0.clear();
        assert!(matches!(
            build(ChainId::Bitcoin, &desc, &coins, &mut getter, 10, 5),
            Err(Error::Spend(SpendCreationError::InputAuthentication(..)))
        ));
    }
    /// Golden bytes pinned before the OP_RETURN builder was shared with
    /// Split step 1. Any change to Claim's poison payload, output order,
    /// economics or PSBT metadata fails here.
    #[test]
    fn claim_poison_bytes_are_pinned() {
        for (chain, script_hex, psbt_digest) in [
            (
                ChainId::Bitcoin,
                "6a4c57434f494e435542452d53504c495401002a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a749deaf27f2caf93c571f4cbe3c4450d6eb6ac07fc0885c8a9948bd32bb5c0d500000000000000",
                "2d2675c76bd4b96aa57fa42c6476a1d1def276775dd281bbdab9a1d3a22f19ba",
            ),
            (
                ChainId::Testnet4,
                "6a4c57434f494e435542452d53504c495401012a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a749deaf27f2caf93c571f4cbe3c4450d6eb6ac07fc0885c8a9948bd32bb5c0d500000000000000",
                "207980e684e5c252d2a15ec66b38f88d1fb5ece762d2df0fcb0d20ef415f7e94",
            ),
        ] {
            let (desc, coins, mut getter) = fixture();
            let built = build(chain, &desc, &coins, &mut getter, 10, 5).unwrap();
            let tx = &built.psbt.unsigned_tx;
            let digest = sha256::Hash::hash(&built.psbt.serialize()).to_string();
            assert_eq!(tx.output[0].script_pubkey.to_hex_string(), script_hex);
            assert_eq!(digest, psbt_digest);
        }
    }
}
