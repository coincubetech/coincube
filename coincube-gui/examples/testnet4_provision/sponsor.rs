// Separate sponsor operation: all existing run/profile material is read-only.
use super::*;
use bitcoin::hashes::{sha256, Hash};

const GENESIS: &str = "00000000da84f2bafbbc53dee25a72ae507ff4914b867c565be350b0da8bf043";
const ENDPOINTS: [&str; 2] = [
    "https://mempool.space/testnet4/api",
    "https://blockstream.info/testnet4/api",
];

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct BoundRun {
    directory: PathBuf,
    run_id: String,
    state_sha256: String,
    descriptor_sha256: String,
    checkpoint_sha256: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u32,
    network: String,
    authorization_ref: String,
    source: BoundRun,
    target: BoundRun,
    previous_txid: String,
    previous_vout: u32,
    previous_value_sats: u64,
    previous_raw_sha256: String,
    source_script: String,
    target_address: String,
    target_script: String,
    amount_sats: u64,
    fee_sats: u64,
    change_sats: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prior_sponsor: Option<PriorSponsor>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PriorSponsor {
    intent_json: String,
    receipt_json: String,
    target: BoundRun,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    endpoint: String,
    genesis: String,
    observed_at: u64,
    txid: String,
    vout: u32,
    raw_tx: String,
    spent: bool,
}

fn hashed_read(path: &Path, expected: &str) -> Result<String> {
    let value = private_read(path)?;
    check(
        sha256::Hash::hash(value.as_bytes()).to_string() == expected,
        "sponsor_input_changed",
    )?;
    Ok(value)
}
fn bound_run(binding: &BoundRun) -> Result<(State, CoincubeDescriptor, Value)> {
    let state: State = serde_json::from_str(&hashed_read(
        &binding.directory.join("provision.json"),
        &binding.state_sha256,
    )?)
    .map_err(|_| "invalid_run_checkpoint")?;
    let descriptor = CoincubeDescriptor::from_str(
        hashed_read(
            &binding.directory.join("descriptor.txt"),
            &binding.descriptor_sha256,
        )?
        .trim(),
    )
    .map_err(|_| "invalid_saved_descriptor")?;
    let checkpoint: Value = serde_json::from_str(&hashed_read(
        &binding.directory.join("checkpoint.json"),
        &binding.checkpoint_sha256,
    )?)
    .map_err(|_| "invalid_run_checkpoint")?;
    check(
        state.version == 1
            && state.run_id == binding.run_id
            && state.cube_uuid == binding.run_id
            && state.vault_id.is_some()
            && state.owner_user_id.is_some_and(|id| id > 0)
            && descriptor.all_xpubs_net_is(NETWORK)
            && state.descriptor_id.as_deref()
                == Some(&descriptor_id_fingerprint(&descriptor).to_string())
            && matches!(descriptor.policy().primary_path(), PathInfo::Multi(2, keys) if keys.len() == 3),
        "sponsor_run_binding",
    )?;
    checkpoint_identity(&state, &checkpoint)?;
    Ok((state, descriptor, checkpoint))
}
fn checkpoint_identity<'a>(state: &State, checkpoint: &'a Value) -> Result<&'a str> {
    let artifacts = &checkpoint["artifacts"];
    let account = artifacts["account_identity"]
        .as_str()
        .ok_or("sponsor_account_binding")?;
    check(
        account.len() == 64
            && account
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && state.vault_id.is_some_and(|id| id > 0)
            && artifacts["vault_id"].as_str() == state.vault_id.map(|id| id.to_string()).as_deref()
            && artifacts["descriptor_id"].as_str() == state.descriptor_id.as_deref(),
        "sponsor_account_binding",
    )?;
    Ok(account)
}

fn check_replay(receipt: &Value, expected: &Transaction, intent_hash: &str) -> Result<()> {
    check(
        receipt["intent_sha256"] == intent_hash
            && receipt["raw_tx"].as_str()
                == Some(hex::encode(consensus::serialize(expected)).as_str()),
        "sponsor_receipt_changed",
    )
}

fn evidence_transaction(intent: &Intent, evidence: &[Evidence], now: u64) -> Result<Transaction> {
    check(evidence.len() == 2, "sponsor_evidence_required")?;
    for (entry, endpoint) in evidence.iter().zip(ENDPOINTS) {
        check(
            entry.endpoint == endpoint
                && entry.genesis == GENESIS
                && !entry.spent
                && entry.txid == intent.previous_txid
                && entry.vout == intent.previous_vout
                && entry.observed_at <= now.saturating_add(5)
                && now.saturating_sub(entry.observed_at) <= 120,
            "sponsor_evidence_invalid",
        )?;
    }
    check(
        evidence[0].raw_tx == evidence[1].raw_tx
            && sha256::Hash::hash(evidence[0].raw_tx.as_bytes()).to_string()
                == intent.previous_raw_sha256,
        "sponsor_evidence_disagreement",
    )?;
    let previous: Transaction = consensus::deserialize(
        &hex::decode(&evidence[0].raw_tx).map_err(|_| "invalid_funding_transaction")?,
    )
    .map_err(|_| "invalid_funding_transaction")?;
    check(
        previous.compute_txid().to_string() == intent.previous_txid,
        "sponsor_previous_txid",
    )?;
    Ok(previous)
}
fn check_intent(intent: &Intent) -> Result<()> {
    check(
        ((intent.version == 1 && intent.prior_sponsor.is_none())
            || (intent.version == 2 && intent.prior_sponsor.is_some()))
            && intent.network == "testnet4"
            && intent.authorization_ref == "coincube#1137:testnet4-sponsor"
            && intent.source.run_id != intent.target.run_id
            && intent.source.directory != intent.target.directory,
        "sponsor_intent_invalid",
    )?;
    check(
        (100_000..=500_000).contains(&intent.amount_sats)
            && (100..=1000).contains(&intent.fee_sats)
            && (1000..=10_000_000).contains(&intent.previous_value_sats)
            && intent.change_sats >= 1000
            && intent
                .previous_value_sats
                .checked_sub(intent.amount_sats)
                .and_then(|v| v.checked_sub(intent.fee_sats))
                == Some(intent.change_sats)
            && intent.previous_vout == 1,
        "sponsor_amount_bounds",
    )
}

fn unsigned_psbt(
    intent: &Intent,
    source: &State,
    source_desc: &CoincubeDescriptor,
    target_desc: &CoincubeDescriptor,
    previous: Transaction,
) -> Result<Psbt> {
    check_intent(intent)?;
    let secp = secp256k1::Secp256k1::new();
    let index = ChildNumber::from_normal_idx(0).map_err(|_| "invalid_derivation")?;
    let change = source_desc.change_descriptor().derive(index, &secp);
    let target = target_desc.receive_descriptor().derive(index, &secp);
    let saved = source.spend.as_ref().ok_or("sponsor_source_incomplete")?;
    let mut original = previous.clone();
    for input in &mut original.input {
        input.witness.clear();
    }
    check(
        saved.change_address == change.address(NETWORK).to_string()
            && previous.compute_txid().to_string() == intent.previous_txid
            && (intent.version == 2
                || (saved.txid == intent.previous_txid
                    && saved.change_sats == intent.previous_value_sats
                    && saved.unsigned_hex == hex::encode(consensus::serialize(&original)))),
        "sponsor_source_spend",
    )?;
    let prevout = previous
        .output
        .get(1)
        .ok_or("funding_output_missing")?
        .clone();
    check(
        previous.output.len() == 2
            && prevout.value.to_sat() == intent.previous_value_sats
            && prevout.script_pubkey == change.script_pubkey()
            && intent.source_script == hex::encode(change.script_pubkey().as_bytes())
            && intent.target_script == hex::encode(target.script_pubkey().as_bytes())
            && intent.target_address == target.address(NETWORK).to_string()
            && target.script_pubkey() != change.script_pubkey(),
        "sponsor_script_binding",
    )?;
    let tx = Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(previous.compute_txid(), 1),
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::default(),
        }],
        output: vec![
            TxOut {
                value: Amount::from_sat(intent.amount_sats),
                script_pubkey: target.script_pubkey(),
            },
            TxOut {
                value: Amount::from_sat(intent.change_sats),
                script_pubkey: change.script_pubkey(),
            },
        ],
    };
    let mut psbt = Psbt::from_unsigned_tx(tx).map_err(|_| "invalid_spend")?;
    change.update_psbt_in(&mut psbt.inputs[0]);
    psbt.inputs[0].non_witness_utxo = Some(previous);
    psbt.inputs[0].witness_utxo = Some(prevout);
    psbt.inputs[0].sighash_type = Some(bitcoin::sighash::EcdsaSighashType::All.into());
    Ok(psbt)
}

// Version 2 supports exactly one recorded sponsor hop. An old receipt is not
// authority: recreate its Master + Border witness and compare the exact public
// transaction currently observed by both explorers before signing anything new.
fn recorded_prior_sponsor(intent: &Intent, observed: &Transaction) -> Result<Intent> {
    check_intent(intent)?;
    let recorded = intent.prior_sponsor.as_ref().ok_or("sponsor_parent_required")?;
    let prior: Intent = serde_json::from_str(&recorded.intent_json)
        .map_err(|_| "sponsor_parent_intent")?;
    check(
        prior.version == 1 && prior.prior_sponsor.is_none(),
        "sponsor_parent_depth",
    )?;
    check_intent(&prior)?;
    let receipt: Value = serde_json::from_str(&recorded.receipt_json)
        .map_err(|_| "sponsor_parent_receipt")?;
    let prior_hash = sha256::Hash::hash(recorded.intent_json.as_bytes()).to_string();
    check_replay(&receipt, observed, &prior_hash)?;
    check(
        prior.source == intent.source
            && prior.source_script == intent.source_script
            && prior.change_sats == intent.previous_value_sats
            && recorded.target.directory == prior.target.directory
            && recorded.target.run_id == prior.target.run_id
            && recorded.target.descriptor_sha256 == prior.target.descriptor_sha256
            && receipt["ok"] == true
            && receipt["action"] == "sponsor"
            && receipt["network"] == "testnet4"
            && receipt["txid"] == observed.compute_txid().to_string()
            && receipt["amount_sats"] == prior.amount_sats
            && receipt["fee_sats"] == prior.fee_sats
            && receipt["change_sats"] == prior.change_sats,
        "sponsor_parent_binding",
    )?;
    Ok(prior)
}

fn verify_prior_sponsor(
    intent: &Intent,
    source: &State,
    source_desc: &CoincubeDescriptor,
    prior_target_desc: &CoincubeDescriptor,
    observed: &Transaction,
    secrets: &Secrets,
) -> Result<()> {
    check_intent(intent)?;
    let prior = recorded_prior_sponsor(intent, observed)?;
    let saved = source.spend.as_ref().ok_or("sponsor_source_incomplete")?;
    let original: Transaction = consensus::deserialize(
        &hex::decode(&saved.unsigned_hex).map_err(|_| "sponsor_source_spend")?,
    )
    .map_err(|_| "sponsor_source_spend")?;
    let prior_psbt = unsigned_psbt(&prior, source, source_desc, prior_target_desc, original)?;
    let expected = sign(prior_psbt, secrets)?;
    check(
        expected == *observed
            && expected.compute_txid().to_string() == intent.previous_txid,
        "sponsor_parent_witness_unsupported",
    )
}

fn sign(mut psbt: Psbt, secrets: &Secrets) -> Result<Transaction> {
    let secp = secp256k1::Secp256k1::new();
    let signer = master(secrets)?;
    let enrollment = border(secrets)?;
    let mut path =
        DerivationPath::from_str("m/48'/1'/0'/2'/1/0").map_err(|_| "invalid_derivation")?;
    let master_pub = signer.xpub_at(&path, &secp).public_key;
    let master_path = path.clone();
    path = enrollment.derivation_path.extend([
        ChildNumber::Normal { index: 1 },
        ChildNumber::Normal { index: 0 },
    ]);
    let border_pub = enrollment
        .xpub
        .derive_pub(
            &secp,
            &[
                ChildNumber::Normal { index: 1 },
                ChildNumber::Normal { index: 0 },
            ],
        )
        .map_err(|_| "invalid_derivation")?
        .public_key;
    let origins = &mut psbt.inputs[0].bip32_derivation;
    check(
        origins
            .get(&master_pub)
            .is_some_and(|(fp, p)| *fp == signer.fingerprint(&secp) && *p == master_path)
            && origins.get(&border_pub) == Some(&(enrollment.fingerprint, path)),
        "sponsor_primary_origins",
    )?;
    // No phone or timelocked Border origins reach either signer.
    origins.retain(|key, _| *key == master_pub || *key == border_pub);
    psbt = signer
        .sign_psbt(psbt, &secp)
        .map_err(|_| "local_signing_failed")?;
    let grid = GridRecoveryPhrase::from_phrase(&secrets.grid_phrase)
        .map_err(|_| "invalid_border_backup")?
        .generate_grid();
    let mut pattern = OrderedPattern::new();
    for &(row, col) in &secrets.pattern {
        pattern
            .add(CellRef::new(row, col))
            .map_err(|_| "invalid_border_backup")?;
    }
    let (mnemonic, checksum) =
        build_mnemonic(&grid, &pattern).map_err(|_| "invalid_border_backup")?;
    check(
        checksum == secrets.checksum_word,
        "border_checksum_mismatch",
    )?;
    let (_, mut signed) = coincube_core::border_wallet::sign_psbt_with_border_wallet(
        mnemonic,
        enrollment.fingerprint,
        NETWORK,
        psbt,
    )
    .map_err(|_| "border_signing_failed")?;
    check(
        signed.inputs[0].partial_sigs.len() == 2
            && signed.inputs[0].partial_sigs.iter().all(|(key, sig)| {
                (key.inner == master_pub || key.inner == border_pub)
                    && sig.sighash_type == bitcoin::sighash::EcdsaSighashType::All
            }),
        "sponsor_primary_signatures",
    )?;
    let unified = coincube_core::psbt_unified::UnifiedPsbt::from_psbt(signed.clone())
        .map_err(|_| "invalid_signed_psbt")?;
    coincube_core::unified_finalize::verify_all_signatures(&unified, &secp)
        .map_err(|_| "invalid_signature")?;
    signed
        .finalize_mut(&secp)
        .map_err(|_| "finalization_failed")?;
    signed
        .extract(&secp)
        .map_err(|_| "signature_verification_failed")
}

pub(super) fn execute(input: &Input, store: &dyn secret_store::Backend) -> Result<Value> {
    let intent_text = private_read(
        input
            .sponsor_intent_file
            .as_ref()
            .ok_or("sponsor_intent_required")?,
    )?;
    let intent_hash = sha256::Hash::hash(intent_text.as_bytes()).to_string();
    check(
        input.sponsor_intent_sha256.as_deref() == Some(&intent_hash),
        "sponsor_authorization_mismatch",
    )?;
    let intent: Intent =
        serde_json::from_str(&intent_text).map_err(|_| "sponsor_intent_invalid")?;
    check_intent(&intent)?;
    let evidence: Vec<Evidence> = serde_json::from_str(&private_read(
        input
            .sponsor_evidence_file
            .as_ref()
            .ok_or("sponsor_evidence_required")?,
    )?)
    .map_err(|_| "sponsor_evidence_invalid")?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock_error")?
        .as_secs();
    let previous = evidence_transaction(&intent, &evidence, now)?;
    let (source, source_desc, source_checkpoint) = bound_run(&intent.source)?;
    let (target, target_desc, target_checkpoint) = bound_run(&intent.target)?;
    check(
        source.profile_dir == input.profile_dir
            && source.run_id == input.run_id
            && source.owner_user_id == target.owner_user_id
            && source.vault_id != target.vault_id
            && checkpoint_identity(&source, &source_checkpoint)?
                == checkpoint_identity(&target, &target_checkpoint)?
            && source_checkpoint["artifacts"]["public_acceptance"]["txid"]
                == source.spend.as_ref().ok_or("sponsor_source_incomplete")?.txid
            && source_checkpoint["artifacts"]["broadcast_txid"]
                == source.spend.as_ref().ok_or("sponsor_source_incomplete")?.txid
            && target_checkpoint["artifacts"]["receive_address"] == intent.target_address,
        "sponsor_run_binding",
    )?;
    let prior_target_desc = if let Some(recorded) = &intent.prior_sponsor {
        // The historical target may have progressed since it was funded.
        // Freeze its current full snapshot separately; its descriptor and
        // identity must still equal the historical intent, never rewrite it.
        let _ = recorded_prior_sponsor(&intent, &previous)?;
        let (prior_target, descriptor, checkpoint) = bound_run(&recorded.target)?;
        check(
            source.owner_user_id == prior_target.owner_user_id
                && source.vault_id != prior_target.vault_id
                && checkpoint_identity(&source, &source_checkpoint)?
                    == checkpoint_identity(&prior_target, &checkpoint)?,
            "sponsor_parent_account",
        )?;
        Some(descriptor)
    } else {
        None
    };
    // Read only on both initial signing and replay. Deterministic reconstruction
    // binds an existing receipt to Master + Border, not just any valid primary pair.
    let locator = secret_store::locator(&source.profile_dir, &source.run_id, "provision_backup")?;
    let snapshot = secret_store::get(store, &locator)?.ok_or("backup_keychain_item_missing")?;
    let secrets: Secrets =
        serde_json::from_value(snapshot.value).map_err(|_| "invalid_run_backup")?;
    check(
        secrets.run_id == source.run_id && secrets.profile_dir == source.profile_dir,
        "run_identity_mismatch",
    )?;
    check(
        descriptor(
            &secrets,
            source
                .phone_descriptor_key
                .as_deref()
                .ok_or("vault_not_provisioned")?,
        )? == source_desc,
        "saved_descriptor_mismatch",
    )?;
    if let Some(prior_target_desc) = prior_target_desc {
        verify_prior_sponsor(
            &intent,
            &source,
            &source_desc,
            &prior_target_desc,
            &previous,
            &secrets,
        )?;
    }
    let psbt = unsigned_psbt(&intent, &source, &source_desc, &target_desc, previous)?;
    let unsigned = psbt.unsigned_tx.clone();
    let tx = sign(psbt, &secrets)?;
    if let Some(path) = &input.sponsor_receipt_file {
        let receipt: Value =
            serde_json::from_str(&private_read(path)?).map_err(|_| "sponsor_receipt_invalid")?;
        check_replay(&receipt, &tx, &intent_hash)?;
    }
    check(
        tx.compute_txid() == unsigned.compute_txid(),
        "transaction_intent_mismatch",
    )?;
    Ok(
        json!({"ok":true,"action":"sponsor","network":"testnet4","intent_sha256":intent_hash,
        "txid":tx.compute_txid().to_string(),"raw_tx":hex::encode(consensus::serialize(&tx)),
        "amount_sats":intent.amount_sats,"fee_sats":intent.fee_sats,"change_sats":intent.change_sats}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (
        Secrets,
        State,
        CoincubeDescriptor,
        CoincubeDescriptor,
        Transaction,
        Intent,
        MasterSigner,
    ) {
        let request: Input = serde_json::from_value(json!({"action":"cube", "network":"testnet4",
            "profile_dir":"/synthetic/source/profile", "run_dir":"/synthetic/source",
            "run_id":"11111111-1111-4111-8111-111111111111"}))
        .unwrap();
        let secrets = generate_run_secrets(&request).unwrap();
        let secp = secp256k1::Secp256k1::new();
        let phone = MasterSigner::generate(NETWORK).unwrap();
        let path = DerivationPath::from_str("m/48'/1'/0'/2'").unwrap();
        let phone_key = format!(
            "[{}{}]{}/<0;1>/*",
            phone.fingerprint(&secp),
            origin_suffix(&path),
            phone.xpub_at(&path, &secp)
        );
        let source_desc = descriptor(&secrets, &phone_key).unwrap();
        let target_secrets = generate_run_secrets(&request).unwrap();
        let target_desc = descriptor(&target_secrets, &phone_key).unwrap();
        let index = ChildNumber::Normal { index: 0 };
        let change = source_desc.change_descriptor().derive(index, &secp);
        let target = target_desc.receive_descriptor().derive(index, &secp);
        let previous = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::default(),
            }],
            output: vec![
                TxOut {
                    value: Amount::from_sat(10_000),
                    script_pubkey: target.script_pubkey(),
                },
                TxOut {
                    value: Amount::from_sat(989_000),
                    script_pubkey: change.script_pubkey(),
                },
            ],
        };
        let mut state: State = serde_json::from_value(json!({"version":1,"run_id":request.run_id,
            "profile_dir":request.profile_dir,"cube_uuid":request.run_id,"cube_name":"synthetic",
            "cube_id":1,"owner_user_id":1,"registration_started":true,"wallet_id":null,
            "target_key_id":"3","target_fingerprint":phone.fingerprint(&secp).to_string(),
            "phone_descriptor_key":phone_key,"vault_id":1,"descriptor_id":descriptor_id_fingerprint(&source_desc).to_string(),"spend":null})).unwrap();
        state.spend = Some(Spend {
            funding_txid: "00".repeat(32),
            funding_vout: 0,
            funding_sats: 1_000_000,
            amount_sats: 10_000,
            fee_sats: 1000,
            recipient: target.address(NETWORK).to_string(),
            change_address: change.address(NETWORK).to_string(),
            change_sats: 989_000,
            unsigned_hex: hex::encode(consensus::serialize(&previous)),
            txid: previous.compute_txid().to_string(),
        });
        let binding = |id: &str| BoundRun {
            directory: PathBuf::from(format!("/synthetic/{id}")),
            run_id: id.into(),
            state_sha256: String::new(),
            descriptor_sha256: String::new(),
            checkpoint_sha256: String::new(),
        };
        let intent = Intent {
            version: 1,
            network: "testnet4".into(),
            authorization_ref: "coincube#1137:testnet4-sponsor".into(),
            source: binding("source"),
            target: binding("target"),
            previous_txid: previous.compute_txid().to_string(),
            previous_vout: 1,
            previous_value_sats: 989_000,
            previous_raw_sha256: sha256::Hash::hash(
                hex::encode(consensus::serialize(&previous)).as_bytes(),
            )
            .to_string(),
            source_script: hex::encode(change.script_pubkey().as_bytes()),
            target_address: target.address(NETWORK).to_string(),
            target_script: hex::encode(target.script_pubkey().as_bytes()),
            amount_sats: 500_000,
            fee_sats: 1000,
            change_sats: 488_000,
            prior_sponsor: None,
        };
        (
            secrets,
            state,
            source_desc,
            target_desc,
            previous,
            intent,
            phone,
        )
    }

    #[test]
    fn primary_master_and_border_sign_change_and_interpreter_checks_witness() {
        let (secrets, state, source, target, previous, intent, _) = fixture();
        let psbt = unsigned_psbt(&intent, &state, &source, &target, previous).unwrap();
        let tx = sign(psbt.clone(), &secrets).unwrap();
        assert_eq!(tx.input[0].sequence, Sequence::MAX);
        assert_eq!(tx.output[0].value.to_sat(), 500_000);
        assert_eq!(tx.output[1].value.to_sat(), 488_000);
        assert!(!tx.input[0].witness.is_empty());
        let mut replay = psbt.clone();
        replay.inputs[0].final_script_witness = Some(tx.input[0].witness.clone());
        assert_eq!(
            replay
                .extract(&secp256k1::Secp256k1::verification_only())
                .unwrap(),
            tx
        );
        let mut forged = psbt;
        forged.inputs[0].final_script_witness = Some(Witness::from_slice(&[vec![1u8]]));
        assert!(forged
            .extract(&secp256k1::Secp256k1::verification_only())
            .is_err());
    }

    fn child_intent(prior: &Intent, parent: &Transaction) -> Intent {
        let mut child = prior.clone();
        child.version = 2;
        child.previous_txid = parent.compute_txid().to_string();
        child.previous_value_sats = parent.output[1].value.to_sat();
        child.previous_raw_sha256 =
            sha256::Hash::hash(hex::encode(consensus::serialize(parent)).as_bytes()).to_string();
        child.amount_sats = 100_000;
        child.change_sats = child.previous_value_sats - child.amount_sats - child.fee_sats;
        let intent_json = serde_json::to_string(prior).unwrap();
        child.prior_sponsor = Some(PriorSponsor {
            receipt_json: json!({"ok":true,"action":"sponsor","network":"testnet4",
                "intent_sha256":sha256::Hash::hash(intent_json.as_bytes()).to_string(),
                "raw_tx":hex::encode(consensus::serialize(parent)),"txid":child.previous_txid,
                "amount_sats":prior.amount_sats,"fee_sats":prior.fee_sats,"change_sats":prior.change_sats}).to_string(),
            intent_json,
            target: prior.target.clone(),
        });
        child
    }

    #[test]
    fn one_recorded_sponsor_hop_reconstructs_parent_and_signs_only_its_change() {
        let (secrets, state, source, target, previous, prior, _) = fixture();
        assert!(serde_json::to_value(&prior).unwrap().get("prior_sponsor").is_none());
        let parent = sign(unsigned_psbt(&prior, &state, &source, &target, previous).unwrap(), &secrets).unwrap();
        let child = child_intent(&prior, &parent);
        verify_prior_sponsor(&child, &state, &source, &target, &parent, &secrets).unwrap();
        let tx = sign(unsigned_psbt(&child, &state, &source, &target, parent.clone()).unwrap(), &secrets).unwrap();
        assert_eq!(tx.input[0].previous_output, OutPoint::new(parent.compute_txid(), 1));
        assert_eq!(tx.output[0].value.to_sat(), 100_000);
        assert_eq!(tx.output[1].value.to_sat(), 387_000);
        assert_eq!(tx.output[1].script_pubkey, parent.output[1].script_pubkey);
    }

    #[test]
    fn one_hop_rejects_valid_alternative_parent_signers_even_with_matching_txid() {
        let (secrets, state, source, target, previous, prior, phone) = fixture();
        let psbt = unsigned_psbt(&prior, &state, &source, &target, previous).unwrap();
        let secp = secp256k1::Secp256k1::new();
        let master_signed = master(&secrets).unwrap().sign_psbt(psbt.clone(), &secp).unwrap();
        let mut alternate = phone.sign_psbt(master_signed, &secp).unwrap();
        alternate.finalize_mut(&secp).unwrap();
        let alternate = alternate.extract(&secp).unwrap();
        let expected = sign(psbt, &secrets).unwrap();
        assert_eq!(alternate.compute_txid(), expected.compute_txid());
        assert_ne!(alternate, expected);
        let child = child_intent(&prior, &alternate);
        assert!(recorded_prior_sponsor(&child, &alternate).is_ok());
        assert_eq!(verify_prior_sponsor(&child, &state, &source, &target, &alternate, &secrets), Err("sponsor_parent_witness_unsupported"));
    }

    #[test]
    fn parent_receipt_source_depth_and_saved_spend_cannot_be_substituted() {
        let (secrets, mut state, source, target, previous, prior, _) = fixture();
        let parent = sign(unsigned_psbt(&prior, &state, &source, &target, previous).unwrap(), &secrets).unwrap();
        let child = child_intent(&prior, &parent);
        let mut missing = child.clone();
        missing.prior_sponsor = None;
        assert!(check_intent(&missing).is_err());
        let mut version_one = child.clone();
        version_one.version = 1;
        assert!(check_intent(&version_one).is_err());
        let mut wrong_source = child.clone();
        wrong_source.source.checkpoint_sha256 = "changed".into();
        assert!(recorded_prior_sponsor(&wrong_source, &parent).is_err());
        let mut nested = child.clone();
        nested.prior_sponsor.as_mut().unwrap().intent_json = serde_json::to_string(&child).unwrap();
        assert_eq!(recorded_prior_sponsor(&nested, &parent).err(), Some("sponsor_parent_depth"));
        let mut changed_receipt = child.clone();
        let recorded = changed_receipt.prior_sponsor.as_mut().unwrap();
        let mut receipt: Value = serde_json::from_str(&recorded.receipt_json).unwrap();
        receipt["raw_tx"] = json!("00");
        recorded.receipt_json = receipt.to_string();
        assert_eq!(recorded_prior_sponsor(&changed_receipt, &parent).err(), Some("sponsor_receipt_changed"));
        let mut changed_descriptor = child.clone();
        changed_descriptor.prior_sponsor.as_mut().unwrap().target.descriptor_sha256 = "other".into();
        assert!(recorded_prior_sponsor(&changed_descriptor, &parent).is_err());
        state.spend.as_mut().unwrap().unsigned_hex.push_str("00");
        assert!(verify_prior_sponsor(&child, &state, &source, &target, &parent, &secrets).is_err());
    }

    #[test]
    fn sponsor_refuses_receive_output_foreign_target_and_changed_saved_spend() {
        let (_, mut state, source, target, mut previous, mut intent, _) = fixture();
        intent.previous_vout = 0;
        assert!(unsigned_psbt(&intent, &state, &source, &target, previous.clone()).is_err());
        intent.previous_vout = 1;
        intent.target_script = intent.source_script.clone();
        assert!(unsigned_psbt(&intent, &state, &source, &target, previous.clone()).is_err());
        intent.target_script = hex::encode(
            target
                .receive_descriptor()
                .derive(
                    ChildNumber::Normal { index: 0 },
                    &secp256k1::Secp256k1::new(),
                )
                .script_pubkey()
                .as_bytes(),
        );
        state.spend.as_mut().unwrap().change_sats += 1;
        assert!(unsigned_psbt(&intent, &state, &source, &target, previous.clone()).is_err());
        state.spend.as_mut().unwrap().change_sats -= 1;
        previous.output[1].script_pubkey = source
            .receive_descriptor()
            .derive(
                ChildNumber::Normal { index: 0 },
                &secp256k1::Secp256k1::new(),
            )
            .script_pubkey();
        assert!(unsigned_psbt(&intent, &state, &source, &target, previous).is_err());
    }

    #[test]
    fn sponsor_never_signs_without_exact_primary_origins() {
        let (secrets, state, source, target, previous, intent, _) = fixture();
        let mut psbt = unsigned_psbt(&intent, &state, &source, &target, previous).unwrap();
        psbt.inputs[0].bip32_derivation.clear();
        assert_eq!(sign(psbt, &secrets), Err("sponsor_primary_origins"));
    }

    #[test]
    fn sponsor_requires_fresh_dual_chain_bound_unspent_evidence() {
        let (_, _, _, _, previous, intent, _) = fixture();
        let mut evidence: Vec<_> = ENDPOINTS
            .iter()
            .map(|endpoint| Evidence {
                endpoint: (*endpoint).into(),
                genesis: GENESIS.into(),
                observed_at: 1000,
                txid: intent.previous_txid.clone(),
                vout: 1,
                raw_tx: hex::encode(consensus::serialize(&previous)),
                spent: false,
            })
            .collect();
        assert!(evidence_transaction(&intent, &evidence, 1000).is_ok());
        assert_eq!(
            evidence_transaction(&intent, &evidence, 1121),
            Err("sponsor_evidence_invalid")
        );
        evidence[1].spent = true;
        assert_eq!(
            evidence_transaction(&intent, &evidence, 1000),
            Err("sponsor_evidence_invalid")
        );
        evidence[1].spent = false;
        evidence[1].genesis = "00".repeat(32);
        assert_eq!(
            evidence_transaction(&intent, &evidence, 1000),
            Err("sponsor_evidence_invalid")
        );
        evidence[1].genesis = GENESIS.into();
        evidence[1].raw_tx.push_str("00");
        assert_eq!(
            evidence_transaction(&intent, &evidence, 1000),
            Err("sponsor_evidence_disagreement")
        );
    }

    #[test]
    fn sponsor_requires_frozen_authorization_before_reading_any_run_or_keychain() {
        let (_, _, _, _, _, mut intent, _) = fixture();
        intent.network = "bitcoin".into();
        assert_eq!(check_intent(&intent), Err("sponsor_intent_invalid"));
        intent.network = "testnet4".into();
        intent.fee_sats = 1001;
        assert_eq!(check_intent(&intent), Err("sponsor_amount_bounds"));
        intent.fee_sats = 1000;
        intent.target.run_id = intent.source.run_id.clone();
        assert_eq!(check_intent(&intent), Err("sponsor_intent_invalid"));
    }

    #[test]
    fn replay_refuses_valid_master_and_phone_primary_pair() {
        let (secrets, state, source, target, previous, intent, phone) = fixture();
        let psbt = unsigned_psbt(&intent, &state, &source, &target, previous).unwrap();
        let expected = sign(psbt.clone(), &secrets).unwrap();
        let secp = secp256k1::Secp256k1::new();
        let signed = master(&secrets)
            .unwrap()
            .sign_psbt(psbt.clone(), &secp)
            .unwrap();
        let mut other = phone.sign_psbt(signed, &secp).unwrap();
        other.finalize_mut(&secp).unwrap();
        let other = other.extract(&secp).unwrap();
        assert_eq!(other.compute_txid(), expected.compute_txid());
        assert_ne!(other, expected);
        let receipt =
            json!({"intent_sha256":"fixed", "raw_tx":hex::encode(consensus::serialize(&other))});
        assert_eq!(
            check_replay(&receipt, &expected, "fixed"),
            Err("sponsor_receipt_changed")
        );
        let receipt =
            json!({"intent_sha256":"fixed", "raw_tx":hex::encode(consensus::serialize(&expected))});
        assert!(check_replay(&receipt, &sign(psbt, &secrets).unwrap(), "fixed").is_ok());
    }

    #[test]
    fn checkpoint_requires_authenticated_account_and_matching_vault_descriptor() {
        let (_, state, _, _, _, _, _) = fixture();
        let mut checkpoint = json!({"artifacts": {"account_identity":"a".repeat(64),
            "vault_id":state.vault_id.unwrap().to_string(),"descriptor_id":state.descriptor_id}});
        assert!(checkpoint_identity(&state, &checkpoint).is_ok());
        checkpoint["artifacts"]["vault_id"] = json!(999);
        assert!(checkpoint_identity(&state, &checkpoint).is_err());
        for malformed in [
            json!(state.vault_id),
            json!(true),
            json!("01"),
            json!("0"),
            json!("1 "),
        ] {
            checkpoint["artifacts"]["vault_id"] = malformed;
            assert!(checkpoint_identity(&state, &checkpoint).is_err());
        }
        checkpoint["artifacts"]["vault_id"] = json!(state.vault_id.unwrap().to_string());
        checkpoint["artifacts"]["descriptor_id"] = json!("wrong");
        assert!(checkpoint_identity(&state, &checkpoint).is_err());
        checkpoint["artifacts"]["descriptor_id"] = json!(state.descriptor_id);
        checkpoint["artifacts"]["account_identity"] = Value::Null;
        assert!(checkpoint_identity(&state, &checkpoint).is_err());
    }
}
