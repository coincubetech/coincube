use std::{
    fs,
    str::FromStr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use async_hwi::{DeviceKind, HWI};
use coincube_core::miniscript::{
    bitcoin::{
        absolute::LockTime,
        bip32::{DerivationPath, Fingerprint, Xpriv, Xpub},
        hashes::{sha256, Hash},
        psbt::PsbtSighashType,
        secp256k1::{self, Secp256k1},
        sighash::EcdsaSighashType,
        transaction::Version,
        Amount, Network, OutPoint, Psbt, ScriptBuf, Transaction, TxIn, TxOut,
    },
    psbt::PsbtExt,
    Descriptor, DescriptorPublicKey,
};

use super::*;
use crate::{dir::CoincubeDirectory, hw::HardwareWallets};

fn secp() -> Secp256k1<secp256k1::All> {
    Secp256k1::new()
}

fn master(seed: u8) -> Xpriv {
    Xpriv::new_master(Network::Bitcoin, &[seed; 32]).unwrap()
}

fn fp(seed: u8) -> Fingerprint {
    master(seed).fingerprint(&secp())
}

fn key_expr(seed: u8, path: &str) -> String {
    let path = DerivationPath::from_str(path).unwrap();
    let xpub = Xpub::from_priv(&secp(), &master(seed).derive_priv(&secp(), &path).unwrap());
    format!(
        "[{}/{}]{}/<0;1>/*",
        fp(seed),
        path.to_string().trim_start_matches("m/"),
        xpub
    )
}

fn desc(s: &str) -> Descriptor<DescriptorPublicKey> {
    Descriptor::from_str(s).unwrap()
}

fn singlesig(script: SinglesigScript, seed: u8) -> Descriptor<DescriptorPublicKey> {
    let (wrap, purpose) = match script {
        SinglesigScript::Pkh => ("pkh(KEY)", 44),
        SinglesigScript::ShWpkh => ("sh(wpkh(KEY))", 49),
        SinglesigScript::Wpkh => ("wpkh(KEY)", 84),
    };
    desc(&wrap.replace("KEY", &key_expr(seed, &format!("m/{purpose}'/0'/0'"))))
}

fn multisig(sorted: bool) -> Descriptor<DescriptorPublicKey> {
    let keys = [1u8, 2, 3]
        .iter()
        .map(|s| key_expr(*s, "m/48'/0'/0'/2'"))
        .collect::<Vec<_>>()
        .join(",");
    let f = if sorted { "sortedmulti" } else { "multi" };
    desc(&format!("wsh({f}(2,{keys}))"))
}

/// A PSBT spending one coin at receive index 0 of `descriptor`, filled the
/// way the Split constructions fill theirs (UTXOs, scripts, derivations).
fn spend(descriptor: &Descriptor<DescriptorPublicKey>) -> Psbt {
    let single = descriptor
        .clone()
        .into_single_descriptors()
        .unwrap()
        .remove(0)
        .at_derivation_index(0)
        .unwrap();
    let prev = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn::default()],
        output: vec![TxOut {
            value: Amount::from_sat(100_000),
            script_pubkey: single.script_pubkey(),
        }],
    };
    let tx = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(prev.compute_txid(), 0),
            ..TxIn::default()
        }],
        output: vec![TxOut {
            value: Amount::from_sat(90_000),
            script_pubkey: ScriptBuf::new_op_return([7u8; 20]),
        }],
    };
    let mut psbt = Psbt::from_unsigned_tx(tx).unwrap();
    psbt.inputs[0].non_witness_utxo = Some(prev.clone());
    if !matches!(descriptor, Descriptor::Pkh(_)) {
        psbt.inputs[0].witness_utxo = Some(prev.output[0].clone());
    }
    psbt.update_input_with_descriptor(0, &single).unwrap();
    psbt
}

/// How the fake keeps a multisig registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Registry {
    /// Jade/Coldcard/BitBox02: the device stores it and can report it.
    DeviceStored,
    /// Ledger: the device returns an HMAC, and only a handle opened with it
    /// can report or use the registration. Signs only through a bound handle.
    Token,
    /// Specter: registration cannot be queried.
    Unqueryable,
}

#[derive(Debug, Default)]
struct Shared {
    stored: Mutex<Vec<(String, String)>>,
    register_calls: AtomicUsize,
    sign_calls: AtomicUsize,
}

#[derive(Debug, Clone)]
struct Fake {
    kind: DeviceKind,
    seed: u8,
    registry: Registry,
    shared: Arc<Shared>,
    bound: Option<(String, String, Option<[u8; 32]>)>,
    sighash: EcdsaSighashType,
    /// Report the sighash only inside each signature, not the input field.
    hide_sighash_field: bool,
    refuse_registration: bool,
}

fn fake(kind: DeviceKind, seed: u8) -> Fake {
    Fake {
        kind,
        seed,
        registry: match kind {
            DeviceKind::Ledger | DeviceKind::LedgerSimulator => Registry::Token,
            DeviceKind::Specter | DeviceKind::SpecterSimulator => Registry::Unqueryable,
            _ => Registry::DeviceStored,
        },
        shared: Arc::default(),
        bound: None,
        sighash: EcdsaSighashType::All,
        hide_sighash_field: false,
        refuse_registration: false,
    }
}

fn token(name: &str, policy: &str) -> [u8; 32] {
    sha256::Hash::hash(format!("hmac|{name}|{policy}").as_bytes()).to_byte_array()
}

impl Fake {
    fn arc(&self) -> Arc<dyn HWI + Send + Sync> {
        Arc::new(self.clone())
    }

    fn registered(&self, name: &str, policy: &str) -> bool {
        match self.registry {
            Registry::Token => self.bound.as_ref().is_some_and(|(n, p, h)| {
                n == name && p == policy && *h == Some(token(name, policy))
            }),
            _ => self
                .shared
                .stored
                .lock()
                .unwrap()
                .contains(&(name.to_string(), policy.to_string())),
        }
    }
}

#[async_trait::async_trait]
impl HWI for Fake {
    fn device_kind(&self) -> DeviceKind {
        self.kind
    }
    async fn get_version(&self) -> Result<async_hwi::Version, async_hwi::Error> {
        Err(async_hwi::Error::UnimplementedMethod)
    }
    async fn get_master_fingerprint(&self) -> Result<Fingerprint, async_hwi::Error> {
        Ok(fp(self.seed))
    }
    async fn get_extended_pubkey(&self, _: &DerivationPath) -> Result<Xpub, async_hwi::Error> {
        Err(async_hwi::Error::UnimplementedMethod)
    }
    async fn register_wallet(
        &self,
        name: &str,
        policy: &str,
    ) -> Result<Option<[u8; 32]>, async_hwi::Error> {
        self.shared.register_calls.fetch_add(1, Ordering::SeqCst);
        if self.refuse_registration {
            return Err(async_hwi::Error::UserRefused);
        }
        match self.registry {
            Registry::Token => Ok(Some(token(name, policy))),
            _ => {
                self.shared
                    .stored
                    .lock()
                    .unwrap()
                    .push((name.to_string(), policy.to_string()));
                Ok(None)
            }
        }
    }
    async fn is_wallet_registered(
        &self,
        name: &str,
        policy: &str,
    ) -> Result<bool, async_hwi::Error> {
        match self.registry {
            Registry::Unqueryable => Err(async_hwi::Error::UnimplementedMethod),
            _ => Ok(self.registered(name, policy)),
        }
    }
    async fn display_address(&self, _: &async_hwi::AddressScript) -> Result<(), async_hwi::Error> {
        Err(async_hwi::Error::UnimplementedMethod)
    }
    async fn sign_tx(&self, psbt: &mut Psbt) -> Result<(), async_hwi::Error> {
        self.shared.sign_calls.fetch_add(1, Ordering::SeqCst);
        if self.registry == Registry::Token && self.bound.is_none() {
            // Ledger cannot sign without a policy.
            return Err(async_hwi::Error::UnimplementedMethod);
        }
        let multisig = psbt.inputs.iter().any(|i| i.witness_script.is_some());
        if multisig {
            let ok = match (&self.registry, &self.bound) {
                (Registry::Token, Some((n, p, _))) => self.registered(n, p),
                _ => !self.shared.stored.lock().unwrap().is_empty(),
            };
            if !ok {
                return Err(async_hwi::Error::Device("policy not registered".into()));
            }
        }
        for input in &mut psbt.inputs {
            input.sighash_type = Some(PsbtSighashType::from(self.sighash));
        }
        let _ = psbt.sign(&master(self.seed), &secp());
        if self.hide_sighash_field {
            for input in &mut psbt.inputs {
                input.sighash_type = None;
            }
        }
        Ok(())
    }
}

/// Opens a bound handle the way a production binder would: same device,
/// constructed with the policy and the session token.
#[derive(Default)]
struct FakeBinder {
    device: Option<Fake>,
    seen: Mutex<Vec<(String, Option<[u8; 32]>)>>,
}

#[async_trait::async_trait]
impl PolicyBinder for FakeBinder {
    async fn bind(
        &self,
        request: BindRequest<'_>,
    ) -> Result<Arc<dyn HWI + Send + Sync>, async_hwi::Error> {
        let mut device = self
            .device
            .clone()
            .ok_or(async_hwi::Error::DeviceNotFound)?;
        assert_eq!(request.kind, device.kind);
        assert_eq!(request.fingerprint, fp(device.seed));
        self.seen
            .lock()
            .unwrap()
            .push((request.name.to_string(), request.hmac.copied()));
        device.bound = Some((
            request.name.to_string(),
            request.policy.to_string(),
            request.hmac.copied(),
        ));
        Ok(Arc::new(device))
    }
}

fn binder(device: &Fake) -> FakeBinder {
    FakeBinder {
        device: Some(device.clone()),
        ..FakeBinder::default()
    }
}

fn sig_count(psbt: &Psbt) -> usize {
    psbt.inputs.iter().map(|i| i.partial_sigs.len()).sum()
}

#[tokio::test]
async fn split_hw_sign_singlesig_shapes_sign_and_finalize() {
    for script in [
        SinglesigScript::Pkh,
        SinglesigScript::ShWpkh,
        SinglesigScript::Wpkh,
    ] {
        let descriptor = singlesig(script, 1);
        for kind in [DeviceKind::Jade, DeviceKind::Ledger, DeviceKind::BitBox02] {
            let device = fake(kind, 1);
            let policy = DevicePolicy::new(&descriptor).unwrap();
            assert_eq!(policy.shape(), PolicyShape::Singlesig(script));
            assert_eq!(policy.name(), "", "default policy is unnamed");
            let mut session = SigningSession::open(device.arc(), fp(1), policy)
                .await
                .unwrap();
            assert_eq!(
                session.register().await.unwrap(),
                RegistrationOutcome::NotNeeded
            );
            assert_eq!(device.shared.register_calls.load(Ordering::SeqCst), 0);
            let binder = binder(&device);
            let psbt = spend(&descriptor);
            let mut signed = session.sign(&psbt, &binder).await.unwrap().into_psbt();
            assert_eq!(sig_count(&signed), 1, "{script:?} {kind}");
            assert_eq!(signed.unsigned_tx, psbt.unsigned_tx);
            // Ledger signs through a handle bound to the default policy
            // with no token; the others sign on the listed handle.
            let seen = binder.seen.lock().unwrap().clone();
            if kind == DeviceKind::Ledger {
                assert_eq!(seen, vec![(String::new(), None)]);
            } else {
                assert!(seen.is_empty());
            }
            // The signatures are real: miniscript verifies them.
            signed.finalize_mut(&secp()).unwrap();
        }
    }
}

#[tokio::test]
async fn split_hw_sign_multisig_registers_then_signs_2_of_3() {
    for sorted in [true, false] {
        let descriptor = multisig(sorted);
        let psbt = spend(&descriptor);
        let policy = DevicePolicy::new(&descriptor).unwrap();
        assert_eq!(policy.shape(), PolicyShape::Multisig { k: 2, n: 3, sorted });
        assert!(policy.name().starts_with("Split") && policy.name().len() == 13);

        // Key 1 on a Jade: the device stores the registration.
        let jade = fake(DeviceKind::Jade, 1);
        let mut session = SigningSession::open(jade.arc(), fp(1), policy.clone())
            .await
            .unwrap();
        assert_eq!(session.handle(), SigningHandle::Listed);
        assert_eq!(
            session.register().await.unwrap(),
            RegistrationOutcome::Registered
        );
        let by_jade = session.sign(&psbt, &NoBinder).await.unwrap().into_psbt();
        assert_eq!(sig_count(&by_jade), 1);
        // A second session finds it already on the device.
        let mut again = SigningSession::open(jade.arc(), fp(1), policy.clone())
            .await
            .unwrap();
        assert_eq!(
            again.register().await.unwrap(),
            RegistrationOutcome::AlreadyRegistered
        );
        assert_eq!(jade.shared.register_calls.load(Ordering::SeqCst), 1);

        // Key 2 on a Ledger: registration returns a token that only this
        // session holds and that the bound handle signs with.
        let ledger = fake(DeviceKind::Ledger, 2);
        let mut session = SigningSession::open(ledger.arc(), fp(2), policy.clone())
            .await
            .unwrap();
        assert_eq!(session.handle(), SigningHandle::Bound);
        assert_eq!(
            session.register().await.unwrap(),
            RegistrationOutcome::Registered
        );
        let ledger_binder = binder(&ledger);
        let by_ledger = session
            .sign(&by_jade, &ledger_binder)
            .await
            .unwrap()
            .into_psbt();
        assert_eq!(
            ledger_binder.seen.lock().unwrap().clone(),
            vec![(
                policy.name().to_string(),
                Some(token(policy.name(), &descriptor.to_string()))
            )]
        );
        assert_eq!(sig_count(&by_ledger), 2);

        let mut combined = by_ledger;
        combined.finalize_mut(&secp()).unwrap();
    }
}

#[tokio::test]
async fn split_hw_sign_refuses_unregistered_policy() {
    let descriptor = multisig(true);
    let psbt = spend(&descriptor);
    let policy = DevicePolicy::new(&descriptor).unwrap();

    // Signing before registering in this session is refused without
    // touching the device.
    let jade = fake(DeviceKind::Jade, 1);
    let session = SigningSession::open(jade.arc(), fp(1), policy.clone())
        .await
        .unwrap();
    assert_eq!(
        session.sign(&psbt, &NoBinder).await.unwrap_err(),
        SignError::PolicyNotRegistered
    );
    assert_eq!(jade.shared.sign_calls.load(Ordering::SeqCst), 0);

    // The registration disappears from the device (wiped or replaced)
    // between registering and signing.
    let mut session = SigningSession::open(jade.arc(), fp(1), policy.clone())
        .await
        .unwrap();
    session.register().await.unwrap();
    jade.shared.stored.lock().unwrap().clear();
    assert_eq!(
        session.sign(&psbt, &NoBinder).await.unwrap_err(),
        SignError::PolicyNotRegistered
    );
    assert_eq!(jade.shared.sign_calls.load(Ordering::SeqCst), 0);

    // A Ledger handle bound with the wrong token does not report the
    // policy, so nothing is signed.
    let ledger = fake(DeviceKind::Ledger, 2);
    let mut session = SigningSession::open(ledger.arc(), fp(2), policy.clone())
        .await
        .unwrap();
    session.register().await.unwrap();
    struct WrongToken(Fake);
    #[async_trait::async_trait]
    impl PolicyBinder for WrongToken {
        async fn bind(
            &self,
            r: BindRequest<'_>,
        ) -> Result<Arc<dyn HWI + Send + Sync>, async_hwi::Error> {
            let mut device = self.0.clone();
            device.bound = Some((r.name.into(), r.policy.into(), Some([0; 32])));
            Ok(Arc::new(device))
        }
    }
    assert_eq!(
        session
            .sign(&psbt, &WrongToken(ledger.clone()))
            .await
            .unwrap_err(),
        SignError::PolicyNotRegistered
    );
    // No binder at all: refused before the device is asked.
    assert_eq!(
        session.sign(&psbt, &NoBinder).await.unwrap_err(),
        SignError::NeedsPolicyBinding(DeviceKind::Ledger)
    );
    assert_eq!(ledger.shared.sign_calls.load(Ordering::SeqCst), 0);

    // The user declines the registration.
    let mut refusing = fake(DeviceKind::Jade, 1);
    refusing.refuse_registration = true;
    let mut session = SigningSession::open(refusing.arc(), fp(1), policy.clone())
        .await
        .unwrap();
    assert_eq!(
        session.register().await.unwrap_err(),
        SignError::RegistrationRefused
    );
    assert_eq!(
        session.sign(&psbt, &NoBinder).await.unwrap_err(),
        SignError::PolicyNotRegistered
    );

    // Specter cannot report registration: multisig is refused at open,
    // singlesig still works.
    let specter = fake(DeviceKind::Specter, 1);
    assert_eq!(
        SigningSession::open(specter.arc(), fp(1), policy.clone())
            .await
            .unwrap_err(),
        SignError::UnsupportedDevice(DeviceKind::Specter)
    );
    let single = singlesig(SinglesigScript::Wpkh, 1);
    let session = SigningSession::open(specter.arc(), fp(1), DevicePolicy::new(&single).unwrap())
        .await
        .unwrap();
    session.sign(&spend(&single), &NoBinder).await.unwrap();
}

#[test]
fn split_hw_sign_handle_table() {
    let single = PolicyShape::Singlesig(SinglesigScript::Wpkh);
    let multi = PolicyShape::Multisig {
        k: 2,
        n: 3,
        sorted: true,
    };
    use SigningHandle::*;
    for (kind, s, m) in [
        (DeviceKind::Jade, Ok(Listed), Ok(Listed)),
        (DeviceKind::Coldcard, Ok(Listed), Ok(Bound)),
        (DeviceKind::BitBox02, Ok(Listed), Ok(Bound)),
        (DeviceKind::Ledger, Ok(Bound), Ok(Bound)),
        (DeviceKind::LedgerSimulator, Ok(Bound), Ok(Bound)),
        (
            DeviceKind::Specter,
            Ok(Listed),
            Err(SignError::UnsupportedDevice(DeviceKind::Specter)),
        ),
    ] {
        assert_eq!(signing_handle(kind, single), s, "{kind}");
        assert_eq!(signing_handle(kind, multi), m, "{kind}");
    }
}

#[tokio::test]
async fn split_hw_sign_refuses_non_all_device_output() {
    let descriptor = singlesig(SinglesigScript::Wpkh, 1);
    let psbt = spend(&descriptor);
    let policy = DevicePolicy::new(&descriptor).unwrap();
    for sighash in [
        EcdsaSighashType::None,
        EcdsaSighashType::Single,
        EcdsaSighashType::AllPlusAnyoneCanPay,
        EcdsaSighashType::NonePlusAnyoneCanPay,
        EcdsaSighashType::SinglePlusAnyoneCanPay,
    ] {
        for hide in [false, true] {
            let mut device = fake(DeviceKind::Jade, 1);
            device.sighash = sighash;
            device.hide_sighash_field = hide;
            let session = SigningSession::open(device.arc(), fp(1), policy.clone())
                .await
                .unwrap();
            assert_eq!(
                session.sign(&psbt, &NoBinder).await.unwrap_err(),
                SignError::NonAllSighash { input: 0 },
                "{sighash} hidden={hide}"
            );
            assert_eq!(device.shared.sign_calls.load(Ordering::SeqCst), 1);
        }
    }

    // A PSBT that itself asks for a non-ALL sighash is refused before the
    // device sees it; an explicit ALL (0x01) is accepted like the default.
    let device = fake(DeviceKind::Jade, 1);
    let session = SigningSession::open(device.arc(), fp(1), policy.clone())
        .await
        .unwrap();
    let mut asks_single = psbt.clone();
    asks_single.inputs[0].sighash_type = Some(PsbtSighashType::from(EcdsaSighashType::Single));
    assert_eq!(
        session.sign(&asks_single, &NoBinder).await.unwrap_err(),
        SignError::NonAllSighash { input: 0 }
    );
    assert_eq!(device.shared.sign_calls.load(Ordering::SeqCst), 0);
    let mut explicit_all = psbt.clone();
    explicit_all.inputs[0].sighash_type = Some(PsbtSighashType::from(EcdsaSighashType::All));
    let mut signed = session
        .sign(&explicit_all, &NoBinder)
        .await
        .unwrap()
        .into_psbt();
    signed.finalize_mut(&secp()).unwrap();
}

#[test]
fn split_hw_sign_screens_untrusted_device_output() {
    let descriptor = multisig(true);
    let original = spend(&descriptor);
    let mine = fp(1);
    let sign_as = |seed: u8, base: &Psbt| {
        let mut psbt = base.clone();
        let _ = psbt.sign(&master(seed), &secp());
        psbt
    };

    let good = sign_as(1, &original);
    let accepted = accept_device_signatures(&original, &good, mine).unwrap();
    assert_eq!(sig_count(&accepted), 1);

    // Extra fields the device may set are dropped, not trusted.
    let mut noisy = good.clone();
    noisy.inputs[0].final_script_witness = Some(Default::default());
    noisy.inputs[0].redeem_script = Some(ScriptBuf::new());
    let accepted = accept_device_signatures(&original, &noisy, mine).unwrap();
    assert!(accepted.inputs[0].final_script_witness.is_none());
    assert_eq!(
        accepted.inputs[0].redeem_script,
        original.inputs[0].redeem_script
    );

    // A changed transaction.
    let mut changed = good.clone();
    changed.unsigned_tx.output[0].value = Amount::from_sat(1);
    assert_eq!(
        accept_device_signatures(&original, &changed, mine).unwrap_err(),
        SignError::TransactionChanged
    );
    // A signature for a key the device does not own in this PSBT.
    let other = sign_as(2, &original);
    assert_eq!(
        accept_device_signatures(&original, &other, mine).unwrap_err(),
        SignError::UnexpectedSignature { input: 0 }
    );
    // A replaced existing signature.
    let (key, sig) = good.inputs[0].partial_sigs.iter().next().unwrap();
    let mut prior = original.clone();
    prior.inputs[0].partial_sigs.insert(*key, *sig);
    let mut replaced = good.clone();
    let mut forged = *sig;
    forged.signature = other.inputs[0]
        .partial_sigs
        .values()
        .next()
        .unwrap()
        .signature;
    replaced.inputs[0].partial_sigs.insert(*key, forged);
    assert_eq!(
        accept_device_signatures(&prior, &replaced, mine).unwrap_err(),
        SignError::UnexpectedSignature { input: 0 }
    );
    // A Taproot signature on a non-Taproot shape.
    let mut taproot = good.clone();
    taproot.inputs[0].tap_key_sig =
        Some(coincube_core::miniscript::bitcoin::taproot::Signature::from_slice(&[1; 64]).unwrap());
    assert_eq!(
        accept_device_signatures(&original, &taproot, mine).unwrap_err(),
        SignError::UnexpectedSignature { input: 0 }
    );
    // Nothing new.
    assert_eq!(
        accept_device_signatures(&original, &original, mine).unwrap_err(),
        SignError::DeviceDidNotSign
    );
    assert_eq!(
        accept_device_signatures(&prior, &good, mine).unwrap_err(),
        SignError::DeviceDidNotSign
    );
}

#[tokio::test]
async fn split_hw_sign_policy_validation() {
    let k1 = key_expr(1, "m/48'/0'/0'/2'");
    let k2 = key_expr(2, "m/48'/0'/0'/2'");
    let refuse = |s: &str| DevicePolicy::new(&desc(s)).unwrap_err();

    assert_eq!(
        refuse(&format!("sh(multi(1,{k1},{k2}))")),
        SignError::UnsupportedShape
    );
    assert_eq!(
        refuse(&format!("sh(wsh(multi(1,{k1},{k2})))")),
        SignError::UnsupportedShape
    );
    assert_eq!(
        refuse(&format!("wsh(and_v(v:pk({k1}),pk({k2})))")),
        SignError::UnsupportedShape
    );
    assert_eq!(
        refuse(&format!("wsh(pk({k1}))")),
        SignError::UnsupportedShape
    );
    assert_eq!(
        refuse(&format!("tr({})", key_expr(1, "m/86'/0'/0'"))),
        SignError::UnsupportedShape
    );
    // Key forms: no origin, a single branch, a hardened wildcard.
    let bare = k1.split(']').nth(1).unwrap().to_string();
    assert_eq!(
        refuse(&format!("wsh(multi(1,{bare},{k2}))")),
        SignError::KeyForm
    );
    assert_eq!(
        refuse(&format!(
            "wsh(multi(1,{},{k2}))",
            k1.replace("/<0;1>/*", "/0/*")
        )),
        SignError::KeyForm
    );
    assert_eq!(
        refuse(&format!(
            "wsh(multi(1,{},{k2}))",
            k1.replace("/<0;1>/*", "/<0;1>/*'")
        )),
        SignError::KeyForm
    );
    assert_eq!(
        refuse(&format!("wsh(multi(1,{k1},{k1}))")),
        SignError::DuplicateKey
    );
    // Singlesig keys must be on their script's standard account path.
    assert_eq!(
        refuse(&format!("wpkh({})", key_expr(1, "m/44'/0'/0'"))),
        SignError::NonStandardSinglesigPath
    );
    assert_eq!(
        refuse(&format!("pkh({})", key_expr(1, "m/44'/0'/0'/0'"))),
        SignError::NonStandardSinglesigPath
    );
    assert_eq!(
        refuse(&format!("sh(wpkh({}))", key_expr(1, "m/49'/1'/0'"))),
        SignError::NonStandardSinglesigPath
    );

    // The device must hold one of the keys, and must be the listed one.
    let policy = DevicePolicy::new(&multisig(true)).unwrap();
    assert_eq!(
        SigningSession::open(fake(DeviceKind::Jade, 9).arc(), fp(9), policy.clone())
            .await
            .unwrap_err(),
        SignError::DeviceNotInPolicy
    );
    assert_eq!(
        SigningSession::open(fake(DeviceKind::Jade, 3).arc(), fp(1), policy)
            .await
            .unwrap_err(),
        SignError::Fingerprint
    );
}

fn temp_root() -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("coincube-split-hw-sign-{unique}"));
    fs::create_dir(&root).unwrap();
    root
}

/// P6: the token stays in memory, nothing reaches the datadir, and a new
/// session registers again.
#[tokio::test]
async fn split_hw_sign_session_persists_nothing_and_reregisters() {
    let root = temp_root();
    let mut devices =
        HardwareWallets::new(CoincubeDirectory::new(root.clone()), Network::Bitcoin).ephemeral();
    let ledger = fake(DeviceKind::Ledger, 2);
    devices.list.push(HardwareWallet::Supported {
        id: "ledger-1".into(),
        device: ledger.arc(),
        kind: DeviceKind::Ledger,
        fingerprint: fp(2),
        version: None,
        registered: None,
        alias: None,
    });
    let descriptor = multisig(false);
    let psbt = spend(&descriptor);
    let policy = DevicePolicy::new(&descriptor).unwrap();
    let hmac_hex = hex::encode(token(policy.name(), &descriptor.to_string()));

    for session_number in 1..=2 {
        let mut session = SigningSession::from_listed(&devices.list[0], policy.clone())
            .await
            .unwrap();
        // A fresh session holds no registration, whatever an earlier one did.
        assert_eq!(
            session.sign(&psbt, &binder(&ledger)).await.unwrap_err(),
            SignError::PolicyNotRegistered
        );
        assert_eq!(
            session.register().await.unwrap(),
            RegistrationOutcome::Registered
        );
        assert_eq!(
            ledger.shared.register_calls.load(Ordering::SeqCst),
            session_number
        );
        let debug = format!("{session:?}");
        assert!(
            !debug.contains(&hmac_hex) && !debug.contains("xpub") && !debug.contains("wsh"),
            "{}",
            debug
        );
        session.sign(&psbt, &binder(&ledger)).await.unwrap();
        drop(session);
    }
    assert!(!devices.persists_pairing());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    fs::remove_dir(root).unwrap();

    // A locked or unsupported listing cannot open a session.
    let locked = HardwareWallet::Unsupported {
        id: "x".into(),
        kind: DeviceKind::Ledger,
        version: None,
        reason: crate::hw::UnsupportedReason::AppIsNotOpen,
    };
    assert_eq!(
        SigningSession::from_listed(&locked, policy)
            .await
            .unwrap_err(),
        SignError::DeviceUnavailable
    );
}
