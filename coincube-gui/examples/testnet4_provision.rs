//! Run-owned Testnet4 provisioning. Private stdin JSON in, sanitized JSON out.
//! Requires integration-tests and the signed macOS device-secret entitlement.

#[cfg(feature = "integration-tests")]
#[path = "testnet4_provision/secret_store.rs"]
mod secret_store;

#[cfg(not(feature = "integration-tests"))]
fn main() {
    println!("{{\"ok\":false,\"network\":\"testnet4\",\"code\":\"integration_feature_required\"}}");
    std::process::exit(2);
}

#[cfg(feature = "integration-tests")]
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    match std::panic::catch_unwind(run::main).unwrap_or(Err("native_panic")) {
        Ok(value) => println!("{}", value),
        Err(code) => {
            println!(
                "{}",
                serde_json::json!({"ok":false,"network":"testnet4","code":code})
            );
            std::process::exit(2);
        }
    }
}

#[cfg(feature = "integration-tests")]
mod run {
    mod bootstrap {
        include!("testnet4_provision/bootstrap.rs");
    }
    use super::secret_store;
    use coincube_core::{
        bip39,
        border_wallet::{
            build_mnemonic, derive_enrollment, CellRef, GridRecoveryPhrase, OrderedPattern,
            WordGrid,
        },
        descriptors::{CoincubeDescriptor, CoincubePolicy, PathInfo},
        miniscript::{
            bitcoin::{
                self,
                bip32::{ChildNumber, DerivationPath, Fingerprint},
                consensus,
                psbt::Psbt,
                secp256k1, Address, Amount, Network, OutPoint, ScriptBuf, Sequence, Transaction,
                TxIn, TxOut, Witness,
            },
            descriptor::DescriptorPublicKey,
            psbt::PsbtExt,
        },
        signer::MasterSigner,
    };
    use coincube_gui::{
        app::{
            settings::{
                self, CubeSettings, GridSeedSource, KeySetting, Settings, VaultIdentity, WalletId,
            },
            wallet::descriptor_id_fingerprint,
        },
        dir::CoincubeDirectory,
        installer::{
            self,
            integration::{ConnectVaultMemberPayload, Context, PathKind, RemoteBackend},
        },
        services::{
            coincube::{CoincubeClient, CoincubeError, CubeKeyRaw, RegisterCubeRequest},
            connect::{
                client::cache::ConnectCache,
                crypto::{resolve_key_xpub, CubeEncryptionKey},
            },
            unlock,
        },
        signer::Signer,
    };
    use serde::{Deserialize, Serialize};
    use serde_json::{json, Value};
    use std::{
        collections::HashMap,
        fs::{self, OpenOptions},
        io::{Read, Write},
        path::{Path, PathBuf},
        str::FromStr,
        sync::{Arc, Mutex},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };
    use zeroize::{Zeroize, Zeroizing};

    type Result<T> = std::result::Result<T, &'static str>;
    const NETWORK: Network = Network::Testnet4;
    const RECOVERY_BLOCKS: u16 = 65535;
    const MAX_FILE: u64 = 4_000_000;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        action: String,
        network: String,
        profile_dir: PathBuf,
        run_dir: PathBuf,
        run_id: String,
        target_key_id: Option<String>,
        target_fingerprint: Option<String>,
        phone_key_file: Option<PathBuf>,
        funding_tx_file: Option<PathBuf>,
        funding_vout: Option<u32>,
        amount_sats: Option<u64>,
        fee_sats: Option<u64>,
        signed_psbt_file: Option<PathBuf>,
        secret_name: Option<String>,
        secret_value: Option<Value>,
        expected_previous_sha256: Option<String>,
        expected_email: Option<String>,
        session_namespace: Option<String>,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Secrets {
        run_id: String,
        profile_dir: PathBuf,
        master_phrase: String,
        cube_pin: String,
        grid_phrase: String,
        pattern: Vec<(u16, u8)>,
        checksum_word: String,
        recipient_phrase: String,
    }
    impl Drop for Secrets {
        fn drop(&mut self) {
            self.master_phrase.zeroize();
            self.cube_pin.zeroize();
            self.grid_phrase.zeroize();
            self.pattern.zeroize();
            self.checksum_word.zeroize();
            self.recipient_phrase.zeroize();
        }
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct State {
        version: u32,
        run_id: String,
        profile_dir: PathBuf,
        cube_uuid: String,
        cube_name: String,
        cube_id: Option<u64>,
        owner_user_id: Option<u32>,
        registration_started: bool,
        wallet_id: Option<WalletId>,
        target_key_id: Option<String>,
        target_fingerprint: Option<String>,
        phone_descriptor_key: Option<String>,
        vault_id: Option<u64>,
        descriptor_id: Option<String>,
        spend: Option<Spend>,
    }

    #[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct Spend {
        funding_txid: String,
        funding_vout: u32,
        funding_sats: u64,
        amount_sats: u64,
        fee_sats: u64,
        recipient: String,
        change_address: String,
        change_sats: u64,
        unsigned_hex: String,
        txid: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PhoneKey {
        key_id: String,
        fingerprint: String,
        network: String,
        xpub: String,
        derivation_path: String,
    }

    fn check(yes: bool, code: &'static str) -> Result<()> {
        if yes {
            Ok(())
        } else {
            Err(code)
        }
    }
    fn private_read(path: &Path) -> Result<String> {
        check(path.is_absolute(), "absolute_paths_required")?;
        let meta = fs::symlink_metadata(path).map_err(|_| "private_file_unavailable")?;
        check(
            meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= MAX_FILE,
            "invalid_private_file",
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            check(
                meta.permissions().mode() & 0o077 == 0,
                "private_file_permissions_required",
            )?;
        }
        fs::read_to_string(path).map_err(|_| "private_file_unavailable")
    }
    fn write_private(path: &Path, value: &str) -> Result<()> {
        let parent = path.parent().ok_or("invalid_private_path")?;
        if let Ok(meta) = fs::symlink_metadata(path) {
            check(
                meta.is_file() && !meta.file_type().is_symlink(),
                "invalid_private_file",
            )?;
        }
        let tmp = parent.join(format!(
            ".provision-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let outcome = (|| {
            let mut file = options.open(&tmp).map_err(|_| "private_write_failed")?;
            file.write_all(value.as_bytes())
                .map_err(|_| "private_write_failed")?;
            file.sync_all().map_err(|_| "private_write_failed")?;
            fs::rename(&tmp, path).map_err(|_| "private_write_failed")?;
            fs::File::open(parent)
                .and_then(|dir| dir.sync_all())
                .map_err(|_| "private_write_failed")
        })();
        if outcome.is_err() {
            let _ = fs::remove_file(tmp);
        }
        outcome
    }
    fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
        let bytes = Zeroizing::new(
            serde_json::to_string(value).map_err(|_| "private_serialization_failed")?,
        );
        write_private(path, &bytes)
    }
    fn state_path(input: &Input) -> PathBuf {
        input.run_dir.join("provision.json")
    }
    fn descriptor_path(input: &Input) -> PathBuf {
        input.run_dir.join("descriptor.txt")
    }
    fn psbt_path(input: &Input) -> PathBuf {
        input.run_dir.join("local-signed.psbt")
    }
    fn save_state(input: &Input, state: &State) -> Result<()> {
        save(&state_path(input), state)
    }

    fn validate_input(input: &Input) -> Result<()> {
        check(input.network == "testnet4", "wrong_network")?;
        check(
            matches!(
                input.action.as_str(),
                "cube"
                    | "vault"
                    | "prepare_spend"
                    | "finalize"
                    | "secret_get"
                    | "secret_put"
                    | "bootstrap_auth"
            ),
            "invalid_action",
        )?;
        let uuid = uuid::Uuid::parse_str(&input.run_id).map_err(|_| "invalid_run_id")?;
        check(uuid.to_string() == input.run_id, "invalid_run_id")?;
        check(
            input.profile_dir.is_absolute() && input.run_dir.is_absolute(),
            "absolute_paths_required",
        )?;
        check(
            input.action.starts_with("secret_") || input.run_dir != input.profile_dir,
            "separate_run_directory_required",
        )?;
        if input.action == "vault" {
            check(
                input
                    .target_key_id
                    .as_ref()
                    .is_some_and(|id| id.parse::<u64>().unwrap_or(0) > 0),
                "target_required",
            )?;
            let fp = input
                .target_fingerprint
                .as_deref()
                .ok_or("target_required")?;
            check(
                fp.len() == 8
                    && fp
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "invalid_target",
            )?;
        }
        if input.action == "prepare_spend" {
            check(
                input.funding_tx_file.is_some() && input.funding_vout.is_some(),
                "funding_required",
            )?;
            check(
                input
                    .amount_sats
                    .is_some_and(|n| (1000..=100_000).contains(&n)),
                "amount_out_of_bounds",
            )?;
            check(
                input.fee_sats.is_some_and(|n| (100..=10_000).contains(&n)),
                "fee_out_of_bounds",
            )?;
        }
        if input.action == "finalize" {
            check(input.signed_psbt_file.is_some(), "signed_psbt_required")?;
        }
        Ok(())
    }

    /// An OS advisory lock disappears on process exit, including crashes.
    fn lock_run(input: &Input) -> Result<fs::File> {
        check(input.run_dir.is_dir(), "private_run_directory_required")?;
        let meta =
            fs::symlink_metadata(&input.run_dir).map_err(|_| "private_run_directory_required")?;
        check(
            !meta.file_type().is_symlink(),
            "private_run_directory_required",
        )?;
        #[cfg(unix)]
        {
            use std::os::{
                fd::AsRawFd,
                unix::fs::{OpenOptionsExt, PermissionsExt},
            };
            check(
                meta.permissions().mode() & 0o077 == 0,
                "private_directory_permissions_required",
            )?;
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(input.run_dir.join("provision.lock"))
                .map_err(|_| "run_lock_unavailable")?;
            // SAFETY: flock only borrows this open descriptor; the returned File
            // retains the lock until the complete invocation finishes.
            check(
                unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
                "run_busy",
            )?;
            Ok(file)
        }
        #[cfg(not(unix))]
        {
            Err("unix_runner_required")
        }
    }

    fn load_or_create(
        input: &Input,
        store: &dyn secret_store::Backend,
    ) -> Result<(Secrets, State)> {
        let path = input.run_dir.join("provision-secrets.json");
        let locator = secret_store::locator(&input.profile_dir, &input.run_id, "provision_backup")?;
        let _lock = secret_store::lock(&input.profile_dir, &locator)?;
        let locator_path = input.run_dir.join("provision-secrets.keychain.json");
        if locator_path.exists() {
            let existing: secret_store::Locator =
                serde_json::from_str(&private_read(&locator_path)?)
                    .map_err(|_| "invalid_secret_locator")?;
            check(existing == locator, "run_identity_mismatch")?;
        }
        let stored = secret_store::get(store, &locator)?;
        let secrets = if path.exists() {
            // Migrate only the existing, validated backup for this exact run.
            // Retain the old file until protected storage and its readback pass.
            let bytes = Zeroizing::new(private_read(&path)?);
            let legacy: Secrets = serde_json::from_str(&bytes).map_err(|_| "invalid_run_backup")?;
            check(
                legacy.run_id == input.run_id && legacy.profile_dir == input.profile_dir,
                "run_identity_mismatch",
            )?;
            let _ = master(&legacy)?;
            let _ = border(&legacy)?;
            let value = serde_json::to_value(&legacy).map_err(|_| "invalid_run_backup")?;
            if let Some(existing) = stored {
                check(existing.value == value, "secret_revision_conflict")?;
            } else {
                secret_store::put(store, &locator, &value, None)?;
            }
            let verified = secret_store::get(store, &locator)?.ok_or("secret_readback_failed")?;
            check(verified.value == value, "secret_readback_failed")?;
            save(&locator_path, &locator)?;
            fs::remove_file(&path).map_err(|_| "plaintext_backup_removal_failed")?;
            fs::File::open(&input.run_dir)
                .and_then(|dir| dir.sync_all())
                .map_err(|_| "plaintext_backup_removal_failed")?;
            legacy
        } else if let Some(stored) = stored {
            serde_json::from_value::<Secrets>(stored.value).map_err(|_| "invalid_run_backup")?
        } else {
            check(!locator_path.exists(), "backup_keychain_item_missing")?;
            check(
                input.action == "cube" && !state_path(input).exists(),
                "run_not_provisioned",
            )?;
            let master = MasterSigner::generate(NETWORK).map_err(|_| "random_generation_failed")?;
            let recipient =
                MasterSigner::generate(NETWORK).map_err(|_| "random_generation_failed")?;
            let grid_phrase =
                GridRecoveryPhrase::generate().map_err(|_| "random_generation_failed")?;
            let mut pattern = OrderedPattern::new();
            while !pattern.is_complete() {
                use rand::Rng;
                let cell = CellRef::new(
                    rand::thread_rng().gen_range(0..WordGrid::ROWS) as u16,
                    rand::thread_rng().gen_range(0..WordGrid::COLS) as u8,
                );
                if !pattern.cells().contains(&cell) {
                    pattern.add(cell).map_err(|_| "border_generation_failed")?;
                }
            }
            let (_, checksum) = build_mnemonic(&grid_phrase.generate_grid(), &pattern)
                .map_err(|_| "border_generation_failed")?;
            let value = Secrets {
                run_id: input.run_id.clone(),
                profile_dir: input.profile_dir.clone(),
                master_phrase: master.words().join(" "),
                cube_pin: format!("{:04}", rand::random::<u16>() % 10_000),
                grid_phrase: grid_phrase.as_str().to_owned(),
                pattern: pattern.cells().iter().map(|c| (c.row, c.col)).collect(),
                checksum_word: checksum.to_owned(),
                recipient_phrase: recipient.words().join(" "),
            };
            let value = serde_json::to_value(&value).map_err(|_| "invalid_run_backup")?;
            secret_store::put(store, &locator, &value, None)?;
            // Verify the durable copy, not the in-memory value used to write it.
            serde_json::from_value::<Secrets>(
                secret_store::get(store, &locator)?
                    .ok_or("secret_readback_failed")?
                    .value,
            )
            .map_err(|_| "backup_readback_failed")?
        };
        check(
            secrets.run_id == input.run_id && secrets.profile_dir == input.profile_dir,
            "run_identity_mismatch",
        )?;
        save(&locator_path, &locator)?;
        let state = if state_path(input).exists() {
            serde_json::from_str::<State>(&private_read(&state_path(input))?)
                .map_err(|_| "invalid_run_checkpoint")?
        } else {
            check(input.action == "cube", "run_not_provisioned")?;
            let state = State {
                version: 1,
                run_id: input.run_id.clone(),
                profile_dir: input.profile_dir.clone(),
                cube_uuid: input.run_id.clone(),
                cube_name: format!("Testnet4 QA {}", input.run_id),
                cube_id: None,
                owner_user_id: None,
                registration_started: false,
                wallet_id: None,
                target_key_id: None,
                target_fingerprint: None,
                phone_descriptor_key: None,
                vault_id: None,
                descriptor_id: None,
                spend: None,
            };
            save_state(input, &state)?;
            state
        };
        check(
            state.version == 1
                && state.run_id == input.run_id
                && state.profile_dir == input.profile_dir
                && state.cube_uuid == input.run_id,
            "run_identity_mismatch",
        )?;
        let _ = master(&secrets)?;
        let _ = border(&secrets)?;
        Ok((secrets, state))
    }
    fn master(secrets: &Secrets) -> Result<MasterSigner> {
        let mnemonic = bip39::Mnemonic::parse_in(bip39::Language::English, &secrets.master_phrase)
            .map_err(|_| "invalid_run_backup")?;
        MasterSigner::from_mnemonic(NETWORK, mnemonic).map_err(|_| "invalid_run_backup")
    }
    fn border(secrets: &Secrets) -> Result<coincube_core::border_wallet::BorderWalletEnrollment> {
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
        derive_enrollment(&mnemonic, NETWORK, &secp256k1::Secp256k1::new())
            .map_err(|_| "invalid_border_backup")
    }

    // This endpoint uses check_success rather than the typed recovery-kit
    // NotFound variant. Only an actual 404 permits first-time vault creation.
    fn vault_is_absent(error: &CoincubeError) -> bool {
        matches!(
            error,
            CoincubeError::NotFound
                | CoincubeError::Unsuccessful(
                    coincube_gui::services::http::NotSuccessResponseInfo {
                        status_code: 404,
                        ..
                    }
                )
        )
    }

    fn rest_error(error: CoincubeError) -> &'static str {
        if error.is_auth_error() {
            "connect_auth_required"
        } else if error.is_not_found() {
            "connect_fixture_not_found"
        } else if error.is_transient() {
            "connect_unavailable"
        } else {
            "connect_request_refused"
        }
    }
    fn client(input: &Input) -> Result<CoincubeClient> {
        let network = CoincubeDirectory::new(input.profile_dir.clone()).network_directory(NETWORK);
        let _ = private_read(&network.path().join("connect.json"))?;
        let cache = ConnectCache::from_file(&network).map_err(|_| "connect_auth_required")?;
        let account = cache.active_account().ok_or("connect_auth_required")?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "clock_error")?
            .as_secs() as i64;
        check(
            !account.tokens.access_token.is_empty() && account.tokens.expires_at > now + 30,
            "connect_auth_expired",
        )?;
        std::env::set_var("COINCUBE_API_URL", "https://api.coincube.io");
        let mut client = CoincubeClient::new();
        client.set_token(&account.tokens.access_token);
        Ok(client)
    }
    fn cube_settings(input: &Input, state: &State) -> Result<CubeSettings> {
        let network = CoincubeDirectory::new(input.profile_dir.clone()).network_directory(NETWORK);
        let settings = Settings::from_file(&network).map_err(|_| "cube_settings_unreadable")?;
        settings
            .cubes
            .into_iter()
            .find(|c| {
                c.id == state.cube_uuid && c.name == state.cube_name && c.network == NETWORK.into()
            })
            .ok_or("local_cube_missing")
    }
    fn unlock_master(input: &Input, state: &State, secrets: &Secrets) -> Result<MasterSigner> {
        let cube = cube_settings(input, state)?;
        let location = unlock::CubeLocation::new(&input.profile_dir, &cube);
        match unlock::unlock_blocking(&location, &secrets.cube_pin)
            .map_err(|_| "cube_unlock_failed")?
        {
            unlock::PinOutcome::Unlock(signer)
                if signer.words().join(" ") == secrets.master_phrase =>
            {
                Ok(*signer)
            }
            _ => Err("cube_backup_mismatch"),
        }
    }
    fn base_output(input: &Input, state: &State) -> Value {
        json!({"ok":true,"action":input.action,"network":"testnet4","cube_uuid":state.cube_uuid,"cube_id":state.cube_id,"cube_name":state.cube_name,"vault_id":state.vault_id.map(|id|id.to_string()),"descriptor_id":state.descriptor_id})
    }

    async fn cube(input: &Input, secrets: &Secrets, state: &mut State) -> Result<Value> {
        let client = client(input)?;
        let user = client.get_user().await.map_err(rest_error)?;
        if let Some(owner) = state.owner_user_id {
            check(owner == user.id, "run_account_changed")?;
        }
        state.owner_user_id = Some(user.id);
        save_state(input, state)?;
        let remote = client.list_cubes().await.map_err(rest_error)?;
        let existing = remote.iter().find(|c| c.uuid == state.cube_uuid);
        if let Some(row) = existing {
            check(state.registration_started, "remote_cube_not_run_owned")?;
            check(
                row.network == "testnet4" && row.name == state.cube_name,
                "remote_cube_mismatch",
            )?;
            let expected = CubeEncryptionKey::derive(&master(secrets)?, NETWORK).public_key_hex();
            check(
                row.encryption_pubkey
                    .as_ref()
                    .is_none_or(|key| key == &expected),
                "remote_cube_key_mismatch",
            )?;
            if let Some(id) = state.cube_id {
                check(id == row.id, "remote_cube_mismatch")?;
            }
        }
        let limits = client
            .get_cube_limits("testnet4")
            .await
            .map_err(rest_error)?;
        check(limits.network == "testnet4", "connect_cube_wrong_network")?;
        check(
            existing.is_some() || limits.current_count < limits.max_allowed as i64,
            "cube_limit_reached",
        )?;
        let words: Vec<String> = secrets
            .master_phrase
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let cube = coincube_gui::home::integration::create_testnet4_cube(
            CoincubeDirectory::new(input.profile_dir.clone()),
            uuid::Uuid::parse_str(&state.cube_uuid).map_err(|_| "invalid_run_id")?,
            state.cube_name.clone(),
            &secrets.cube_pin,
            &words,
            limits.max_allowed,
        )
        .await?;
        let remote_id = if let Some(row) = existing {
            row.id
        } else {
            // A retry lists by the durable UUID first. A lost POST response
            // never causes a replacement Cube UUID to be generated.
            state.registration_started = true;
            save_state(input, state)?;
            let registered = client
                .register_cube(RegisterCubeRequest {
                    uuid: state.cube_uuid.clone(),
                    name: state.cube_name.clone(),
                    network: "testnet4".into(),
                    has_vault: None,
                    spark_stable_balance: None,
                })
                .await
                .map_err(rest_error)?;
            let expected = CubeEncryptionKey::derive(&master(secrets)?, NETWORK).public_key_hex();
            check(
                registered.uuid == state.cube_uuid
                    && registered.name == state.cube_name
                    && registered.network == "testnet4"
                    && registered
                        .encryption_pubkey
                        .as_ref()
                        .is_none_or(|key| key == &expected),
                "remote_cube_mismatch",
            )?;
            registered.id
        };
        state.cube_id = Some(remote_id);
        save_state(input, state)?;
        let signer = unlock_master(input, state, secrets)?;
        let encryption = CubeEncryptionKey::derive(&signer, NETWORK);
        client
            .put_cube_encryption_pubkey(remote_id, &encryption.public_key_hex())
            .await
            .map_err(rest_error)?;
        let network = CoincubeDirectory::new(input.profile_dir.clone()).network_directory(NETWORK);
        settings::update_settings_file(&network, |mut data| {
            if let Some(c) = data.cubes.iter_mut().find(|c| c.id == state.cube_uuid) {
                c.remote_synced = true;
                c.connect_owner = Some(settings::ConnectOwner::from_user(&user));
                c.connect_encryption_pubkey = Some(encryption.public_key_hex());
            }
            Some(data)
        })
        .await
        .map_err(|_| "cube_settings_write_failed")?;
        let mut output = base_output(input, state);
        output["backup_verified"] = json!(true);
        output["master_fingerprint"] = json!(cube
            .master_signer_fingerprint
            .ok_or("master_identity_missing")?
            .to_string());
        Ok(output)
    }

    fn phone_descriptor(row: &CubeKeyRaw, key: &CubeEncryptionKey, cube_id: u64) -> Result<String> {
        check(
            row.network == "testnet4"
                && !row.is_owner_self_recovery()
                && !row.is_envelope_invalid(),
            "phone_key_not_eligible",
        )?;
        let xpub = resolve_key_xpub(row, Some(key), cube_id, NETWORK)
            .map_err(|_| "phone_xpub_unavailable")?;
        let path = DerivationPath::from_str(&row.derivation_path)
            .map_err(|_| "invalid_phone_derivation")?;
        let suffix = origin_suffix(&path);
        Ok(format!("[{}{}]{}/<0;1>/*", row.fingerprint, suffix, xpub))
    }
    fn bound_phone_descriptor(
        rows: &[CubeKeyRaw],
        target: &str,
        fingerprint: &str,
        encryption: &CubeEncryptionKey,
        cube_id: u64,
        exported: Option<&PhoneKey>,
        saved: Option<&str>,
    ) -> Result<String> {
        let matches: Vec<_> = rows
            .iter()
            .filter(|row| row.id.to_string() == target)
            .collect();
        check(matches.len() <= 1, "ambiguous_phone_key")?;
        let row = matches.first().ok_or("phone_key_not_attached")?;
        check(
            row.fingerprint == fingerprint,
            "phone_key_identity_mismatch",
        )?;
        // W3 responses can omit the legacy cubeId field. Membership must always
        // come from get_cube_keys; an explicit conflicting legacy value is invalid.
        check(
            row.cube_id == 0 || row.cube_id == cube_id,
            "phone_key_cube_mismatch",
        )?;
        let envelope = row
            .xpub_envelope
            .as_ref()
            .ok_or("phone_key_envelope_required")?;
        // This scenario attaches an already-existing personal key, so resealing
        // must bind its real row ID, not the legacy enrol-time zero ID.
        encryption
            .open(envelope, cube_id, row.id)
            .map_err(|_| "phone_key_envelope_binding")?;
        let phone = phone_descriptor(row, encryption, cube_id)?;
        if let Some(exported) = exported {
            check(
                exported.network == "testnet4"
                    && exported.key_id == target
                    && exported.fingerprint == fingerprint,
                "phone_key_identity_mismatch",
            )?;
            let path = DerivationPath::from_str(&exported.derivation_path)
                .map_err(|_| "invalid_phone_derivation")?;
            let expected = DescriptorPublicKey::from_str(&format!(
                "[{}{}]{}/<0;1>/*",
                exported.fingerprint,
                origin_suffix(&path),
                exported.xpub
            ))
            .map_err(|_| "invalid_phone_key_file")?;
            let actual =
                DescriptorPublicKey::from_str(&phone).map_err(|_| "invalid_descriptor_key")?;
            check(actual == expected, "phone_key_export_mismatch")?;
        }
        if let Some(saved) = saved {
            check(saved == phone, "phone_key_changed")?;
        }
        Ok(phone)
    }
    fn origin_suffix(path: &DerivationPath) -> String {
        if path.as_ref().is_empty() {
            String::new()
        } else {
            format!("/{}", path.to_string().trim_start_matches("m/"))
        }
    }
    fn descriptor(secrets: &Secrets, phone: &str) -> Result<CoincubeDescriptor> {
        let secp = secp256k1::Secp256k1::new();
        let master = master(secrets)?;
        let path = DerivationPath::from_str("m/48'/1'/0'/2'").map_err(|_| "invalid_derivation")?;
        let local = format!(
            "[{}{}]{}/<0;1>/*",
            master.fingerprint(&secp),
            origin_suffix(&path),
            master.xpub_at(&path, &secp)
        );
        let recovery = border(secrets)?;
        let phone_key =
            DescriptorPublicKey::from_str(phone).map_err(|_| "invalid_descriptor_key")?;
        let phone_fp = phone_key.master_fingerprint();
        check(
            phone_fp != master.fingerprint(&secp)
                && phone_fp != recovery.fingerprint
                && master.fingerprint(&secp) != recovery.fingerprint,
            "independent_signers_required",
        )?;
        let key = |branches| {
            format!(
                "[{}{}]{}/<{}>/*",
                recovery.fingerprint,
                origin_suffix(&recovery.derivation_path),
                recovery.xpub,
                branches
            )
        };
        let parse =
            |text: &str| DescriptorPublicKey::from_str(text).map_err(|_| "invalid_descriptor_key");
        Ok(CoincubeDescriptor::new(
            CoincubePolicy::new_legacy(
                PathInfo::Multi(2, vec![parse(&local)?, parse(phone)?, parse(&key("0;1"))?]),
                vec![(RECOVERY_BLOCKS, PathInfo::Single(parse(&key("2;3"))?))]
                    .into_iter()
                    .collect(),
            )
            .map_err(|_| "invalid_vault_policy")?,
        ))
    }

    async fn vault(input: &Input, secrets: &Secrets, state: &mut State) -> Result<Value> {
        let client = client(input)?;
        let user = client.get_user().await.map_err(rest_error)?;
        check(state.owner_user_id == Some(user.id), "run_account_changed")?;
        let cube_id = state.cube_id.ok_or("cube_not_registered")?;
        let local = cube_settings(input, state)?;
        let master = unlock_master(input, state, secrets)?;
        let target = input.target_key_id.as_ref().ok_or("target_required")?;
        let fp = input.target_fingerprint.as_ref().ok_or("target_required")?;
        if let Some(saved) = &state.target_key_id {
            check(
                saved == target && state.target_fingerprint.as_ref() == Some(fp),
                "run_target_changed",
            )?;
        }
        let rows = client
            .get_cube_keys(&state.cube_uuid)
            .await
            .map_err(rest_error)?;
        let exported: Option<PhoneKey> = input
            .phone_key_file
            .as_ref()
            .map(|path| {
                serde_json::from_str(&private_read(path)?).map_err(|_| "invalid_phone_key_file")
            })
            .transpose()?;
        let phone = bound_phone_descriptor(
            &rows,
            target,
            fp,
            &CubeEncryptionKey::derive(&master, NETWORK),
            cube_id,
            exported.as_ref(),
            state.phone_descriptor_key.as_deref(),
        )?;
        let descriptor = descriptor(secrets, &phone)?;
        let descriptor_id = descriptor_id_fingerprint(&descriptor).to_string();
        if let Some(saved) = &state.descriptor_id {
            check(saved == &descriptor_id, "descriptor_changed")?;
        }
        state.target_key_id = Some(target.clone());
        state.target_fingerprint = Some(fp.clone());
        state.phone_descriptor_key = Some(phone);
        state.descriptor_id = Some(descriptor_id.clone());
        if state.wallet_id.is_none() {
            state.wallet_id = Some(WalletId::generate(&descriptor));
        }
        save_state(input, state)?;
        write_private(&descriptor_path(input), &descriptor.to_string())?;
        let wallet_id = state.wallet_id.clone().ok_or("wallet_identity_missing")?;
        let root = CoincubeDirectory::new(input.profile_dir.clone());
        let network = root.network_directory(NETWORK);
        let disk = Settings::from_file(&network).map_err(|_| "cube_settings_unreadable")?;
        if !disk.wallets.iter().any(|w| w.wallet_id() == wallet_id) {
            let cache = ConnectCache::from_file(&network).map_err(|_| "connect_auth_required")?;
            let account = cache.active_account().ok_or("connect_auth_required")?;
            let mut ctx = Context::new(
                NETWORK,
                root.clone(),
                RemoteBackend::None,
                Some(&local),
                Some(client.clone()),
            );
            ctx.descriptor = Some(descriptor.clone());
            ctx.wallet_alias = state.cube_name.clone();
            ctx.cube_pin = Some(Zeroizing::new(secrets.cube_pin.clone()));
            ctx.use_coincube_connect = true;
            ctx.bitcoin_backend = Some(coincubed::config::BitcoinBackend::Esplora(
                installer::integration::testnet4_esplora_config(&account.tokens.access_token),
            ));
            let local_fp = master.fingerprint(&secp256k1::Secp256k1::new());
            let border_fp = border(secrets)?.fingerprint;
            let phone_fp = Fingerprint::from_str(fp).map_err(|_| "invalid_target")?;
            ctx.keys = HashMap::from([
                (
                    local_fp,
                    KeySetting {
                        name: "QA Master Signer".into(),
                        master_fingerprint: local_fp,
                        provider_key: None,
                        is_border_wallet: false,
                        grid_seed_source: None,
                        replay_protected: None,
                        keychain_key_id: None,
                    },
                ),
                (
                    phone_fp,
                    KeySetting {
                        name: "QA Keychain".into(),
                        master_fingerprint: phone_fp,
                        provider_key: None,
                        is_border_wallet: false,
                        grid_seed_source: None,
                        replay_protected: None,
                        keychain_key_id: Some(target.parse().map_err(|_| "invalid_target")?),
                    },
                ),
                (
                    border_fp,
                    KeySetting {
                        name: "QA Border Wallet".into(),
                        master_fingerprint: border_fp,
                        provider_key: None,
                        is_border_wallet: true,
                        grid_seed_source: Some(GridSeedSource::Independent),
                        replay_protected: None,
                        keychain_key_id: None,
                    },
                ),
            ]);
            ctx.keychain_keys_recorded = true;
            installer::integration::install_testnet4_wallet_resumable(
                ctx,
                wallet_id.clone(),
                Arc::new(Mutex::new(Signer::new(master))),
            )
            .await
            .map_err(|_| "local_vault_install_failed")?;
        }
        let identity = VaultIdentity::new(wallet_id, Some(&descriptor));
        if let Some(existing) = &local.vault_wallet_id {
            check(
                existing == &identity.wallet_id
                    && local.vault_fingerprint.as_ref() == Some(&descriptor_id),
                "existing_vault_conflict",
            )?;
        }
        settings::update_settings_file(&network, |mut data| {
            if let Some(cube) = data.cubes.iter_mut().find(|c| c.id == state.cube_uuid) {
                cube.set_vault(identity.clone());
            }
            Some(data)
        })
        .await
        .map_err(|_| "cube_settings_write_failed")?;
        let remote = match client.get_connect_vault(cube_id).await {
            Ok(vault) => vault,
            Err(error) if vault_is_absent(&error) => {
                installer::integration::create_connect_vault(
                    Some(client.clone()),
                    Some(state.cube_uuid.clone()),
                    Some(state.cube_name.clone()),
                    "testnet4".into(),
                    vec![ConnectVaultMemberPayload {
                        fingerprint: Fingerprint::from_str(fp).map_err(|_| "invalid_target")?,
                        key_id: target.parse().map_err(|_| "invalid_target")?,
                        contact_id: None,
                        path_kind: PathKind::Primary,
                    }],
                    Some((RECOVERY_BLOCKS as u32).div_ceil(144) as i32),
                    Some(descriptor_id.clone()),
                )
                .await
                .map_err(|_| "connect_vault_create_failed")?;
                client
                    .get_connect_vault(cube_id)
                    .await
                    .map_err(rest_error)?
            }
            Err(error) => return Err(rest_error(error)),
        };
        check(
            remote.cube_id == cube_id
                && remote.fingerprint == descriptor_id
                && remote.status.is_active()
                && remote.members.len() == 1
                && remote.members.iter().any(|m| {
                    m.key_id.map(|id| id.to_string()).as_ref() == Some(target)
                        && m.role == coincube_gui::services::coincube::VaultMemberRole::Keyholder
                }),
            "connect_vault_mismatch",
        )?;
        if let Some(saved) = state.vault_id {
            check(saved == remote.id, "connect_vault_mismatch")?;
        }
        state.vault_id = Some(remote.id);
        save_state(input, state)?;
        let mut output = base_output(input, state);
        let derived = descriptor.receive_descriptor().derive(
            ChildNumber::from_normal_idx(0).map_err(|_| "invalid_derivation")?,
            &secp256k1::Secp256k1::new(),
        );
        output["receive_address"] = json!(derived.address(NETWORK).to_string());
        output["descriptor_file"] = json!(descriptor_path(input));
        output["local_cube_verified"] = json!(true);
        Ok(output)
    }

    fn read_descriptor(
        input: &Input,
        secrets: &Secrets,
        state: &State,
    ) -> Result<CoincubeDescriptor> {
        let rebuilt = descriptor(
            secrets,
            state
                .phone_descriptor_key
                .as_deref()
                .ok_or("vault_not_provisioned")?,
        )?;
        let saved = CoincubeDescriptor::from_str(private_read(&descriptor_path(input))?.trim())
            .map_err(|_| "invalid_saved_descriptor")?;
        check(
            saved == rebuilt
                && state.descriptor_id.as_deref()
                    == Some(&descriptor_id_fingerprint(&saved).to_string()),
            "saved_descriptor_mismatch",
        )?;
        Ok(saved)
    }
    fn spend_output(input: &Input, state: &State, spend: &Spend) -> Value {
        let mut output = base_output(input, state);
        output["psbt_file"] = json!(psbt_path(input));
        output["descriptor_file"] = json!(descriptor_path(input));
        output["expected_txid"] = json!(spend.txid);
        output["recipient"] = json!(spend.recipient);
        output["amount_sats"] = json!(spend.amount_sats);
        output["fee_sats"] = json!(spend.fee_sats);
        output["inputs"] = json!([{"txid":spend.funding_txid,"vout":spend.funding_vout,"value_sats":spend.funding_sats}]);
        output["outputs"] = json!([{"address":spend.recipient,"value_sats":spend.amount_sats},{"address":spend.change_address,"value_sats":spend.change_sats}]);
        output
    }
    fn prepare(input: &Input, secrets: &Secrets, state: &mut State) -> Result<Value> {
        check(state.vault_id.is_some(), "vault_not_provisioned")?;
        let signer = unlock_master(input, state, secrets)?;
        let descriptor = read_descriptor(input, secrets, state)?;
        let secp = secp256k1::Secp256k1::new();
        let index = ChildNumber::from_normal_idx(0).map_err(|_| "invalid_derivation")?;
        let receive = descriptor.receive_descriptor().derive(index, &secp);
        let change = descriptor.change_descriptor().derive(index, &secp);
        let tx_hex = private_read(input.funding_tx_file.as_ref().ok_or("funding_required")?)?;
        let previous: Transaction = consensus::deserialize(
            &hex::decode(tx_hex.trim()).map_err(|_| "invalid_funding_transaction")?,
        )
        .map_err(|_| "invalid_funding_transaction")?;
        let vout = input.funding_vout.ok_or("funding_required")?;
        let prevout = previous
            .output
            .get(vout as usize)
            .ok_or("funding_output_missing")?
            .clone();
        check(
            prevout.script_pubkey == receive.script_pubkey(),
            "funding_script_mismatch",
        )?;
        let amount = input.amount_sats.ok_or("amount_out_of_bounds")?;
        let fee = input.fee_sats.ok_or("fee_out_of_bounds")?;
        let change_sats = prevout
            .value
            .to_sat()
            .checked_sub(amount)
            .and_then(|v| v.checked_sub(fee))
            .ok_or("insufficient_funding")?;
        check(
            (1000..=10_000_000).contains(&change_sats),
            "change_out_of_bounds",
        )?;
        let recipient_mnemonic =
            bip39::Mnemonic::parse_in(bip39::Language::English, &secrets.recipient_phrase)
                .map_err(|_| "invalid_recipient_backup")?;
        let recipient_signer = MasterSigner::from_mnemonic(NETWORK, recipient_mnemonic)
            .map_err(|_| "invalid_recipient_backup")?;
        let recipient_path =
            DerivationPath::from_str("m/84'/1'/0'/0/0").map_err(|_| "invalid_derivation")?;
        let recipient_pubkey = bitcoin::CompressedPublicKey(
            recipient_signer.xpub_at(&recipient_path, &secp).public_key,
        );
        let recipient = Address::p2wpkh(&recipient_pubkey, NETWORK);
        let tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::new(previous.compute_txid(), vout),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::default(),
            }],
            output: vec![
                TxOut {
                    value: Amount::from_sat(amount),
                    script_pubkey: recipient.script_pubkey(),
                },
                TxOut {
                    value: Amount::from_sat(change_sats),
                    script_pubkey: change.script_pubkey(),
                },
            ],
        };
        let spend = Spend {
            funding_txid: previous.compute_txid().to_string(),
            funding_vout: vout,
            funding_sats: prevout.value.to_sat(),
            amount_sats: amount,
            fee_sats: fee,
            recipient: recipient.to_string(),
            change_address: change.address(NETWORK).to_string(),
            change_sats,
            unsigned_hex: hex::encode(consensus::serialize(&tx)),
            txid: tx.compute_txid().to_string(),
        };
        if let Some(saved) = &state.spend {
            check(saved == &spend, "spend_intent_changed")?;
        }
        let mut psbt = Psbt::from_unsigned_tx(tx).map_err(|_| "invalid_spend")?;
        receive.update_psbt_in(&mut psbt.inputs[0]);
        psbt.inputs[0].witness_utxo = Some(prevout);
        psbt.inputs[0].non_witness_utxo = Some(previous);
        let psbt = signer
            .sign_psbt(psbt, &secp)
            .map_err(|_| "local_signing_failed")?;
        let progress = descriptor
            .partial_spend_info(&psbt)
            .map_err(|_| "invalid_signature_progress")?;
        check(
            progress.primary_path().sigs_count == 1,
            "expected_one_local_signature",
        )?;
        state.spend = Some(spend.clone());
        save_state(input, state)?;
        write_private(&psbt_path(input), &psbt.to_string())?;
        Ok(spend_output(input, state, &spend))
    }

    fn finalize(input: &Input, secrets: &Secrets, state: &State) -> Result<Value> {
        let spend = state.spend.as_ref().ok_or("spend_not_prepared")?;
        let descriptor = read_descriptor(input, secrets, state)?;
        let original = Psbt::from_str(private_read(&psbt_path(input))?.trim())
            .map_err(|_| "invalid_saved_psbt")?;
        let mut signed = Psbt::from_str(
            private_read(
                input
                    .signed_psbt_file
                    .as_ref()
                    .ok_or("signed_psbt_required")?,
            )?
            .trim(),
        )
        .map_err(|_| "invalid_signed_psbt")?;
        check(
            original.unsigned_tx == signed.unsigned_tx
                && hex::encode(consensus::serialize(&signed.unsigned_tx)) == spend.unsigned_hex
                && signed.unsigned_tx.compute_txid().to_string() == spend.txid,
            "transaction_intent_mismatch",
        )?;
        // The collect bridge returns an unfinalized signature-only merge. No
        // remote prevout, witness script, origin or final witness is trusted.
        let mut a = original.clone();
        let mut b = signed.clone();
        for input in &mut a.inputs {
            input.partial_sigs.clear();
        }
        for input in &mut b.inputs {
            input.partial_sigs.clear();
        }
        check(a == b, "signed_psbt_metadata_changed")?;
        check(
            signed.fee().map_err(|_| "invalid_fee")?.to_sat() == spend.fee_sats,
            "fee_intent_mismatch",
        )?;
        let secp = secp256k1::Secp256k1::verification_only();
        let progress = descriptor
            .partial_spend_info(&signed)
            .map_err(|_| "invalid_signature_progress")?;
        let phone = Fingerprint::from_str(
            state
                .target_fingerprint
                .as_deref()
                .ok_or("target_required")?,
        )
        .map_err(|_| "invalid_target")?;
        check(
            progress.primary_path().sigs_count >= 2
                && progress.primary_path().signed_pubkeys.contains_key(&phone),
            "phone_signature_missing",
        )?;
        let unified = coincube_core::psbt_unified::UnifiedPsbt::from_psbt(signed.clone())
            .map_err(|_| "invalid_signed_psbt")?;
        coincube_core::unified_finalize::verify_all_signatures(&unified, &secp)
            .map_err(|_| "invalid_signature")?;
        signed
            .finalize_mut(&secp)
            .map_err(|_| "finalization_failed")?;
        let tx = signed
            .extract(&secp)
            .map_err(|_| "signature_verification_failed")?;
        check(
            tx.compute_txid().to_string() == spend.txid,
            "transaction_intent_mismatch",
        )?;
        let mut output = spend_output(input, state, spend);
        output["raw_tx"] = json!(hex::encode(consensus::serialize(&tx)));
        output["txid"] = json!(tx.compute_txid().to_string());
        Ok(output)
    }

    async fn execute(input: Input) -> Result<Value> {
        validate_input(&input)?;
        let _lock = lock_run(&input)?;
        if input.action.starts_with("secret_") {
            let locator = secret_store::locator(
                &input.profile_dir,
                &input.run_id,
                input.secret_name.as_deref().ok_or("secret_name_required")?,
            )?;
            let _secret_lock = secret_store::lock(&input.profile_dir, &locator)?;
            if input.action == "secret_get" {
                return Ok(
                    match secret_store::get(&secret_store::Keychain, &locator)? {
                        Some(snapshot) => {
                            json!({"ok":true,"network":"testnet4","value":snapshot.value,"sha256":snapshot.sha256,"service":locator.service,"account":locator.account})
                        }
                        None => {
                            json!({"ok":true,"network":"testnet4","value":null,"sha256":null,"service":locator.service,"account":locator.account})
                        }
                    },
                );
            }
            let revision = secret_store::put(
                &secret_store::Keychain,
                &locator,
                input.secret_value.as_ref().ok_or("secret_value_required")?,
                input.expected_previous_sha256.as_deref(),
            )?;
            return Ok(
                json!({"ok":true,"network":"testnet4","stored":true,"sha256":revision,"service":locator.service,"account":locator.account}),
            );
        }
        if input.action == "bootstrap_auth" {
            return bootstrap::bootstrap(&input, &bootstrap::Production).await;
        }
        let (secrets, mut state) = load_or_create(&input, &secret_store::Keychain)?;
        match input.action.as_str() {
            "cube" => cube(&input, &secrets, &mut state).await,
            "vault" => vault(&input, &secrets, &mut state).await,
            "prepare_spend" => prepare(&input, &secrets, &mut state),
            "finalize" => finalize(&input, &secrets, &state),
            _ => Err("invalid_action"),
        }
    }
    pub fn main() -> Result<Value> {
        let mut data = Zeroizing::new(String::new());
        std::io::stdin()
            .take(32_769)
            .read_to_string(&mut data)
            .map_err(|_| "invalid_input")?;
        check(data.len() <= 32_768, "input_too_large")?;
        let input = serde_json::from_str(&data).map_err(|_| "invalid_input")?;
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|_| "runtime_unavailable")?
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(120), execute(input))
                    .await
                    .map_err(|_| "provision_timeout")?
            })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn vault_creation_requires_a_real_not_found_response() {
            use coincube_gui::services::http::NotSuccessResponseInfo;
            assert!(vault_is_absent(&CoincubeError::NotFound));
            for status_code in [400, 401, 403, 404, 409, 429, 500, 502, 503] {
                let error = CoincubeError::Unsuccessful(NotSuccessResponseInfo {
                    status_code,
                    text: "vault not found".into(),
                });
                assert_eq!(vault_is_absent(&error), status_code == 404);
            }
            assert!(!vault_is_absent(&CoincubeError::Api("404".into())));
            assert!(!vault_is_absent(&CoincubeError::RateLimited {
                retry_after: Duration::from_secs(60),
            }));
        }

        #[test]
        fn phone_export_requires_real_cube_membership_and_exact_key() {
            let signer = MasterSigner::from_str(NETWORK, "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about").unwrap();
            let secp = secp256k1::Secp256k1::new();
            let path = DerivationPath::from_str("m/48'/1'/0'/2'").unwrap();
            let encryption = CubeEncryptionKey::derive(&signer, NETWORK);
            let mut exported = PhoneKey {
                key_id: "7".into(),
                fingerprint: signer.fingerprint(&secp).to_string(),
                network: "testnet4".into(),
                xpub: signer.xpub_at(&path, &secp).to_string(),
                derivation_path: path.to_string(),
            };
            let mut row: CubeKeyRaw = serde_json::from_value(json!({
                "id":7,"cubeId":42,"name":"synthetic phone key", "xpub":exported.xpub,
                "fingerprint":exported.fingerprint,"derivationPath":exported.derivation_path,
                "network":"testnet4","status":"active"
            }))
            .unwrap();
            let resolve = |rows: &[CubeKeyRaw], export: &PhoneKey, saved: Option<&str>| {
                bound_phone_descriptor(
                    rows,
                    "7",
                    &signer.fingerprint(&secp).to_string(),
                    &encryption,
                    42,
                    Some(export),
                    saved,
                )
            };
            assert_eq!(resolve(&[], &exported, None), Err("phone_key_not_attached"));
            row.id = 8;
            assert_eq!(
                resolve(&[row.clone()], &exported, None),
                Err("phone_key_not_attached")
            );
            row.id = 7;
            row.cube_id = 20;
            assert_eq!(
                resolve(&[row.clone()], &exported, None),
                Err("phone_key_cube_mismatch")
            );
            row.cube_id = 42;
            assert_eq!(
                resolve(&[row.clone()], &exported, None),
                Err("phone_key_envelope_required")
            );
            // Published crypto known-answer vector: cube 42 / key 7.
            row.xpub_envelope = Some(serde_json::from_value(json!({
                "scheme":coincube_gui::services::inheritance::ecies::SCHEME,
                "recipient":"cube-owner", "aadKeyIdBound":true,
                "ephemeralPubkey":"032c0b7cf95324a07d05398b240174dc0c2be444d96b159aa6c7f7b1e668680991", "nonce":"0000000000000000cafebabe", "ciphertext":"fc13b1b9639e00e163b3664b62f516ad49d7f19c5383a758706ca813fa8e236cf14a4189aa61ee94801d31cb26a14a999eb5ea2c90a53bc704c5b262ff2b4cf984e97d7c92d13069b829b972c501190db9eaba00b8df84a25c78125e602cff3b037c7db65974b063084596a64667d5f92d647067c3c5453237d7e9e3573a57"
            })).unwrap());
            let original = resolve(&[row.clone()], &exported, None).unwrap();
            assert_eq!(
                resolve(&[row.clone()], &exported, Some(&original)),
                Ok(original.clone())
            );
            assert_eq!(
                resolve(&[row.clone()], &exported, Some("changed")),
                Err("phone_key_changed")
            );
            exported.xpub = MasterSigner::generate(NETWORK)
                .unwrap()
                .xpub_at(&path, &secp)
                .to_string();
            assert_eq!(
                resolve(&[row.clone()], &exported, Some(&original)),
                Err("phone_key_export_mismatch")
            );
            exported.xpub = row.xpub.clone();
            exported.derivation_path = "m/48'/1'/1'/2'".into();
            assert_eq!(
                resolve(&[row.clone()], &exported, None),
                Err("phone_key_export_mismatch")
            );
            exported.derivation_path = row.derivation_path.clone();
            row.fingerprint = "deadbeef".into();
            assert_eq!(
                resolve(&[row.clone()], &exported, None),
                Err("phone_key_identity_mismatch")
            );
            row.fingerprint = exported.fingerprint.clone();
            row.cube_id = 0; // W3 omits the legacy field; the scoped row is still required.
            assert_eq!(resolve(&[row.clone()], &exported, None), Ok(original));
            let bound = row.xpub_envelope.clone();
            row.xpub_envelope = Some(serde_json::from_value(json!({
                "scheme":coincube_gui::services::inheritance::ecies::SCHEME,
                "recipient":"cube-owner", "aadKeyIdBound":false,
                "ephemeralPubkey":"029ac20335eb38768d2052be1dbbc3c8f6178407458e51e6b4ad22f1d91758895b", "nonce":"00000000000000000badc0de", "ciphertext":"317bb132d9a0d43257a3519b396a3fae30502124a693d4d06648f319f96fc6924b2b2b5af537437d128122c7f4ea7732d394c968c51bae9f20a7eff8cbefe1fc1503d462a415b380c1771ac1a31731ea25f757721a08aeb198219b1d14258ee5acd4697d58d3f9f7bbeb58dabec39253a454fc68aba4620df5c7207a4cf774"
            })).unwrap());
            assert_eq!(
                resolve(&[row.clone()], &exported, None),
                Err("phone_key_envelope_binding")
            );
            row.xpub_envelope = bound;
            assert_eq!(
                bound_phone_descriptor(
                    &[row.clone()],
                    "7",
                    &exported.fingerprint,
                    &encryption,
                    43,
                    Some(&exported),
                    None
                ),
                Err("phone_key_envelope_binding")
            );
            assert_eq!(
                resolve(&[row.clone(), row], &exported, None),
                Err("ambiguous_phone_key")
            );
        }

        struct Temp(PathBuf);
        impl Temp {
            fn new() -> Self {
                let path = std::env::temp_dir()
                    .join(format!("coincube-provision-test-{}", uuid::Uuid::new_v4()));
                fs::create_dir(&path).unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
                }
                Self(path)
            }
        }
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        fn input(dir: &Temp) -> Input {
            fs::create_dir_all(dir.0.join("profile")).unwrap();
            Input {
                action: "cube".into(),
                network: "testnet4".into(),
                profile_dir: dir.0.join("profile"),
                run_dir: dir.0.clone(),
                run_id: uuid::Uuid::new_v4().to_string(),
                target_key_id: None,
                target_fingerprint: None,
                phone_key_file: None,
                funding_tx_file: None,
                funding_vout: None,
                amount_sats: None,
                fee_sats: None,
                signed_psbt_file: None,
                secret_name: None,
                secret_value: None,
                expected_previous_sha256: None,
                expected_email: None,
                session_namespace: None,
            }
        }
        #[test]
        fn rejects_other_network_and_unbounded_spend_before_side_effects() {
            let dir = Temp::new();
            let mut request = input(&dir);
            request.network = "bitcoin".into();
            assert_eq!(validate_input(&request), Err("wrong_network"));
            request.network = "testnet4".into();
            request.action = "prepare_spend".into();
            request.funding_tx_file = Some(dir.0.join("funding"));
            request.funding_vout = Some(0);
            request.amount_sats = Some(100_001);
            request.fee_sats = Some(1000);
            assert_eq!(validate_input(&request), Err("amount_out_of_bounds"));
            assert!(!state_path(&request).exists());
        }
        #[test]
        fn durable_backup_reconstructs_same_master_border_and_run_identity() {
            let dir = Temp::new();
            let mut request = input(&dir);
            let store = secret_store::tests::Memory::default();
            let (first, state) = load_or_create(&request, &store).unwrap();
            let (second, again) = load_or_create(&request, &store).unwrap();
            let secp = secp256k1::Secp256k1::new();
            assert!(
                master(&first).unwrap().fingerprint(&secp)
                    == master(&second).unwrap().fingerprint(&secp)
            );
            assert!(border(&first).unwrap().xpub == border(&second).unwrap().xpub);
            assert!(state.cube_uuid == again.cube_uuid);
            assert!(!dir.0.join("provision-secrets.json").exists());
            request.run_id = uuid::Uuid::new_v4().to_string();
            assert!(matches!(
                load_or_create(&request, &store),
                Err("run_identity_mismatch")
            ));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(dir.0.join("provision-secrets.keychain.json"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
        #[test]
        fn missing_keychain_item_never_resets_existing_run_backup() {
            let dir = Temp::new();
            let request = input(&dir);
            let store = secret_store::tests::Memory::default();
            let _ = load_or_create(&request, &store).unwrap();
            store.0.borrow_mut().clear();
            assert!(matches!(
                load_or_create(&request, &store),
                Err("backup_keychain_item_missing")
            ));
            assert!(store.0.borrow().is_empty());
        }
        #[test]
        fn legacy_plaintext_backup_is_removed_only_after_verified_migration() {
            let dir = Temp::new();
            let request = input(&dir);
            let store = secret_store::tests::Memory::default();
            let (first, _) = load_or_create(&request, &store).unwrap();
            let legacy = dir.0.join("provision-secrets.json");
            save(&legacy, &first).unwrap();
            store.0.borrow_mut().clear();
            let (again, _) = load_or_create(&request, &store).unwrap();
            assert!(again.master_phrase == first.master_phrase);
            assert!(!legacy.exists());
            assert_eq!(store.0.borrow().len(), 1);
        }
        #[test]
        #[cfg(unix)]
        fn run_lock_is_exclusive_and_released_on_drop() {
            let dir = Temp::new();
            let request = input(&dir);
            let lock = lock_run(&request).unwrap();
            assert!(matches!(lock_run(&request), Err("run_busy")));
            drop(lock);
            assert!(lock_run(&request).is_ok());
        }
        #[test]
        fn finalization_checks_actual_signatures_metadata_and_pinned_transaction() {
            let dir = Temp::new();
            let mut request = input(&dir);
            let store = secret_store::tests::Memory::default();
            let (secrets, mut state) = load_or_create(&request, &store).unwrap();
            let secp = secp256k1::Secp256k1::new();
            let phone = MasterSigner::generate(NETWORK).unwrap();
            let path = DerivationPath::from_str("m/48'/1'/0'/2'").unwrap();
            let phone_key = format!(
                "[{}{}]{}/<0;1>/*",
                phone.fingerprint(&secp),
                origin_suffix(&path),
                phone.xpub_at(&path, &secp)
            );
            let descriptor = descriptor(&secrets, &phone_key).unwrap();
            state.phone_descriptor_key = Some(phone_key);
            state.target_fingerprint = Some(phone.fingerprint(&secp).to_string());
            state.descriptor_id = Some(descriptor_id_fingerprint(&descriptor).to_string());
            let index = ChildNumber::from_normal_idx(0).unwrap();
            let receive = descriptor.receive_descriptor().derive(index, &secp);
            let change = descriptor.change_descriptor().derive(index, &secp);
            let previous = Transaction {
                version: bitcoin::transaction::Version::TWO,
                lock_time: bitcoin::absolute::LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: OutPoint::null(),
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::MAX,
                    witness: Witness::default(),
                }],
                output: vec![TxOut {
                    value: Amount::from_sat(20_000),
                    script_pubkey: receive.script_pubkey(),
                }],
            };
            let tx = Transaction {
                version: bitcoin::transaction::Version::TWO,
                lock_time: bitcoin::absolute::LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: OutPoint::new(previous.compute_txid(), 0),
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                    witness: Witness::default(),
                }],
                output: vec![
                    TxOut {
                        value: Amount::from_sat(10_000),
                        script_pubkey: receive.script_pubkey(),
                    },
                    TxOut {
                        value: Amount::from_sat(9_000),
                        script_pubkey: change.script_pubkey(),
                    },
                ],
            };
            state.spend = Some(Spend {
                funding_txid: previous.compute_txid().to_string(),
                funding_vout: 0,
                funding_sats: 20_000,
                amount_sats: 10_000,
                fee_sats: 1000,
                recipient: receive.address(NETWORK).to_string(),
                change_address: change.address(NETWORK).to_string(),
                change_sats: 9_000,
                unsigned_hex: hex::encode(consensus::serialize(&tx)),
                txid: tx.compute_txid().to_string(),
            });
            let mut psbt = Psbt::from_unsigned_tx(tx).unwrap();
            receive.update_psbt_in(&mut psbt.inputs[0]);
            psbt.inputs[0].witness_utxo = Some(previous.output[0].clone());
            psbt.inputs[0].non_witness_utxo = Some(previous);
            let local = master(&secrets).unwrap().sign_psbt(psbt, &secp).unwrap();
            write_private(&descriptor_path(&request), &descriptor.to_string()).unwrap();
            write_private(&psbt_path(&request), &local.to_string()).unwrap();
            let signed_file = dir.0.join("signed.psbt");
            request.signed_psbt_file = Some(signed_file.clone());
            request.action = "finalize".into();
            write_private(&signed_file, &local.to_string()).unwrap();
            assert!(matches!(
                finalize(&request, &secrets, &state),
                Err("phone_signature_missing")
            ));
            let signed = phone.sign_psbt(local, &secp).unwrap();
            write_private(&signed_file, &signed.to_string()).unwrap();
            let output = finalize(&request, &secrets, &state).unwrap();
            let tx: Transaction =
                consensus::deserialize(&hex::decode(output["raw_tx"].as_str().unwrap()).unwrap())
                    .unwrap();
            assert!(!tx.input[0].witness.is_empty());
            assert_eq!(output["txid"], state.spend.as_ref().unwrap().txid);
            let mut forged = signed.clone();
            forged.inputs[0].witness_utxo.as_mut().unwrap().value = Amount::from_sat(30_000);
            write_private(&signed_file, &forged.to_string()).unwrap();
            assert!(matches!(
                finalize(&request, &secrets, &state),
                Err("signed_psbt_metadata_changed")
            ));
            let mut altered = signed;
            altered.unsigned_tx.output[0].value = Amount::from_sat(10_001);
            write_private(&signed_file, &altered.to_string()).unwrap();
            assert!(matches!(
                finalize(&request, &secrets, &state),
                Err("transaction_intent_mismatch")
            ));
        }
        #[test]
        fn crash_after_encrypted_seed_write_is_resumable_only_for_exact_seed() {
            use coincube_core::{seed_crypt, signer::MnemonicFileName};
            let dir = Temp::new();
            let request = input(&dir);
            let store = secret_store::tests::Memory::default();
            let (secrets, state) = load_or_create(&request, &store).unwrap();
            let secp = secp256k1::Secp256k1::new();
            let phone = MasterSigner::generate(NETWORK).unwrap();
            let path = DerivationPath::from_str("m/48'/1'/0'/2'").unwrap();
            let phone_key = format!(
                "[{}{}]{}/<0;1>/*",
                phone.fingerprint(&secp),
                origin_suffix(&path),
                phone.xpub_at(&path, &secp)
            );
            let descriptor = descriptor(&secrets, &phone_key).unwrap();
            let wallet_id = WalletId::generate(&descriptor);
            let root = CoincubeDirectory::new(request.profile_dir.clone());
            let mut ctx = Context::new(NETWORK, root.clone(), RemoteBackend::None, None, None);
            ctx.cube_id = Some(state.cube_uuid.clone());
            ctx.cube_pin = Some(Zeroizing::new(secrets.cube_pin.clone()));
            ctx.descriptor = Some(descriptor);
            let signer = Signer::new(master(&secrets).unwrap());
            let device_secret = Zeroizing::new(rand::random::<[u8; 32]>());
            let verify = |ctx: &Context| {
                installer::integration::verify_existing_testnet4_seed(
                    ctx,
                    &wallet_id,
                    &signer,
                    &device_secret,
                )
            };
            assert!(!verify(&ctx).unwrap());
            // Exact crashpoint: the encrypted descriptor seed landed, but the
            // install has not yet appended WalletSettings to settings.json.
            signer
                .store_encrypted_for_chain(
                    &root,
                    NETWORK.into(),
                    &wallet_id.descriptor_checksum,
                    wallet_id.timestamp.unwrap(),
                    &secrets.cube_pin,
                    &state.cube_uuid,
                    Some(&device_secret),
                )
                .unwrap();
            assert!(Settings::from_file(&root.network_directory(NETWORK)).is_err());
            let seed_path = MasterSigner::mnemonics_folder_for_chain(root.path(), NETWORK.into())
                .join(
                    MnemonicFileName {
                        fingerprint: signer.fingerprint(),
                        descriptor_info: Some((
                            wallet_id.descriptor_checksum.clone(),
                            wallet_id.timestamp.unwrap(),
                        )),
                    }
                    .to_string(),
                );
            let original = fs::read(&seed_path).unwrap();
            // This true result is the resumable installer branch that skips
            // create_new and proceeds to the original settings write.
            assert!(verify(&ctx).unwrap());
            assert_eq!(fs::read(&seed_path).unwrap(), original);
            let mut wrong_chain = ctx.clone();
            wrong_chain.set_bitcoin_network(Network::Bitcoin).unwrap();
            assert!(verify(&wrong_chain).is_err());
            let mut wrong_pin = ctx.clone();
            wrong_pin.cube_pin = Some(Zeroizing::new("not-the-pin".into()));
            assert!(verify(&wrong_pin).is_err());
            let impostor = MasterSigner::generate(NETWORK).unwrap();
            let wrong_bytes = seed_crypt::encrypt(
                impostor.words().join(" ").as_bytes(),
                &secrets.cube_pin,
                &state.cube_uuid,
                Some(&device_secret),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&seed_path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            fs::write(&seed_path, &wrong_bytes).unwrap();
            assert!(verify(&ctx).is_err());
            assert_eq!(fs::read(&seed_path).unwrap(), wrong_bytes);
        }
        #[test]
        #[ignore = "requires the signed macOS device-secret runner; no API or phone used"]
        fn run_owned_cube_uses_real_backup_and_reopens_on_resume() {
            let dir = Temp::new();
            let request = input(&dir);
            let (secrets, state) = load_or_create(&request, &secret_store::Keychain).unwrap();
            let words: Vec<String> = secrets
                .master_phrase
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            let create = || {
                coincube_gui::home::integration::create_testnet4_cube(
                    CoincubeDirectory::new(request.profile_dir.clone()),
                    uuid::Uuid::parse_str(&state.cube_uuid).unwrap(),
                    state.cube_name.clone(),
                    &secrets.cube_pin,
                    &words,
                    10,
                )
            };
            let first = runtime.block_on(create()).unwrap();
            let second = runtime.block_on(create()).unwrap();
            assert_eq!(first.id, second.id);
            assert!(second.backed_up);
            assert!(second.creation_backup_bypass.is_none());
            unlock::device_secret::delete(&first.id).unwrap();
            #[cfg(target_os = "macos")]
            unlock::device_secret::integration::delete_test_item(
                &secret_store::locator(&request.profile_dir, &request.run_id, "provision_backup")
                    .unwrap()
                    .account,
            )
            .unwrap();
        }
    }
}
