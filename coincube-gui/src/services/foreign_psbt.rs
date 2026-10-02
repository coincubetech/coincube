//! Session-only PSBT preparation for the BTCB2 foreign-wallet Split tool.
//!
//! This module constructs and checks a signing handoff. It cannot finalize,
//! broadcast, persist secrets, or grant Claim/ancestry authority.

use std::{collections::BTreeSet, convert::TryFrom, str::FromStr};

use coincube_core::{
    chain::ChainId,
    miniscript::{
        bitcoin::{
            self, absolute, psbt::Psbt, sighash::EcdsaSighashType, transaction, Amount, OutPoint,
            Sequence, Transaction, TxIn, TxOut,
        },
        descriptor::DefiniteDescriptorKey,
        psbt::PsbtExt,
        Descriptor,
    },
    spend,
};
use sha2::{Digest, Sha256};

use super::foreign_scan::{Branch, DiscoveredCoin, ForkSide, ScanDescriptor, ScanReport};
use crate::{
    app::{
        settings::{CubeSettings, WalletId},
        wallet::{descriptor_id_fingerprint, Wallet},
    },
    daemon::model::{GetAddressResult, GetInfoResult, ListCoinsEntry},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForeignPsbtError {
    UnsupportedChain,
    StaleSession,
    SourceChanged,
    TargetChanged,
    TargetAddress,
    StaleAddress,
    Empty,
    DuplicateInput,
    Unconfirmed,
    Descriptor,
    Prevout,
    Economics,
    Psbt,
    Parse,
    ConstructionChanged,
    UnsupportedSighash,
    MissingSignature,
    /// A source descriptor has no PSBT-file signing route (`tr` is scan-only).
    UnsupportedRoute,
    /// Any Taproot PSBT field on import; no foreign route signs Taproot.
    Taproot,
    /// No observed fork height, so no coin can be classified as pre-fork.
    ForkUnknown,
    /// The reserved target address already has history in the target Vault.
    TargetUsed,
    /// The target Vault's daemon cannot prove the reserved address unused:
    /// it is not fully synced, has not completed a poll since the reservation,
    /// is rescanning, holds back a reorg or diverged, or has not recorded the
    /// reservation.
    TargetFreshnessUnknown,
}

/// Explicit change selection. The amount is never inferred by the handoff.
pub struct ForeignChange {
    pub index: u32,
    pub amount: Amount,
}

/// What the target Vault's daemon reported for a Split destination: the
/// address `get_new_address` reserved, then `get_info` and the coin listing
/// of every status, both taken after the reservation and after a successful
/// poll that finished no earlier than the reservation. The daemon's history is
/// the only evidence of address use, so each observation is required.
#[derive(Debug, Clone)]
pub struct TargetReservation {
    pub reserved: GetAddressResult,
    /// Unix seconds taken immediately before `get_new_address`. `info` must
    /// show a successful poll at or after this time.
    pub requested_at: u32,
    pub info: GetInfoResult,
    /// `list_coins` with no status or outpoint filter: unconfirmed,
    /// confirmed, spending and spent coins alike.
    pub coins: Vec<ListCoinsEntry>,
}

/// Opaque proof that a daemon-reserved receive address belongs to the exact
/// selected BTCB2 Cube and its currently loaded Vault descriptor, and that it
/// is fresh: the daemon, fully synced, has recorded the reservation and knows
/// no coin (of any status) ever paid to it (#571 P3-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetAddressEvidence {
    cube_id: String,
    vault_id: WalletId,
    vault_fingerprint: String,
    derivation_index: bitcoin::bip32::ChildNumber,
    script_pubkey: bitcoin::ScriptBuf,
    generation: u64,
}

impl TargetAddressEvidence {
    pub fn authenticate(
        cube: &CubeSettings,
        wallet: &Wallet,
        reservation: &TargetReservation,
        generation: u64,
    ) -> Result<Self, ForeignPsbtError> {
        let reserved = &reservation.reserved;
        if cube.id.is_empty()
            || cube.network != ChainId::BitcoinBlake2b
            || wallet.chain != ChainId::BitcoinBlake2b
            || cube.vault_wallet_id.as_ref() != Some(&wallet.id())
        {
            return Err(ForeignPsbtError::TargetChanged);
        }
        let vault_fingerprint = descriptor_id_fingerprint(&wallet.main_descriptor).to_string();
        if cube.vault_fingerprint.as_deref() != Some(vault_fingerprint.as_str())
            || reserved.derivation_index.is_hardened()
        {
            return Err(ForeignPsbtError::TargetChanged);
        }
        let secp = bitcoin::secp256k1::Secp256k1::verification_only();
        let derived = wallet
            .main_descriptor
            .receive_descriptor()
            .derive(reserved.derivation_index, &secp)
            .address(wallet.chain.bitcoin_network());
        if derived != reserved.address {
            return Err(ForeignPsbtError::TargetAddress);
        }
        let script_pubkey = derived.script_pubkey();
        check_target_fresh(wallet, reservation, &script_pubkey)?;
        Ok(Self {
            cube_id: cube.id.clone(),
            vault_id: wallet.id(),
            vault_fingerprint,
            derivation_index: reserved.derivation_index,
            script_pubkey,
            generation,
        })
    }

    pub fn cube_id(&self) -> &str {
        &self.cube_id
    }

    pub fn vault_fingerprint(&self) -> &str {
        &self.vault_fingerprint
    }

    pub fn derivation_index(&self) -> bitcoin::bip32::ChildNumber {
        self.derivation_index
    }
}

/// Fail closed unless the target daemon proves the reserved receive address
/// unused. The listing must come from this Vault's descriptor; the daemon must
/// be fully synced, have completed a poll at or after the reservation time
/// (Electrum/Esplora report `sync == 1.0` before any poll), with no rescan,
/// held-back reorg or divergence; its receive
/// index must already cover the reservation; and no coin of any status may pay
/// the reserved script or sit at the reserved receive index.
fn check_target_fresh(
    wallet: &Wallet,
    reservation: &TargetReservation,
    script_pubkey: &bitcoin::Script,
) -> Result<(), ForeignPsbtError> {
    let info = &reservation.info;
    if info.descriptors.main != wallet.main_descriptor {
        return Err(ForeignPsbtError::TargetChanged);
    }
    let index = u32::from(reservation.reserved.derivation_index);
    // `sync` is rounded up to exactly 1.0 when complete; NaN fails.
    let synced = info.sync >= 1.0;
    let polled_since = info
        .last_poll_timestamp
        .is_some_and(|polled| polled >= reservation.requested_at);
    if !synced
        || !polled_since
        || info.rescan_progress.is_some()
        || info.refused_reorg_depth.is_some()
        || info.chain_divergence
        || info.receive_index < index
    {
        return Err(ForeignPsbtError::TargetFreshnessUnknown);
    }
    let used = reservation.coins.iter().any(|coin| {
        coin.address.script_pubkey().as_script() == script_pubkey
            || (!coin.is_change && u32::from(coin.derivation_index) == index)
    });
    if used {
        return Err(ForeignPsbtError::TargetUsed);
    }
    Ok(())
}

/// Current UI/session identity supplied again at import time. Its descriptor
/// fingerprint is computed internally so callers cannot assert one by fiat.
pub struct ForeignSession<'a> {
    pub chain: ChainId,
    pub generation: u64,
    pub target: &'a TargetAddressEvidence,
    pub external: &'a ScanDescriptor,
    pub internal: Option<&'a ScanDescriptor>,
}

/// An imported partial-signature PSBT that remains evidence only. There is no
/// finalization or submission method on this type.
#[derive(Debug)]
pub struct VerifiedForeignPsbt(Psbt);
impl VerifiedForeignPsbt {
    pub fn psbt(&self) -> &Psbt {
        &self.0
    }
}

/// Step-two authority for a foreign sweep (#568 B2). Scan evidence,
/// economics, a reserved address or a saved phase cannot create one: only
/// `claim_coordinator::fork::split::SplitPreparation::check_signing` mints
/// it, from fresh evidence that step 1 has six Bitcoin confirmations at the
/// tip, RDTS its margin, and every claimed coin is still unspent on BTCB2. It
/// is one-use, short-lived and bound to the check, generation, claimed
/// prevouts and tracked step-1 txid.
pub(crate) use super::claim_coordinator::fork::split::ForeignStep2Authorization;

pub struct PreparedForeignSweep {
    original: Psbt,
    chain: ChainId,
    generation: u64,
    target: TargetAddressEvidence,
    source_fingerprint: [u8; 32],
    fee: Amount,
}

/// Fee-independent review of what a sweep would spend: the pre-fork coins of
/// the authenticated scan and a conservative signed size. `maximum_signed_vbytes`
/// is the maximum for every selected input, so the later exact transaction may
/// be smaller but never larger for the same one-output shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SweepInputs {
    pub inputs: usize,
    pub total: Amount,
    pub maximum_signed_vbytes: u64,
    /// Confirmed at or after the observed fork height: not part of this
    /// split and not known to be BTCB2-only (it may still be replayable).
    pub excluded_post_fork: usize,
    /// No confirming height: excluded (fail closed).
    pub excluded_unknown: usize,
}

/// Read-only, conservative sweep-all economics. Constructing this value does
/// not construct a PSBT or grant any signing, finalisation, or broadcast
/// capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SweepEconomics {
    pub inputs: usize,
    pub total: Amount,
    pub feerate_sat_vb: u64,
    pub maximum_signed_vbytes: u64,
    pub fee: Amount,
    pub destination: Amount,
}

struct SelectedInput<'r> {
    coin: &'r DiscoveredCoin,
    definite: Descriptor<DefiniteDescriptorKey>,
    output: TxOut,
}

struct Selection<'r> {
    inputs: Vec<SelectedInput<'r>>,
    total: u64,
    excluded_post_fork: usize,
    excluded_unknown: usize,
}

/// The one input filter shared by review and construction: session identity,
/// the PSBT-file signing route for every source descriptor (`tr` has none),
/// an observed fork height, and only coins confirmed below it.
fn select_pre_fork<'r>(
    report: &'r ScanReport,
    session: &ForeignSession<'_>,
) -> Result<Selection<'r>, ForeignPsbtError> {
    verify_session(report.chain(), report.generation(), session)?;
    if std::iter::once(session.external)
        .chain(session.internal)
        .any(|descriptor| !descriptor.capabilities().signing.psbt_file)
    {
        return Err(ForeignPsbtError::UnsupportedRoute);
    }
    if report.coins().is_empty() {
        return Err(ForeignPsbtError::Empty);
    }
    if report.fork_height().is_none() {
        return Err(ForeignPsbtError::ForkUnknown);
    }
    let mut coins: Vec<_> = report.coins().iter().collect();
    coins.sort_by_key(|coin| coin.outpoint);
    let mut seen = BTreeSet::<OutPoint>::new();
    let mut selection = Selection {
        inputs: Vec::with_capacity(coins.len()),
        total: 0,
        excluded_post_fork: 0,
        excluded_unknown: 0,
    };
    for coin in coins {
        if !seen.insert(coin.outpoint) {
            return Err(ForeignPsbtError::DuplicateInput);
        }
        match report.fork_side(coin) {
            ForkSide::PreFork => {}
            ForkSide::Unconfirmed => return Err(ForeignPsbtError::Unconfirmed),
            ForkSide::PostFork => {
                selection.excluded_post_fork += 1;
                continue;
            }
            ForkSide::Unknown => {
                selection.excluded_unknown += 1;
                continue;
            }
        }
        let definite = descriptor_for(coin.branch, session)?
            .derive(coin.index)
            .map_err(|_| ForeignPsbtError::Descriptor)?;
        if matches!(definite, Descriptor::Tr(_)) {
            return Err(ForeignPsbtError::UnsupportedRoute);
        }
        if definite.script_pubkey() != coin.output.script_pubkey {
            return Err(ForeignPsbtError::Descriptor);
        }
        let output = spend::authenticate_previous_output(
            &coin.outpoint,
            Some(&coin.previous),
            Some(&coin.output),
        )
        .map_err(|_| ForeignPsbtError::Prevout)?;
        selection.total = selection
            .total
            .checked_add(output.value.to_sat())
            .filter(|value| *value <= Amount::MAX_MONEY.to_sat())
            .ok_or(ForeignPsbtError::Economics)?;
        selection.inputs.push(SelectedInput {
            coin,
            definite,
            output,
        });
    }
    if selection.inputs.is_empty() {
        return Err(ForeignPsbtError::Empty);
    }
    Ok(selection)
}

fn unsigned_inputs(selection: &Selection<'_>) -> Vec<TxIn> {
    selection
        .inputs
        .iter()
        .map(|input| TxIn {
            previous_output: input.coin.outpoint,
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            ..TxIn::default()
        })
        .collect()
}

/// Fee-independent sweep review. Refuses `tr`, an unknown fork height,
/// unconfirmed coins and an empty pre-fork set.
pub fn review_sweep_inputs(
    report: &ScanReport,
    session: ForeignSession<'_>,
) -> Result<SweepInputs, ForeignPsbtError> {
    let selection = select_pre_fork(report, &session)?;
    let mut satisfaction_weight = 0_u64;
    let mut has_witness = false;
    for input in &selection.inputs {
        satisfaction_weight = satisfaction_weight
            .checked_add(
                input
                    .definite
                    .max_weight_to_satisfy()
                    .map_err(|_| ForeignPsbtError::Descriptor)?
                    .to_wu(),
            )
            .ok_or(ForeignPsbtError::Economics)?;
        has_witness |= !matches!(input.definite, Descriptor::Pkh(_));
    }
    let unsigned = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: unsigned_inputs(&selection),
        output: vec![TxOut {
            value: Amount::from_sat(spend::DUST_OUTPUT_SATS),
            script_pubkey: session.target.script_pubkey.clone(),
        }],
    };
    // `max_weight_to_satisfy` measures from an input that already carries its
    // empty witness-stack byte. The unsigned serialization above has no
    // witness section, so a witness transaction also needs marker+flag and one
    // empty-stack byte for every input (including legacy inputs in a mixed
    // transaction).
    let witness_overhead = if has_witness {
        2_u64
            .checked_add(
                u64::try_from(unsigned.input.len()).map_err(|_| ForeignPsbtError::Economics)?,
            )
            .ok_or(ForeignPsbtError::Economics)?
    } else {
        0
    };
    let maximum_signed_vbytes = unsigned
        .weight()
        .to_wu()
        .checked_add(satisfaction_weight)
        .and_then(|weight| weight.checked_add(witness_overhead))
        .and_then(|weight| weight.checked_add(3))
        .map(|weight| weight / 4)
        .ok_or(ForeignPsbtError::Economics)?;
    Ok(SweepInputs {
        inputs: selection.inputs.len(),
        total: Amount::from_sat(selection.total),
        maximum_signed_vbytes,
        excluded_post_fork: selection.excluded_post_fork,
        excluded_unknown: selection.excluded_unknown,
    })
}

/// Compute a conservative, one-output sweep review from the authenticated scan
/// evidence and a BTCB2-scoped fee rate. This deliberately stops before
/// [`PreparedForeignSweep`], which requires [`ForeignStep2Authorization`].
pub fn review_sweep_economics(
    report: &ScanReport,
    session: ForeignSession<'_>,
    feerate_sat_vb: u64,
) -> Result<SweepEconomics, ForeignPsbtError> {
    if !(1..=spend::MAX_FEERATE).contains(&feerate_sat_vb) {
        verify_session(report.chain(), report.generation(), &session)?;
        return Err(ForeignPsbtError::Economics);
    }
    let inputs = review_sweep_inputs(report, session)?;
    let fee_sat = inputs
        .maximum_signed_vbytes
        .checked_mul(feerate_sat_vb)
        .filter(|fee| *fee <= spend::MAX_FEE.to_sat())
        .ok_or(ForeignPsbtError::Economics)?;
    let destination_sat = inputs
        .total
        .to_sat()
        .checked_sub(fee_sat)
        .filter(|amount| *amount >= spend::DUST_OUTPUT_SATS)
        .ok_or(ForeignPsbtError::Economics)?;
    Ok(SweepEconomics {
        inputs: inputs.inputs,
        total: inputs.total,
        feerate_sat_vb,
        maximum_signed_vbytes: inputs.maximum_signed_vbytes,
        fee: Amount::from_sat(fee_sat),
        destination: Amount::from_sat(destination_sat),
    })
}

/// A fee-rate source for a Split sweep, tagged with the chain whose mempool
/// it measures. The BTCB2 flow consults only a BTCB2-scoped source.
#[async_trait::async_trait]
pub trait SweepFeeSource: Send + Sync {
    fn chain(&self) -> ChainId;
    async fn mid_priority_sat_vb(&self) -> Option<u64>;
}

/// The fail-closed source when no Connect account session can read the
/// BTCB2 estimate (`ConnectBtcb2Fees` in `split_fees`, #568 D4). Bitcoin
/// mainnet fees do not describe the BTCB2 mempool, so the review shows fees
/// as unavailable.
pub struct UnavailableBtcb2Fees;

#[async_trait::async_trait]
impl SweepFeeSource for UnavailableBtcb2Fees {
    fn chain(&self) -> ChainId {
        ChainId::BitcoinBlake2b
    }
    async fn mid_priority_sat_vb(&self) -> Option<u64> {
        None
    }
}

/// Resolve a BTCB2 sweep fee rate. A source scoped to any other chain is
/// never queried; an out-of-bounds rate is unavailable, not clamped.
pub async fn btcb2_sweep_feerate(source: &dyn SweepFeeSource) -> Option<u64> {
    if source.chain() != ChainId::BitcoinBlake2b {
        return None;
    }
    source
        .mid_priority_sat_vb()
        .await
        .filter(|rate| (1..=spend::MAX_FEERATE).contains(rate))
}

impl PreparedForeignSweep {
    /// Construct the exact unsigned step-two sweep. Redeems (consumes)
    /// step-two authority, which must cover exactly the selected coins: the
    /// step-1 claimed prevouts, under this session's chain and generation.
    /// No change output: step 2 sweeps the claimed coins whole (#568).
    // Dormant: no production caller until B3b wires step 2 behind the token.
    #[allow(dead_code)]
    pub(crate) fn new(
        report: &ScanReport,
        session: ForeignSession<'_>,
        destination_amount: Amount,
        fee: Amount,
        change: Option<ForeignChange>,
        authorization: ForeignStep2Authorization,
    ) -> Result<Self, ForeignPsbtError> {
        if change.is_some() {
            return Err(ForeignPsbtError::Economics);
        }
        let prevouts: Vec<OutPoint> = select_pre_fork(report, &session)?
            .inputs
            .iter()
            .map(|input| input.coin.outpoint)
            .collect();
        authorization
            .redeem(session.chain, session.generation, &prevouts)
            .map_err(|error| match error {
                super::claim_coordinator::fork::split::RedeemError::Stale => {
                    ForeignPsbtError::StaleSession
                }
                super::claim_coordinator::fork::split::RedeemError::Mismatch => {
                    ForeignPsbtError::ConstructionChanged
                }
            })?;
        Self::construct(report, session, destination_amount, fee, None)
    }

    /// Consume every authenticated pre-fork coin in the report and construct
    /// one exact unsigned transaction. The authenticated target supplies the
    /// destination script; the caller supplies its amount, fee and optional
    /// change, whose sum must equal the selected inputs exactly. Private: only
    /// [`Self::new`] (behind the token) and this module's tests reach it.
    fn construct(
        report: &ScanReport,
        session: ForeignSession<'_>,
        destination_amount: Amount,
        fee: Amount,
        change: Option<ForeignChange>,
    ) -> Result<Self, ForeignPsbtError> {
        let selection = select_pre_fork(report, &session)?;
        if destination_amount.to_sat() < spend::DUST_OUTPUT_SATS
            || destination_amount > Amount::MAX_MONEY
            || fee == Amount::ZERO
            || fee > spend::MAX_FEE
        {
            return Err(ForeignPsbtError::Economics);
        }

        let mut outputs = vec![TxOut {
            value: destination_amount,
            script_pubkey: session.target.script_pubkey.clone(),
        }];
        let change_descriptor = match change {
            Some(change) => {
                let descriptor = session.internal.ok_or(ForeignPsbtError::Descriptor)?;
                let definite = descriptor
                    .derive(change.index)
                    .map_err(|_| ForeignPsbtError::Descriptor)?;
                if change.amount.to_sat() < spend::DUST_OUTPUT_SATS
                    || change.amount > Amount::MAX_MONEY
                {
                    return Err(ForeignPsbtError::Economics);
                }
                let script_pubkey = definite.script_pubkey();
                if script_pubkey == outputs[0].script_pubkey
                    || selection
                        .inputs
                        .iter()
                        .any(|input| input.output.script_pubkey == script_pubkey)
                {
                    return Err(ForeignPsbtError::Economics);
                }
                outputs.push(TxOut {
                    value: change.amount,
                    script_pubkey,
                });
                Some(definite)
            }
            None => None,
        };
        let spent = outputs.iter().try_fold(fee.to_sat(), |sum, output| {
            sum.checked_add(output.value.to_sat())
        });
        if spent != Some(selection.total) {
            return Err(ForeignPsbtError::Economics);
        }

        let tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: unsigned_inputs(&selection),
            output: outputs,
        };
        let mut psbt = Psbt::from_unsigned_tx(tx).map_err(|_| ForeignPsbtError::Psbt)?;
        for (index, input) in selection.inputs.into_iter().enumerate() {
            psbt.inputs[index].non_witness_utxo = Some(input.coin.previous.clone());
            psbt.inputs[index].witness_utxo = Some(input.output);
            psbt.update_input_with_descriptor(index, &input.definite)
                .map_err(|_| ForeignPsbtError::Descriptor)?;
        }
        if let Some(descriptor) = change_descriptor {
            psbt.update_output_with_descriptor(1, &descriptor)
                .map_err(|_| ForeignPsbtError::Descriptor)?;
        }
        Ok(Self {
            original: psbt,
            chain: session.chain,
            generation: session.generation,
            target: session.target.clone(),
            source_fingerprint: source_fingerprint(&session)?,
            fee,
        })
    }

    pub fn export_text(&self) -> String {
        self.original.to_string()
    }

    pub fn fee(&self) -> Amount {
        self.fee
    }

    /// Parse a returned PSBT and allow only standard SIGHASH_ALL signatures to
    /// augment the exact prepared object. Signature validity and satisfaction
    /// are deliberately left to a later finalization boundary.
    pub fn import_text(
        &self,
        text: &str,
        current: ForeignSession<'_>,
    ) -> Result<VerifiedForeignPsbt, ForeignPsbtError> {
        if current.chain != ChainId::BitcoinBlake2b {
            return Err(ForeignPsbtError::UnsupportedChain);
        }
        if current.chain != self.chain || current.generation != self.generation {
            return Err(ForeignPsbtError::StaleSession);
        }
        if current.target.cube_id != self.target.cube_id
            || current.target.vault_id != self.target.vault_id
            || current.target.vault_fingerprint != self.target.vault_fingerprint
        {
            return Err(ForeignPsbtError::TargetChanged);
        }
        if current.target != &self.target || current.target.generation != current.generation {
            return Err(ForeignPsbtError::StaleAddress);
        }
        if source_fingerprint(&current)? != self.source_fingerprint {
            return Err(ForeignPsbtError::SourceChanged);
        }
        let signed = Psbt::from_str(text.trim()).map_err(|_| ForeignPsbtError::Parse)?;
        if signed.unsigned_tx != self.original.unsigned_tx
            || signed.inputs.len() != self.original.inputs.len()
            || signed.outputs.len() != self.original.outputs.len()
        {
            return Err(ForeignPsbtError::ConstructionChanged);
        }
        // No foreign route signs Taproot, so any Taproot field is refused
        // before signatures are counted: a key-path or script-path signature
        // is never evidence here.
        if signed.inputs.iter().any(|input| {
            input.tap_key_sig.is_some()
                || !input.tap_script_sigs.is_empty()
                || !input.tap_scripts.is_empty()
                || !input.tap_key_origins.is_empty()
                || input.tap_internal_key.is_some()
                || input.tap_merkle_root.is_some()
        }) || signed.outputs.iter().any(|output| {
            output.tap_internal_key.is_some()
                || output.tap_tree.is_some()
                || !output.tap_key_origins.is_empty()
        }) {
            return Err(ForeignPsbtError::Taproot);
        }
        let mut normalized = signed.clone();
        let mut signatures = 0usize;
        for (index, input) in signed.inputs.iter().enumerate() {
            if input.sighash_type != self.original.inputs[index].sighash_type {
                return Err(ForeignPsbtError::ConstructionChanged);
            }
            if input.partial_sigs.iter().any(|(key, signature)| {
                signature.sighash_type != EcdsaSighashType::All
                    || !input.bip32_derivation.contains_key(&key.inner)
            }) {
                return Err(ForeignPsbtError::UnsupportedSighash);
            }
            signatures += input.partial_sigs.len();
            normalized.inputs[index].partial_sigs.clear();
        }
        if signatures == 0 {
            return Err(ForeignPsbtError::MissingSignature);
        }
        if normalized != self.original {
            return Err(ForeignPsbtError::ConstructionChanged);
        }
        Ok(VerifiedForeignPsbt(signed))
    }
}

fn verify_session(
    report_chain: ChainId,
    report_generation: u64,
    session: &ForeignSession<'_>,
) -> Result<(), ForeignPsbtError> {
    if report_chain != ChainId::BitcoinBlake2b || session.chain != ChainId::BitcoinBlake2b {
        return Err(ForeignPsbtError::UnsupportedChain);
    }
    if report_generation != session.generation {
        return Err(ForeignPsbtError::StaleSession);
    }
    if session.target.cube_id.is_empty()
        || session.target.generation != session.generation
        || session.external.branch() != Branch::External
        || session
            .internal
            .is_some_and(|descriptor| descriptor.branch() != Branch::Internal)
    {
        return Err(ForeignPsbtError::Descriptor);
    }
    Ok(())
}

fn descriptor_for<'a>(
    branch: Branch,
    session: &'a ForeignSession<'_>,
) -> Result<&'a ScanDescriptor, ForeignPsbtError> {
    match branch {
        Branch::External => Ok(session.external),
        Branch::Internal => session.internal.ok_or(ForeignPsbtError::Descriptor),
    }
}

fn source_fingerprint(session: &ForeignSession<'_>) -> Result<[u8; 32], ForeignPsbtError> {
    if session.external.branch() != Branch::External
        || session
            .internal
            .is_some_and(|descriptor| descriptor.branch() != Branch::Internal)
    {
        return Err(ForeignPsbtError::Descriptor);
    }
    let mut digest = Sha256::new();
    for (tag, descriptor) in [(0_u8, Some(session.external)), (1, session.internal)] {
        digest.update([tag]);
        match descriptor {
            Some(descriptor) => {
                let canonical = descriptor.canonical();
                digest.update((canonical.len() as u64).to_be_bytes());
                digest.update(canonical.as_bytes());
            }
            None => digest.update(0_u64.to_be_bytes()),
        }
    }
    Ok(digest.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::settings::VaultIdentity;
    use crate::services::foreign_scan::Branch as ScanBranch;
    use crate::services::{
        foreign_scan::{DiscoveredCoin, ScanReport},
        foreign_wallet_source::{AccountDescriptors, SessionSeedSource, StandardSinglesig},
    };
    use bitcoin::sighash::TapSighashType;
    use coincube_core::{
        descriptors::CoincubeDescriptor,
        miniscript::bitcoin::{
            self, bip32::ChildNumber, hashes::Hash, secp256k1, BlockHash, PublicKey,
        },
    };
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };
    use zeroize::Zeroizing;

    /// Test-only observed fork height; production reads it from the anchor.
    const FORK_HEIGHT: u64 = 900;
    const WORDS: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const TARGET_DESC: &str = "wsh(or_d(multi(2,[ffd63c8d/48'/1'/0'/2']tpubDExA3EC3iAsPxPhFn4j6gMiVup6V2eH3qKyk69RcTc9TTNRfFYVPad8bJD5FCHVQxyBT4izKsvr7Btd2R4xmQ1hZkvsqGBaeE82J71uTK4N/<0;1>/*,[de6eb005/48'/1'/0'/2']tpubDFGuYfS2JwiUSEXiQuNGdT3R7WTDhbaE6jbUhgYSSdhmfQcSx7ZntMPPv7nrkvAqjpj3jX9wbhSGMeKVao4qAzhbNyBi7iQmv5xxQk6H6jz/<0;1>/*),and_v(v:pkh([ffd63c8d/48'/1'/0'/2']tpubDExA3EC3iAsPxPhFn4j6gMiVup6V2eH3qKyk69RcTc9TTNRfFYVPad8bJD5FCHVQxyBT4izKsvr7Btd2R4xmQ1hZkvsqGBaeE82J71uTK4N/<2;3>/*),older(3))))#p9ax3xxp";

    fn target_context(cube_id: &str) -> (CoincubeDescriptor, Wallet, CubeSettings) {
        let descriptor = CoincubeDescriptor::from_str(TARGET_DESC).unwrap();
        let wallet = Wallet::new(descriptor.clone())
            .with_chain(ChainId::BitcoinBlake2b)
            .with_pinned_at(Some(42));
        let cube = CubeSettings::new_with_raw_id(
            cube_id.to_owned(),
            "Target".to_owned(),
            ChainId::BitcoinBlake2b,
        )
        .with_vault(VaultIdentity::new(wallet.id(), Some(&descriptor)));
        (descriptor, wallet, cube)
    }

    fn target_evidence(cube_id: &str, index: u32, generation: u64) -> TargetAddressEvidence {
        let (descriptor, wallet, cube) = target_context(cube_id);
        TargetAddressEvidence::authenticate(
            &cube,
            &wallet,
            &fresh_reservation(&descriptor, index),
            generation,
        )
        .unwrap()
    }

    fn receive_address(descriptor: &CoincubeDescriptor, index: u32) -> bitcoin::Address {
        let secp = secp256k1::Secp256k1::verification_only();
        descriptor
            .receive_descriptor()
            .derive(ChildNumber::from_normal_idx(index).unwrap(), &secp)
            .address(bitcoin::Network::Bitcoin)
    }

    /// Reservation time used by the fixtures (Unix seconds).
    const RESERVED_AT: u32 = 1_800_000_000;

    /// A synced daemon that has just reserved receive `index`, polled after
    /// the reservation, and knows no coin in the Vault.
    fn fresh_reservation(descriptor: &CoincubeDescriptor, index: u32) -> TargetReservation {
        reservation_at(
            descriptor,
            index,
            receive_address(descriptor, index),
            Vec::new(),
        )
    }

    fn reservation_at(
        descriptor: &CoincubeDescriptor,
        index: u32,
        address: bitcoin::Address,
        coins: Vec<ListCoinsEntry>,
    ) -> TargetReservation {
        TargetReservation {
            reserved: GetAddressResult::new(address, ChildNumber::from_normal_idx(index).unwrap()),
            requested_at: RESERVED_AT,
            info: GetInfoResult {
                version: String::new(),
                network: bitcoin::Network::Bitcoin,
                block_height: 1_000,
                sync: 1.0,
                descriptors: coincubed::commands::GetInfoDescriptors {
                    main: descriptor.clone(),
                },
                rescan_progress: None,
                refused_reorg_depth: None,
                chain_divergence: false,
                timestamp: 0,
                last_poll_timestamp: Some(RESERVED_AT + 1),
                receive_index: index,
                change_index: 0,
            },
            coins,
        }
    }

    fn vault_coin(
        descriptor: &CoincubeDescriptor,
        index: u32,
        is_change: bool,
        spent: bool,
    ) -> ListCoinsEntry {
        let secp = secp256k1::Secp256k1::verification_only();
        let child = ChildNumber::from_normal_idx(index).unwrap();
        let branch = if is_change {
            descriptor.change_descriptor()
        } else {
            descriptor.receive_descriptor()
        };
        ListCoinsEntry {
            amount: Amount::from_sat(5_000),
            outpoint: OutPoint::new(bitcoin::Txid::from_byte_array([index as u8; 32]), 0),
            address: branch
                .derive(child, &secp)
                .address(bitcoin::Network::Bitcoin),
            block_height: Some(800),
            derivation_index: child,
            spend_info: spent.then_some(coincubed::commands::LCSpendInfo {
                txid: bitcoin::Txid::from_byte_array([0xee; 32]),
                height: Some(801),
            }),
            is_immature: false,
            is_change,
            is_from_self: false,
        }
    }

    fn fixture() -> (
        PreparedForeignSweep,
        SessionSeedSource,
        TargetAddressEvidence,
    ) {
        let source = SessionSeedSource::new(
            Zeroizing::new(WORDS.to_owned()),
            Zeroizing::new("session passphrase".to_owned()),
        )
        .unwrap();
        let descriptors = source.descriptors(StandardSinglesig::Bip84, 0).unwrap();
        let script = descriptors.external.script(7).unwrap();
        let previous = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(100_000),
                script_pubkey: script,
            }],
        };
        let coin = DiscoveredCoin {
            branch: Branch::External,
            index: 7,
            outpoint: OutPoint::new(previous.compute_txid(), 0),
            output: previous.output[0].clone(),
            previous,
            confirmed: true,
            block_height: Some(FORK_HEIGHT as u32 - 1),
            block_hash: Some(BlockHash::from_byte_array([3; 32])),
        };
        let report = ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            9,
            BlockHash::from_byte_array([2; 32]),
            vec![coin],
        )
        .with_fork_height(Some(FORK_HEIGHT));
        let target = target_evidence("vault-a", 11, 9);
        let prepared = PreparedForeignSweep::construct(
            &report,
            ForeignSession {
                chain: ChainId::BitcoinBlake2b,
                generation: 9,
                target: &target,
                external: &descriptors.external,
                internal: Some(&descriptors.internal),
            },
            Amount::from_sat(90_000),
            Amount::from_sat(1_000),
            Some(ForeignChange {
                index: 4,
                amount: Amount::from_sat(9_000),
            }),
        )
        .unwrap();
        (prepared, source, target)
    }

    fn signed_text(prepared: &PreparedForeignSweep) -> String {
        let mut psbt = Psbt::from_str(&prepared.export_text()).unwrap();
        let secret = secp256k1::SecretKey::from_slice(&[7; 32]).unwrap();
        let secp = secp256k1::Secp256k1::new();
        let signature = secp.sign_ecdsa(&secp256k1::Message::from_digest([8; 32]), &secret);
        let expected_key = *psbt.inputs[0].bip32_derivation.keys().next().unwrap();
        psbt.inputs[0].partial_sigs.insert(
            PublicKey::new(expected_key),
            bitcoin::ecdsa::Signature {
                signature,
                sighash_type: EcdsaSighashType::All,
            },
        );
        psbt.to_string()
    }

    fn economics_fixture(
        value: u64,
        confirmed: bool,
    ) -> (ScanReport, AccountDescriptors, TargetAddressEvidence) {
        economics_fixture_at(value, confirmed.then_some(FORK_HEIGHT as u32 - 1))
    }

    /// One coin at `height` (`None` = unconfirmed) with the fork observed at
    /// [`FORK_HEIGHT`].
    fn economics_fixture_at(
        value: u64,
        height: Option<u32>,
    ) -> (ScanReport, AccountDescriptors, TargetAddressEvidence) {
        let source = SessionSeedSource::new(
            Zeroizing::new(WORDS.to_owned()),
            Zeroizing::new("economics passphrase".to_owned()),
        )
        .unwrap();
        let descriptors = source.descriptors(StandardSinglesig::Bip84, 0).unwrap();
        let script = descriptors.external.script(2).unwrap();
        let previous = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(value),
                script_pubkey: script,
            }],
        };
        let report = ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            21,
            BlockHash::from_byte_array([4; 32]),
            vec![DiscoveredCoin {
                branch: Branch::External,
                index: 2,
                outpoint: OutPoint::new(previous.compute_txid(), 0),
                output: previous.output[0].clone(),
                previous,
                confirmed: height.is_some(),
                block_height: height,
                block_hash: height.map(|_| BlockHash::from_byte_array([6; 32])),
            }],
        )
        .with_fork_height(Some(FORK_HEIGHT));
        (report, descriptors, target_evidence("vault-a", 13, 21))
    }

    #[test]
    fn review_economics_is_bounded_and_uses_maximum_signed_vsize() {
        let (report, descriptors, target) = economics_fixture(100_000, true);
        let review = review_sweep_economics(
            &report,
            ForeignSession {
                chain: ChainId::BitcoinBlake2b,
                generation: 21,
                target: &target,
                external: &descriptors.external,
                internal: Some(&descriptors.internal),
            },
            5,
        )
        .unwrap();
        assert_eq!(review.inputs, 1);
        assert!(review.maximum_signed_vbytes > 41);
        assert_eq!(
            review.fee.to_sat(),
            review.maximum_signed_vbytes * review.feerate_sat_vb
        );
        assert_eq!(review.destination.to_sat() + review.fee.to_sat(), 100_000);

        for invalid in [0, spend::MAX_FEERATE + 1] {
            assert_eq!(
                review_sweep_economics(
                    &report,
                    ForeignSession {
                        chain: ChainId::BitcoinBlake2b,
                        generation: 21,
                        target: &target,
                        external: &descriptors.external,
                        internal: Some(&descriptors.internal),
                    },
                    invalid,
                )
                .unwrap_err(),
                ForeignPsbtError::Economics
            );
        }
    }

    #[test]
    fn review_economics_refuses_unconfirmed_and_dust_remainders() {
        let (unconfirmed, descriptors, target) = economics_fixture(100_000, false);
        assert_eq!(
            review_sweep_economics(
                &unconfirmed,
                ForeignSession {
                    chain: ChainId::BitcoinBlake2b,
                    generation: 21,
                    target: &target,
                    external: &descriptors.external,
                    internal: Some(&descriptors.internal),
                },
                5,
            )
            .unwrap_err(),
            ForeignPsbtError::Unconfirmed
        );

        let (small, descriptors, target) = economics_fixture(1_000, true);
        assert_eq!(
            review_sweep_economics(
                &small,
                ForeignSession {
                    chain: ChainId::BitcoinBlake2b,
                    generation: 21,
                    target: &target,
                    external: &descriptors.external,
                    internal: Some(&descriptors.internal),
                },
                spend::MAX_FEERATE,
            )
            .unwrap_err(),
            ForeignPsbtError::Economics
        );
    }

    #[test]
    fn signed_text_round_trips_and_preserves_bound_economics() {
        let (prepared, source, target) = fixture();
        let descriptors = source.descriptors(StandardSinglesig::Bip84, 0).unwrap();
        let verified = prepared
            .import_text(
                &signed_text(&prepared),
                ForeignSession {
                    chain: ChainId::BitcoinBlake2b,
                    generation: 9,
                    target: &target,
                    external: &descriptors.external,
                    internal: Some(&descriptors.internal),
                },
            )
            .unwrap();
        assert_eq!(prepared.fee(), Amount::from_sat(1_000));
        assert_eq!(
            verified.psbt().unsigned_tx.output[0].value,
            Amount::from_sat(90_000)
        );
        assert_eq!(
            verified.psbt().unsigned_tx.output[0].script_pubkey,
            target.script_pubkey
        );
        assert_eq!(
            verified.psbt().unsigned_tx.output[1].value,
            Amount::from_sat(9_000)
        );
    }

    #[test]
    fn taproot_fields_on_import_refuse() {
        let (prepared, source, target) = fixture();
        let descriptors = source.descriptors(StandardSinglesig::Bip84, 0).unwrap();
        let secp = secp256k1::Secp256k1::new();
        let keypair = secp256k1::Keypair::from_secret_key(
            &secp,
            &secp256k1::SecretKey::from_slice(&[7; 32]).unwrap(),
        );
        let (xonly, _) = keypair.x_only_public_key();
        let signature = bitcoin::taproot::Signature {
            signature: secp
                .sign_schnorr_no_aux_rand(&secp256k1::Message::from_digest([8; 32]), &keypair),
            sighash_type: TapSighashType::All,
        };
        let leaf = bitcoin::taproot::TapLeafHash::from_byte_array([9; 32]);
        let mut variants = Vec::new();
        // Before #568 A1 a key-path signature with an internal key was
        // accepted as signature evidence.
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.inputs[0].tap_internal_key = Some(xonly);
        psbt.inputs[0].tap_key_sig = Some(signature);
        variants.push(psbt);
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.inputs[0]
            .tap_script_sigs
            .insert((xonly, leaf), signature);
        variants.push(psbt);
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.inputs[0].tap_internal_key = Some(xonly);
        variants.push(psbt);
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.outputs[1].tap_internal_key = Some(xonly);
        variants.push(psbt);
        for variant in variants {
            assert_eq!(
                prepared
                    .import_text(
                        &variant.to_string(),
                        ForeignSession {
                            chain: ChainId::BitcoinBlake2b,
                            generation: 9,
                            target: &target,
                            external: &descriptors.external,
                            internal: Some(&descriptors.internal),
                        },
                    )
                    .unwrap_err(),
                ForeignPsbtError::Taproot
            );
        }
    }

    /// A `tr` key-path source with one confirmed pre-fork coin. Scan accepts
    /// it; every signing-side path must refuse it.
    fn taproot_fixture() -> (ScanReport, ScanDescriptor, TargetAddressEvidence) {
        let secp = secp256k1::Secp256k1::new();
        let xpub = bitcoin::bip32::Xpub::from_priv(
            &secp,
            &bitcoin::bip32::Xpriv::new_master(bitcoin::Network::Bitcoin, &[42; 32]).unwrap(),
        );
        let tr = ScanDescriptor::parse(ScanBranch::External, &format!("tr({xpub}/0/*)")).unwrap();
        let previous = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(100_000),
                script_pubkey: tr.script(0).unwrap(),
            }],
        };
        let report = ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            21,
            BlockHash::from_byte_array([4; 32]),
            vec![DiscoveredCoin {
                branch: ScanBranch::External,
                index: 0,
                outpoint: OutPoint::new(previous.compute_txid(), 0),
                output: previous.output[0].clone(),
                previous,
                confirmed: true,
                block_height: Some(FORK_HEIGHT as u32 - 1),
                block_hash: Some(BlockHash::from_byte_array([6; 32])),
            }],
        )
        .with_fork_height(Some(FORK_HEIGHT));
        (report, tr, target_evidence("vault-a", 13, 21))
    }

    #[test]
    fn taproot_source_refuses_economics_and_construction() {
        let (report, tr, target) = taproot_fixture();
        let session = || ForeignSession {
            chain: ChainId::BitcoinBlake2b,
            generation: 21,
            target: &target,
            external: &tr,
            internal: None,
        };
        assert_eq!(
            review_sweep_inputs(&report, session()).unwrap_err(),
            ForeignPsbtError::UnsupportedRoute
        );
        assert_eq!(
            review_sweep_economics(&report, session(), 5).unwrap_err(),
            ForeignPsbtError::UnsupportedRoute
        );
        assert_eq!(
            PreparedForeignSweep::construct(
                &report,
                session(),
                Amount::from_sat(99_000),
                Amount::from_sat(1_000),
                None,
            )
            .err(),
            Some(ForeignPsbtError::UnsupportedRoute)
        );
    }

    #[test]
    fn only_pre_fork_coins_are_reviewed_and_unknown_fork_height_refuses() {
        let (pre, descriptors, target) = economics_fixture_at(100_000, Some(899));
        let make = || ForeignSession {
            chain: ChainId::BitcoinBlake2b,
            generation: 21,
            target: &target,
            external: &descriptors.external,
            internal: Some(&descriptors.internal),
        };
        let review = review_sweep_inputs(&pre, make()).unwrap();
        assert_eq!((review.inputs, review.excluded_post_fork), (1, 0));

        // Confirmed at the fork height: excluded, nothing left.
        let (post, ..) = economics_fixture_at(100_000, Some(FORK_HEIGHT as u32));
        assert_eq!(
            review_sweep_inputs(&post, make()).unwrap_err(),
            ForeignPsbtError::Empty
        );
        assert_eq!(
            PreparedForeignSweep::construct(
                &post,
                make(),
                Amount::from_sat(99_000),
                Amount::from_sat(1_000),
                None,
            )
            .err(),
            Some(ForeignPsbtError::Empty)
        );

        // Mixed: the post-fork coin is counted as excluded, not swept.
        let mut coins = pre.coins().to_vec();
        let mut late = post.coins()[0].clone();
        late.outpoint.vout = 1;
        late.previous.output.push(late.output.clone());
        late.outpoint.txid = late.previous.compute_txid();
        coins.push(late);
        let mixed = ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            21,
            BlockHash::from_byte_array([4; 32]),
            coins.clone(),
        )
        .with_fork_height(Some(FORK_HEIGHT));
        let review = review_sweep_inputs(&mixed, make()).unwrap();
        assert_eq!((review.inputs, review.excluded_post_fork), (1, 1));
        assert_eq!(review.total, Amount::from_sat(100_000));

        // A confirmed coin without a confirming height is excluded.
        let mut heightless = coins[0].clone();
        heightless.block_height = None;
        let unknown = ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            21,
            BlockHash::from_byte_array([4; 32]),
            vec![heightless],
        )
        .with_fork_height(Some(FORK_HEIGHT));
        assert_eq!(
            review_sweep_inputs(&unknown, make()).unwrap_err(),
            ForeignPsbtError::Empty
        );

        // No observed fork height: fail closed, even for an old coin.
        let unobserved = pre.clone().with_fork_height(None);
        assert_eq!(
            review_sweep_economics(&unobserved, make(), 5).unwrap_err(),
            ForeignPsbtError::ForkUnknown
        );
        assert_eq!(
            PreparedForeignSweep::construct(
                &unobserved,
                make(),
                Amount::from_sat(99_000),
                Amount::from_sat(1_000),
                None,
            )
            .err(),
            Some(ForeignPsbtError::ForkUnknown)
        );
    }

    struct CountingFees {
        chain: ChainId,
        rate: Option<u64>,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl SweepFeeSource for CountingFees {
        fn chain(&self) -> ChainId {
            self.chain
        }
        async fn mid_priority_sat_vb(&self) -> Option<u64> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.rate
        }
    }

    #[tokio::test]
    async fn btcb2_sweep_never_queries_a_bitcoin_fee_source() {
        use std::sync::atomic::Ordering;
        let bitcoin = CountingFees {
            chain: ChainId::Bitcoin,
            rate: Some(5),
            calls: Default::default(),
        };
        assert_eq!(btcb2_sweep_feerate(&bitcoin).await, None);
        assert_eq!(bitcoin.calls.load(Ordering::SeqCst), 0);

        let btcb2 = CountingFees {
            chain: ChainId::BitcoinBlake2b,
            rate: Some(5),
            calls: Default::default(),
        };
        assert_eq!(btcb2_sweep_feerate(&btcb2).await, Some(5));
        assert_eq!(btcb2.calls.load(Ordering::SeqCst), 1);
        for rate in [Some(0), Some(spend::MAX_FEERATE + 1), None] {
            let bounded = CountingFees {
                chain: ChainId::BitcoinBlake2b,
                rate,
                calls: Default::default(),
            };
            assert_eq!(btcb2_sweep_feerate(&bounded).await, None);
        }
        // Production has no BTCB2 estimator: fees are unavailable.
        assert_eq!(btcb2_sweep_feerate(&UnavailableBtcb2Fees).await, None);
    }

    #[test]
    fn taproot_key_signature_on_p2wpkh_input_refuses() {
        let (prepared, source, target) = fixture();
        let descriptors = source.descriptors(StandardSinglesig::Bip84, 0).unwrap();
        let mut psbt = Psbt::from_str(&prepared.export_text()).unwrap();
        assert!(psbt.inputs[0]
            .witness_utxo
            .as_ref()
            .unwrap()
            .script_pubkey
            .is_p2wpkh());
        assert!(psbt.inputs[0].tap_internal_key.is_none());

        let secp = secp256k1::Secp256k1::new();
        let keypair = secp256k1::Keypair::from_secret_key(
            &secp,
            &secp256k1::SecretKey::from_slice(&[7; 32]).unwrap(),
        );
        psbt.inputs[0].tap_key_sig = Some(bitcoin::taproot::Signature {
            signature: secp
                .sign_schnorr_no_aux_rand(&secp256k1::Message::from_digest([8; 32]), &keypair),
            sighash_type: TapSighashType::All,
        });

        assert_eq!(
            prepared
                .import_text(
                    &psbt.to_string(),
                    ForeignSession {
                        chain: ChainId::BitcoinBlake2b,
                        generation: 9,
                        target: &target,
                        external: &descriptors.external,
                        internal: Some(&descriptors.internal),
                    },
                )
                .unwrap_err(),
            ForeignPsbtError::Taproot
        );
    }

    #[test]
    fn transaction_and_metadata_tampering_refuse() {
        let (prepared, source, target) = fixture();
        let descriptors = source.descriptors(StandardSinglesig::Bip84, 0).unwrap();
        let mut variants = Vec::new();
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.unsigned_tx.output[0].value = Amount::from_sat(1);
        variants.push(psbt);
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.unsigned_tx.output[1].script_pubkey = bitcoin::ScriptBuf::new();
        variants.push(psbt);
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.unsigned_tx.input[0].previous_output.vout = 1;
        variants.push(psbt);
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.inputs[0].witness_utxo.as_mut().unwrap().value = Amount::from_sat(1);
        variants.push(psbt);
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.inputs[0].non_witness_utxo = None;
        variants.push(psbt);
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.inputs[0].bip32_derivation.clear();
        variants.push(psbt);
        for variant in variants {
            assert!(prepared
                .import_text(
                    &variant.to_string(),
                    ForeignSession {
                        chain: ChainId::BitcoinBlake2b,
                        generation: 9,
                        target: &target,
                        external: &descriptors.external,
                        internal: Some(&descriptors.internal)
                    }
                )
                .is_err());
        }

        let mut sighash_tamper = Psbt::from_str(&signed_text(&prepared)).unwrap();
        assert!(sighash_tamper.inputs[0].sighash_type.is_none());
        sighash_tamper.inputs[0].sighash_type = Some(EcdsaSighashType::All.into());
        assert_eq!(
            prepared
                .import_text(
                    &sighash_tamper.to_string(),
                    ForeignSession {
                        chain: ChainId::BitcoinBlake2b,
                        generation: 9,
                        target: &target,
                        external: &descriptors.external,
                        internal: Some(&descriptors.internal),
                    }
                )
                .unwrap_err(),
            ForeignPsbtError::ConstructionChanged
        );
    }

    #[test]
    fn stale_target_chain_and_source_refuse() {
        let (prepared, source, target) = fixture();
        let descriptors = source.descriptors(StandardSinglesig::Bip84, 0).unwrap();
        let text = signed_text(&prepared);
        let wrong_cube = target_evidence("vault-b", 11, 9);
        let stale_address = target_evidence("vault-a", 12, 9);
        assert_eq!(
            prepared
                .import_text(
                    &text,
                    ForeignSession {
                        chain: ChainId::BitcoinBlake2b,
                        generation: 10,
                        target: &target,
                        external: &descriptors.external,
                        internal: Some(&descriptors.internal)
                    }
                )
                .unwrap_err(),
            ForeignPsbtError::StaleSession
        );
        assert_eq!(
            prepared
                .import_text(
                    &text,
                    ForeignSession {
                        chain: ChainId::BitcoinBlake2b,
                        generation: 9,
                        target: &wrong_cube,
                        external: &descriptors.external,
                        internal: Some(&descriptors.internal)
                    }
                )
                .unwrap_err(),
            ForeignPsbtError::TargetChanged
        );
        assert_eq!(
            prepared
                .import_text(
                    &text,
                    ForeignSession {
                        chain: ChainId::BitcoinBlake2b,
                        generation: 9,
                        target: &stale_address,
                        external: &descriptors.external,
                        internal: Some(&descriptors.internal)
                    }
                )
                .unwrap_err(),
            ForeignPsbtError::StaleAddress
        );
        let other = SessionSeedSource::new(
            Zeroizing::new(WORDS.to_owned()),
            Zeroizing::new("other".to_owned()),
        )
        .unwrap()
        .descriptors(StandardSinglesig::Bip84, 0)
        .unwrap();
        assert_eq!(
            prepared
                .import_text(
                    &text,
                    ForeignSession {
                        chain: ChainId::BitcoinBlake2b,
                        generation: 9,
                        target: &target,
                        external: &other.external,
                        internal: Some(&other.internal)
                    }
                )
                .unwrap_err(),
            ForeignPsbtError::SourceChanged
        );
        assert_eq!(
            prepared
                .import_text(
                    &text,
                    ForeignSession {
                        chain: ChainId::Bitcoin,
                        generation: 9,
                        target: &target,
                        external: &descriptors.external,
                        internal: Some(&descriptors.internal)
                    }
                )
                .unwrap_err(),
            ForeignPsbtError::UnsupportedChain
        );
    }

    #[test]
    fn target_authentication_rejects_wrong_vault_and_arbitrary_address() {
        let (descriptor, wallet, cube) = target_context("vault-a");

        let wrong_wallet = Wallet::new(descriptor.clone())
            .with_chain(ChainId::BitcoinBlake2b)
            .with_pinned_at(Some(43));
        assert_eq!(
            TargetAddressEvidence::authenticate(
                &cube,
                &wrong_wallet,
                &fresh_reservation(&descriptor, 11),
                9,
            )
            .unwrap_err(),
            ForeignPsbtError::TargetChanged
        );

        let arbitrary = receive_address(&descriptor, 12);
        assert_eq!(
            TargetAddressEvidence::authenticate(
                &cube,
                &wallet,
                &reservation_at(&descriptor, 11, arbitrary, Vec::new()),
                9,
            )
            .unwrap_err(),
            ForeignPsbtError::TargetAddress
        );
    }

    /// #571 P3-2: owning the address is not enough. A reserved receive
    /// address the Vault has already used (by any coin status, on either
    /// matching evidence) is refused, and so is any daemon view that cannot
    /// prove it unused.
    #[test]
    fn target_authentication_requires_a_provably_unused_address() {
        let (descriptor, wallet, cube) = target_context("vault-a");
        let authenticate = |reservation: &TargetReservation| {
            TargetAddressEvidence::authenticate(&cube, &wallet, reservation, 9)
        };
        let fresh = fresh_reservation(&descriptor, 11);
        let evidence = authenticate(&fresh).unwrap();
        assert_eq!(
            evidence.derivation_index(),
            ChildNumber::from_normal_idx(11).unwrap()
        );

        // Coins elsewhere in the Vault do not make index 11 used: other
        // receive indexes, and a change coin at the same numeric index
        // (a different script).
        let mut unrelated = fresh.clone();
        unrelated.coins = vec![
            vault_coin(&descriptor, 10, false, true),
            vault_coin(&descriptor, 12, false, false),
            vault_coin(&descriptor, 11, true, false),
        ];
        unrelated.info.receive_index = 12;
        assert_eq!(authenticate(&unrelated).unwrap(), evidence);

        // A used address: an unspent, then a spent coin at the reserved index.
        for spent in [false, true] {
            let mut used = fresh.clone();
            used.coins = vec![vault_coin(&descriptor, 11, false, spent)];
            assert_eq!(
                authenticate(&used).unwrap_err(),
                ForeignPsbtError::TargetUsed,
                "spent={}",
                spent
            );
        }
        // Either match alone is use: the script (whatever index the daemon
        // recorded) or the receive index (whatever address it reported).
        let mut by_script = fresh.clone();
        let mut coin = vault_coin(&descriptor, 11, false, false);
        coin.derivation_index = ChildNumber::from_normal_idx(500).unwrap();
        by_script.coins = vec![coin];
        assert_eq!(
            authenticate(&by_script).unwrap_err(),
            ForeignPsbtError::TargetUsed
        );
        let mut by_index = fresh.clone();
        let mut coin = vault_coin(&descriptor, 11, false, false);
        coin.address = receive_address(&descriptor, 500);
        by_index.coins = vec![coin];
        assert_eq!(
            authenticate(&by_index).unwrap_err(),
            ForeignPsbtError::TargetUsed
        );

        // The daemon cannot prove freshness.
        let unproven: [fn(&mut GetInfoResult); 9] = [
            // #592 F1: Electrum/Esplora report sync 1.0 before any poll. A
            // daemon that never polled, or last polled before the
            // reservation, has not looked at the chain for this address.
            |i| i.last_poll_timestamp = None,
            |i| i.last_poll_timestamp = Some(RESERVED_AT - 1),
            |i| i.sync = 0.999,
            |i| i.sync = f64::NAN,
            |i| i.rescan_progress = Some(0.5),
            |i| i.refused_reorg_depth = Some(7),
            |i| i.chain_divergence = true,
            // The reservation is not recorded by this daemon.
            |i| i.receive_index = 10,
            |i| i.receive_index = 0,
        ];
        for (n, mutate) in unproven.iter().enumerate() {
            let mut reservation = fresh.clone();
            mutate(&mut reservation.info);
            assert_eq!(
                authenticate(&reservation).unwrap_err(),
                ForeignPsbtError::TargetFreshnessUnknown,
                "case {}",
                n
            );
        }
        // A poll in the same second as the reservation, or later, counts.
        for polled in [RESERVED_AT, RESERVED_AT + 60] {
            let mut reservation = fresh.clone();
            reservation.info.last_poll_timestamp = Some(polled);
            assert_eq!(authenticate(&reservation).unwrap(), evidence);
        }
        // The history belongs to another descriptor.
        let mut other = fresh.clone();
        other.info.descriptors.main = CoincubeDescriptor::from_str(
            "wsh(or_d(pk([ffd63c8d/48'/1'/0'/2']tpubDExA3EC3iAsPxPhFn4j6gMiVup6V2eH3qKyk69RcTc9TTNRfFYVPad8bJD5FCHVQxyBT4izKsvr7Btd2R4xmQ1hZkvsqGBaeE82J71uTK4N/<0;1>/*),and_v(v:pkh([de6eb005/48'/1'/0'/2']tpubDFGuYfS2JwiUSEXiQuNGdT3R7WTDhbaE6jbUhgYSSdhmfQcSx7ZntMPPv7nrkvAqjpj3jX9wbhSGMeKVao4qAzhbNyBi7iQmv5xxQk6H6jz/<0;1>/*),older(3))))",
        )
        .unwrap();
        assert_eq!(
            authenticate(&other).unwrap_err(),
            ForeignPsbtError::TargetChanged
        );
    }

    #[test]
    fn seed_psbt_round_trip_never_writes_the_seed() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let datadir = std::env::temp_dir().join(format!("coincube-split-psbt-{unique}"));
        fs::create_dir(&datadir).unwrap();
        let (prepared, source, target) = fixture();
        let descriptors = source.descriptors(StandardSinglesig::Bip84, 0).unwrap();
        prepared
            .import_text(
                &signed_text(&prepared),
                ForeignSession {
                    chain: ChainId::BitcoinBlake2b,
                    generation: 9,
                    target: &target,
                    external: &descriptors.external,
                    internal: Some(&descriptors.internal),
                },
            )
            .unwrap();
        drop(source);
        assert_eq!(fs::read_dir(&datadir).unwrap().count(), 0);
        fs::remove_dir(datadir).unwrap();
    }
}
