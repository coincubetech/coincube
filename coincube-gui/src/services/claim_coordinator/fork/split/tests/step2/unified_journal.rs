//! Split (#568 B4b-1b, B4b-3a) fork-only (`kind: Unified`) journals and the
//! step-2 reconciler. The fork-only record reuses the step-2 fields, but its
//! reconcile is the fork-only path's (`UnifiedReconciler`, U1/U2): the
//! step-2 reconciler refuses to reopen it at all, whatever it holds, and so
//! never runs its step-1-centric observation on it. Also the shared
//! unified-sweep fixtures of the `unified_flow` tests.
use super::*;
use crate::services::{
    claim_coordinator::fork::split::step2::SplitStep2Reconciler, claim_observation::FailureKind,
};
use coincube_core::{
    bip39::Mnemonic,
    foreign_split::{create_unified_sweep, finalize_unified_sweep, UnifiedInputs, UnifiedSweep},
    miniscript::bitcoin::{hashes::sha256, Script},
    psbt_unified::UnifiedPsbt,
    signer::SessionSigner,
};

/// A P2PKH foreign wallet whose key comes from a mnemonic, so a session
/// signer can sign `ALL|UNIFIED` for it; its scriptSigs change the txid.
pub(super) struct UnifiedWallet {
    pub(super) source: SplitSource,
    mnemonic: Mnemonic,
}
pub(super) fn unified_wallet() -> UnifiedWallet {
    let secp = Secp256k1::new();
    let mnemonic = Mnemonic::from_entropy(&[4; 16]).unwrap();
    let master = Xpriv::new_master(Network::Bitcoin, &mnemonic.to_seed("")).unwrap();
    let path = "m/44'/0'/0'";
    let child = master
        .derive_priv(&secp, &DerivationPath::from_str(path).unwrap())
        .unwrap();
    let key = format!(
        "[{}/{}]{}",
        master.fingerprint(&secp),
        path.trim_start_matches("m/"),
        Xpub::from_priv(&secp, &child)
    );
    let branch = |b: u32| Descriptor::from_str(&format!("pkh({key}/{b}/*)")).unwrap();
    UnifiedWallet {
        source: SplitSource::new(branch(0), Some(branch(1))).unwrap(),
        mnemonic,
    }
}
/// The wallet's two splittable coins, freshly authenticated (pre-fork on
/// both chains).
pub(super) fn unified_coins(wallet: &UnifiedWallet) -> Vec<SplitCoin> {
    vec![
        coin(&wallet.source, SplitBranch::External, 0, 150_000),
        coin(&wallet.source, SplitBranch::Internal, 1, 70_000),
    ]
}
/// Core's unified sweep of those coins into `target` at BTCB2 tip `tip`.
pub(super) fn unified_sweep(wallet: &UnifiedWallet, target: &Script, tip: u32) -> UnifiedSweep {
    create_unified_sweep(
        &UnifiedInputs {
            chain: ChainId::BitcoinBlake2b,
            source: &wallet.source,
            coins: &unified_coins(wallet),
            fork_height: FORK,
            target,
        },
        2,
        LockTime::from_height(tip).unwrap(),
        tip,
    )
    .unwrap()
}
/// `psbt` signed `ALL|UNIFIED` by the wallet's seed.
pub(super) fn sign_unified(wallet: &UnifiedWallet, psbt: &Psbt) -> UnifiedPsbt {
    let signer =
        SessionSigner::from_mnemonic(Network::Bitcoin, wallet.mnemonic.clone(), "").unwrap();
    signer
        .sign_unified(
            &UnifiedPsbt::from_psbt(psbt.clone()).unwrap(),
            ChainId::BitcoinBlake2b,
            &Secp256k1::new(),
        )
        .unwrap()
}

/// Services nothing here may reach: every read panics.
pub(super) struct Unreachable;
#[async_trait]
impl ObservationSource for Unreachable {
    fn now(&self) -> i64 {
        unreachable!("no read is made")
    }
    async fn anchor(&self, _: ChainId) -> Result<NetworkAnchorStatus, FailureKind> {
        unreachable!("no read is made")
    }
    async fn tip(&self, _: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
        unreachable!("no read is made")
    }
    async fn transaction(
        &self,
        _: ChainId,
        _: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        unreachable!("no read is made")
    }
    async fn hash_at_height(
        &self,
        _: ChainId,
        _: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        unreachable!("no read is made")
    }
}
#[async_trait]
impl SplitForkServices for Unreachable {
    fn source(&self) -> &dyn ObservationSource {
        self
    }
    fn origin(&self) -> &str {
        ORIGIN
    }
    async fn btcb2_unspent(&self, _: &str) -> Result<FreshRead<Vec<OutPoint>>, FailureKind> {
        unreachable!("no read is made")
    }
    async fn address_used(&self, _: ChainId, _: &str) -> Result<FreshRead<bool>, FailureKind> {
        unreachable!("no read is made")
    }
}

/// A fork-only journal of the unified wallet into the Vault's receive
/// address at index 5, its source digest, and its signed sweep, which core
/// verified Protected; the submission is recorded when `submitted`.
pub(super) fn fork_only_journal(submitted: bool) -> (Temp, sha256::Hash, Transaction) {
    let wallet = unified_wallet();
    let target = address(&vault(), 5).script_pubkey();
    let sweep = unified_sweep(&wallet, &target, 100);
    let verified = finalize_unified_sweep(
        &sweep,
        &sign_unified(&wallet, sweep.psbt()),
        &Secp256k1::verification_only(),
    )
    .unwrap();
    let signed = verified.transaction().clone();
    let temp = Temp::new();
    let mut controller =
        Controller::create_unified_split(&temp.0, TARGET.into(), &sweep, 5, context()).unwrap();
    assert_ne!(signed.compute_txid(), sweep.txid(), "pkh: the txids differ");
    if submitted {
        controller
            .record_unified_broadcast_intent(&context(), &verified)
            .unwrap();
    }
    (temp, wallet.source.digest(), signed)
}
pub(super) fn reconciler(
    temp: &Temp,
    digest: sha256::Hash,
    sender: &watch::Sender<u64>,
) -> Result<SplitStep2Reconciler, Error> {
    SplitStep2Reconciler::open(
        &temp.0,
        TARGET.into(),
        digest,
        context(),
        sender.subscribe(),
        Box::new(Unreachable),
        policy(),
    )
}

/// B4b-3a (U2, replacing #650's admission pins): the step-2 reconciler
/// refuses a fork-only journal with `InvalidBinding`, with or without a
/// recorded submission, before any read (its services panic on any), and
/// releases it: the record, its submission and its signed sweep are kept
/// and nothing is observed. The fork-only reconcile is `UnifiedReconciler`'s
/// (`unified_flow`).
#[test]
fn step2_reconciler_refuses_a_fork_only_journal() {
    let (sender, _) = watch::channel(7);
    for submitted in [false, true] {
        let (temp, digest, signed) = fork_only_journal(submitted);
        let journal = temp.journal();
        assert!(
            matches!(
                reconciler(&temp, digest, &sender),
                Err(Error::InvalidBinding)
            ),
            "submitted: {}",
            submitted
        );
        assert_eq!(temp.journal(), journal);
        let controller = Controller::reopen(
            &temp.0,
            &claim_workflow::split_identity(TARGET.into(), digest),
            context(),
        )
        .unwrap();
        assert!(!controller.split_step2_observed());
        if submitted {
            assert_eq!(
                controller.recorded_fork_submission().map(|s| s.txid()),
                Some(signed.compute_txid())
            );
            assert_eq!(controller.recorded_split_step2(), Some(&signed));
        } else {
            assert_eq!(controller.recorded_fork_submission(), None);
        }
    }
}
