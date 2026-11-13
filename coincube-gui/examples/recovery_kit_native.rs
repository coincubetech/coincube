//! Signed, feature-gated native integration runner. JSON stdout is public status
//! only. Seed, PIN, password and device factors never enter argv/env/plaintext files.
#[cfg(not(all(feature = "integration-tests", target_os = "macos")))]
fn main() {
    println!("{{\"ok\":false,\"code\":\"NATIVE_MACOS_SIGNED_BUILD_REQUIRED\"}}");
    std::process::exit(2);
}

#[cfg(all(feature = "integration-tests", target_os = "macos"))]
mod native {
    use coincube_core::{
        bip39,
        miniscript::bitcoin::hashes::{sha256, Hash},
    };
    use coincube_gui::{
        chain::ChainId,
        dir::CoincubeDirectory,
        recovery_native::{self, Identity},
        services::{
            coincube::{CoincubeClient, RecoveryKit},
            unlock::device_secret,
        },
    };
    use rand::RngCore;
    use serde::{Deserialize, Serialize};
    use std::{
        convert::TryInto,
        fs::{self, OpenOptions},
        io::{Read, Write},
        os::unix::{
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            io::AsRawFd,
        },
        path::{Path, PathBuf},
    };
    use zeroize::Zeroizing;
    type Result<T> = std::result::Result<T, &'static str>;
    fn check(ok: bool, code: &'static str) -> Result<()> {
        if ok {
            Ok(())
        } else {
            Err(code)
        }
    }
    fn hash(bytes: &[u8]) -> String {
        sha256::Hash::hash(bytes).to_string()
    }
    #[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct Config {
        version: u8,
        case_root: PathBuf,
        run_id: uuid::Uuid,
        chain: ChainId,
        auth_account: String,
        owner_id: u32,
        source_head: String,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {
        action: String,
        config: Config,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AuthInput {
        config: Config,
        access_token: String,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AbandonInput {
        config: Config,
        old_directory: PathBuf,
        access_token: String,
    }
    impl Drop for AbandonInput {
        fn drop(&mut self) {
            use zeroize::Zeroize;
            self.access_token.zeroize();
        }
    }
    impl Drop for AuthInput {
        fn drop(&mut self) {
            use zeroize::Zeroize;
            self.access_token.zeroize();
        }
    }
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct State {
        config: Config,
        stage: String,
        pending: Option<String>,
        creator: Process,
        restorer: Option<Process>,
        identity: Option<Identity>,
        kit: Option<Kit>,
        original_manifest: Option<String>,
        wipe_manifest: Option<String>,
        wipe_oracle: Option<recovery_native::UnfundedProof>,
        retirement_oracle: Option<recovery_native::UnfundedProof>,
        original_factor: Option<String>,
        canary_uuid: Option<uuid::Uuid>,
        canary: Option<String>,
        restored_factor: Option<String>,
        proof_sha256: Option<String>,
        remote_delete_started: bool,
    }
    #[derive(Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Process {
        pid: u32,
        start_id: String,
    }
    fn start_id(pid: u32) -> Result<String> {
        let output = std::process::Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .output()
            .map_err(|_| "native_kernel_process")?;
        check(output.status.success(), "native_kernel_process")?;
        let value = String::from_utf8(output.stdout)
            .map_err(|_| "native_kernel_process")?
            .trim()
            .to_owned();
        check(!value.is_empty(), "native_kernel_process")?;
        Ok(value)
    }
    fn this_process() -> Result<Process> {
        let pid = std::process::id();
        Ok(Process {
            pid,
            start_id: start_id(pid)?,
        })
    }
    #[derive(Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Kit {
        id: u64,
        cube_id: u64,
        encrypted_seed: String,
        encrypted_descriptor: String,
        scheme: String,
        created: String,
        updated: String,
    }
    impl Kit {
        fn from_api(k: RecoveryKit) -> Self {
            Self {
                id: k.id,
                cube_id: k.cube_id,
                encrypted_seed: k.encrypted_cube_seed,
                encrypted_descriptor: k.encrypted_wallet_descriptor,
                scheme: k.encryption_scheme,
                created: k.created_at,
                updated: k.updated_at,
            }
        }
        fn api(&self) -> RecoveryKit {
            RecoveryKit {
                id: self.id,
                cube_id: self.cube_id,
                encrypted_cube_seed: self.encrypted_seed.clone(),
                encrypted_wallet_descriptor: self.encrypted_descriptor.clone(),
                encryption_scheme: self.scheme.clone(),
                created_at: self.created.clone(),
                updated_at: self.updated.clone(),
            }
        }
    }
    fn absent(path: &Path) -> Result<bool> {
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(_) => Err("native_absence_unknown"),
        }
    }

    fn account(c: &Config, role: &str) -> String {
        hash(
            format!(
                "recovery-native-v1:{}:{}:{}:{}",
                c.case_root.display(),
                c.run_id,
                c.chain.api_str(),
                role
            )
            .as_bytes(),
        )
    }
    fn load(c: &Config, role: &str) -> Result<Zeroizing<Vec<u8>>> {
        let a = account(c, role);
        device_secret::integration::verify(&a).map_err(|_| "native_secret_protection")?;
        device_secret::integration::load(&a)
            .map_err(|_| "native_keychain_blocked")?
            .ok_or("native_secret_missing")
    }
    fn put(c: &Config, role: &str, value: &[u8]) -> Result<()> {
        let a = account(c, role);
        check(
            device_secret::integration::add(&a, value).map_err(|_| "native_keychain_blocked")?,
            "native_secret_already_exists",
        )?;
        check(load(c, role)?.as_slice() == value, "native_secret_readback")
    }
    fn string(bytes: Zeroizing<Vec<u8>>) -> Result<Zeroizing<String>> {
        Ok(Zeroizing::new(
            std::str::from_utf8(&bytes)
                .map_err(|_| "native_secret_encoding")?
                .to_owned(),
        ))
    }
    fn random(size: usize) -> Zeroizing<Vec<u8>> {
        let mut bytes = Zeroizing::new(vec![0; size]);
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        bytes
    }
    fn private(path: &Path) -> Result<Vec<u8>> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| "native_file_unavailable")?;
        let metadata = file.metadata().map_err(|_| "native_file_metadata")?;
        check(
            metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.permissions().mode() & 0o077 == 0
                && metadata.len() < 4_000_000,
            "native_private_file",
        )?;
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(4_000_000)
            .read_to_end(&mut bytes)
            .map_err(|_| "native_file_read")?;
        Ok(bytes)
    }
    fn save(root: &Path, state: &State) -> Result<()> {
        let bytes = serde_json::to_vec(state).map_err(|_| "native_state_encode")?;
        write(root, "state.next", &bytes)?;
        fs::rename(root.join("state.next"), root.join("state.json"))
            .map_err(|_| "native_state_commit")?;
        fs::File::open(root)
            .and_then(|f| f.sync_all())
            .map_err(|_| "native_state_sync")
    }
    fn write(root: &Path, name: &str, bytes: &[u8]) -> Result<()> {
        let path = root.join(name);
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| "native_file_create")?;
        f.write_all(bytes)
            .and_then(|_| f.sync_all())
            .map_err(|_| "native_file_sync")
    }
    fn preflight(c: &Config) -> Result<()> {
        check(
            c.version == 1
                && c.case_root.is_absolute()
                && c.case_root
                    .file_name()
                    .is_some_and(|s| s.to_str() == Some(&c.run_id.to_string()))
                && c.owner_id > 0,
            "native_config",
        )?;
        recovery_native::allowed_chain(c.chain)?;
        check(
            std::env::var("COINCUBE_API_URL")
                .ok()
                .is_none_or(|url| url == "https://api.coincube.io"),
            "native_production_endpoint",
        )?;
        std::env::set_var("COINCUBE_API_URL", "https://api.coincube.io");
        check(
            option_env!("COINCUBE_NATIVE_RECOVERY_PREREQUISITE") == Some("1"),
            "NATIVE_RECOVERY_PR1174_BLOCKED",
        )?;
        check(
            option_env!("COINCUBE_NATIVE_RECOVERY_SOURCE") == Some(c.source_head.as_str()),
            "native_source_binding",
        )?;
        check(c.auth_account == account(c, "auth"), "native_auth_scope")?;
        check(
            c.auth_account.len() == 64 && c.auth_account.bytes().all(|b| b.is_ascii_hexdigit()),
            "native_auth_handle",
        )
    }
    fn process_absent(process: &Process) -> Result<()> {
        check(
            process.pid != std::process::id(),
            "native_prior_process_alive",
        )?;
        match start_id(process.pid) {
            Ok(current) => check(current != process.start_id, "native_prior_process_alive"),
            Err(_) => check(
                unsafe { libc::kill(process.pid as i32, 0) } != 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
                "native_process_absence_unverified",
            ),
        }
    }
    fn manifest(path: &Path) -> Result<String> {
        fn walk(root: &Path, path: &Path, rows: &mut Vec<(PathBuf, String)>) -> Result<()> {
            let mut entries = fs::read_dir(path)
                .map_err(|_| "native_profile_inventory")?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|_| "native_profile_inventory")?;
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let p = e.path();
                let m = fs::symlink_metadata(&p).map_err(|_| "native_profile_inventory")?;
                check(!m.file_type().is_symlink(), "native_profile_symlink")?;
                if m.is_dir() {
                    walk(root, &p, rows)?;
                } else {
                    check(m.is_file(), "native_profile_type")?;
                    rows.push((
                        p.strip_prefix(root)
                            .map_err(|_| "native_profile_escape")?
                            .to_owned(),
                        hash(&fs::read(&p).map_err(|_| "native_profile_read")?),
                    ));
                }
            }
            Ok(())
        }
        let mut rows = Vec::new();
        walk(path, path, &mut rows)?;
        check(!rows.is_empty(), "native_profile_empty")?;
        Ok(hash(
            &serde_json::to_vec(&rows).map_err(|_| "native_inventory_encode")?,
        ))
    }
    fn binding(state: &State) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(&(
            &state.config,
            &state.identity,
            &state.kit,
            &state.original_manifest,
            &state.original_factor,
            &state.canary_uuid,
            &state.canary,
        ))
        .map_err(|_| "native_binding_encode")?;
        Ok(hash(&bytes).into_bytes())
    }
    fn validate_identity(c: &Config, identity: &Identity, kit: &Kit) -> Result<()> {
        use coincube_core::{descriptors::CoincubeDescriptor, miniscript::bitcoin::bip32::Xpub};
        use std::str::FromStr;
        check(
            identity.cube.id == c.run_id.to_string()
                && identity.cube.network == c.chain
                && identity.owner_id == c.owner_id
                && identity.remote_cube_id > 0
                && kit.cube_id == identity.remote_cube_id
                && kit.id > 0,
            "native_identity_binding",
        )?;
        let root = Xpub::from_str(&identity.root_xpub).map_err(|_| "native_root_xpub")?;
        let desc =
            CoincubeDescriptor::from_str(&identity.descriptor).map_err(|_| "native_descriptor")?;
        check(
            root.fingerprint().to_string() == identity.fingerprint
                && desc.contains_fingerprint_in_path(root.fingerprint(), None),
            "native_root_descriptor_binding",
        )
    }
    fn auth(c: &Config) -> Result<(CoincubeClient, Zeroizing<String>)> {
        device_secret::integration::verify(&c.auth_account)
            .map_err(|_| "native_auth_protection")?;
        let jwt = string(
            device_secret::integration::load(&c.auth_account)
                .map_err(|_| "native_auth_blocked")?
                .ok_or("native_auth_missing")?,
        )?;
        check(!jwt.is_empty(), "native_auth_missing")?;
        let mut client = CoincubeClient::new();
        client.set_token(&jwt);
        Ok((client, jwt))
    }
    async fn raw(
        client: &reqwest::Client,
        jwt: &str,
        path: &str,
        missing: bool,
    ) -> Result<Option<serde_json::Value>> {
        let response = client
            .get(format!("https://api.coincube.io/api/v1{path}"))
            .bearer_auth(jwt)
            .send()
            .await
            .map_err(|_| "native_retirement_api_blocked")?;
        if missing && response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        check(
            response.status().is_success(),
            "native_retirement_api_blocked",
        )?;
        let bytes = response
            .bytes()
            .await
            .map_err(|_| "native_retirement_api_blocked")?;
        check(bytes.len() < 4_000_000, "native_retirement_size")?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "native_retirement_schema")?;
        if path != "/approvals" {
            check(
                value.get("success").and_then(|v| v.as_bool()) == Some(true),
                "native_retirement_success",
            )?;
        }
        Ok(Some(value))
    }
    fn no_references(
        cube: &serde_json::Value,
        keys: &serde_json::Value,
        approvals: &serde_json::Value,
        identity: &Identity,
    ) -> Result<()> {
        check(
            cube.get("success").and_then(|v| v.as_bool()) == Some(true)
                && keys.get("success").and_then(|v| v.as_bool()) == Some(true),
            "native_retirement_success",
        )?;
        let cube = cube
            .get("data")
            .filter(|v| v.is_object())
            .ok_or("native_retirement_schema")?;
        let keys = keys
            .get("data")
            .and_then(|v| v.as_array())
            .ok_or("native_retirement_schema")?;
        check(
            cube.get("id").and_then(|v| v.as_u64()) == Some(identity.remote_cube_id)
                && cube.get("uuid").and_then(|v| v.as_str()) == Some(identity.cube.id.as_str())
                && cube.get("userId").and_then(|v| v.as_u64()) == Some(identity.owner_id as u64)
                && cube.get("network").and_then(|v| v.as_str())
                    == Some(identity.cube.network.api_str())
                && cube.get("name").and_then(|v| v.as_str()) == Some(identity.cube.name.as_str())
                && cube.get("status").and_then(|v| v.as_str()) == Some("active")
                && cube.get("hasRecoveryKit").and_then(|v| v.as_bool()) == Some(true),
            "native_retirement_binding",
        )?;
        for field in ["members", "pendingInvites"] {
            check(
                cube.get(field)
                    .and_then(|v| v.as_array())
                    .is_some_and(|v| v.is_empty()),
                "native_retirement_shared",
            )?;
        }
        for field in [
            "vault",
            "lightningAddress",
            "sparkStableBalance",
            "encryptionPubkey",
        ] {
            check(
                cube.get(field).is_some_and(|v| v.is_null()),
                "native_retirement_reference",
            )?;
        }
        check(
            keys.is_empty()
                && cube
                    .get("keys")
                    .is_none_or(|v| v.as_array().is_some_and(|rows| rows.is_empty())),
            "native_retirement_keys",
        )?;
        check(
            approvals
                .get("approvals")
                .and_then(|v| v.as_array())
                .is_some_and(|v| v.is_empty()),
            "native_retirement_approval",
        )
    }
    fn may_reconcile_absence(pending: Option<&str>, delete_started: bool) -> bool {
        pending == Some("retire_remote") && delete_started
    }
    async fn retire(c: &Config, state: &mut State, root: &Path, restored: &Path) -> Result<()> {
        use coincube_gui::services::connect::grpc::{
            self, interceptor::AuthInterceptor, session::GrpcSessionClient,
        };
        let identity = state.identity.clone().ok_or("native_identity_missing")?;
        let (client, jwt) = auth(c)?;
        check(
            client
                .get_user()
                .await
                .map_err(|_| "native_auth_blocked")?
                .id
                == c.owner_id,
            "native_account_changed",
        )?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|_| "native_retirement_http")?;
        let base = format!("/connect/cubes/{}", identity.remote_cube_id);
        let kit = state.kit.clone().ok_or("native_kit_missing")?;
        let pw = string(load(c, "recovery_password")?)?;
        recovery_native::validate_kit(&kit.api(), &pw, &identity)?;
        let Some(cube) = raw(&http, &jwt, &base, true).await? else {
            // A prior acknowledged intent may have lost the DELETE response.
            check(
                may_reconcile_absence(state.pending.as_deref(), state.remote_delete_started),
                "native_retirement_unexpected_absence",
            )?;
            return Ok(());
        };
        let server = client
            .download_recovery_kit(identity.remote_cube_id, &pw)
            .await
            .map_err(|_| "native_retirement_kit_blocked")?;
        check(
            server.id == kit.id
                && server.cube_id == kit.cube_id
                && server.encrypted_cube_seed == kit.encrypted_seed
                && server.encrypted_wallet_descriptor == kit.encrypted_descriptor,
            "native_retirement_kit_changed",
        )?;
        state.retirement_oracle = Some(
            recovery_native::require_unfunded(
                &CoincubeDirectory::new(restored.to_owned()),
                &identity,
                client.clone(),
            )
            .await?,
        );
        let keys = raw(&http, &jwt, &format!("{base}/keys"), false)
            .await?
            .ok_or("native_retirement_schema")?;
        let approvals = raw(&http, &jwt, "/approvals", false)
            .await?
            .ok_or("native_retirement_schema")?;
        no_references(&cube, &keys, &approvals, &identity)?;
        check(
            raw(&http, &jwt, &format!("{base}/vault"), true)
                .await?
                .is_none(),
            "native_retirement_vault",
        )?;
        let channel = grpc::create_channel("https://grpc.coincube.io:443")
            .await
            .map_err(|_| "native_retirement_pending_blocked")?;
        let mut pending = GrpcSessionClient::new(channel, AuthInterceptor::new(&jwt));
        check(
            pending
                .list_pending_sessions()
                .await
                .map_err(|_| "native_retirement_pending_blocked")?
                .sessions
                .is_empty()
                && pending
                    .list_pending_decrypt_requests()
                    .await
                    .map_err(|_| "native_retirement_pending_blocked")?
                    .requests
                    .is_empty(),
            "native_retirement_pending",
        )?;
        // Re-read exact sharing/key inventory immediately before the ONE DELETE.
        let fresh = raw(&http, &jwt, &base, false)
            .await?
            .ok_or("native_retirement_schema")?;
        let fresh_keys = raw(&http, &jwt, &format!("{base}/keys"), false)
            .await?
            .ok_or("native_retirement_schema")?;
        let fresh_approvals = raw(&http, &jwt, "/approvals", false)
            .await?
            .ok_or("native_retirement_schema")?;
        no_references(&fresh, &fresh_keys, &fresh_approvals, &identity)?;
        check(
            client
                .get_user()
                .await
                .map_err(|_| "native_auth_blocked")?
                .id
                == c.owner_id,
            "native_account_changed",
        )?;
        check(
            pending
                .list_pending_sessions()
                .await
                .map_err(|_| "native_retirement_pending_blocked")?
                .sessions
                .is_empty()
                && pending
                    .list_pending_decrypt_requests()
                    .await
                    .map_err(|_| "native_retirement_pending_blocked")?
                    .requests
                    .is_empty(),
            "native_retirement_pending",
        )?;
        state.remote_delete_started = true;
        save(root, state)?;
        client
            .delete_cube(&identity.remote_cube_id.to_string())
            .await
            .map_err(|_| "native_retirement_delete_uncertain")?;
        check(
            raw(&http, &jwt, &base, true).await?.is_none(),
            "native_retirement_readback",
        )
    }
    async fn observe_before_create(c: &Config, jwt: &str, client: &CoincubeClient) -> Result<()> {
        check(absent(&c.case_root)?, "native_run_not_fresh")?;
        let parent = c.case_root.parent().ok_or("native_root_parent")?;
        check(
            parent.canonicalize().map_err(|_| "native_root_parent")? == parent,
            "native_root_symlink_parent",
        )?;
        let metadata = fs::metadata(parent).map_err(|_| "native_root_parent")?;
        check(
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.permissions().mode() & 0o077 == 0,
            "native_parent_private",
        )?;
        check(
            device_secret::load_optional(&c.run_id.to_string())
                .map_err(|_| "native_keychain_blocked")?
                .is_none(),
            "native_original_factor_exists",
        )?;
        observe_remote_before_create(c, jwt, client).await
    }
    async fn observe_remote_before_create(
        c: &Config,
        jwt: &str,
        client: &CoincubeClient,
    ) -> Result<()> {
        use coincube_gui::services::connect::grpc::{
            self, interceptor::AuthInterceptor, session::GrpcSessionClient,
        };
        // Both inventories cover supported owned and contact-shared references,
        // across all networks. No label or hasVault boolean grants absence.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|_| "native_admission_http")?;
        for path in ["/connect/cubes", "/connect/cubes/shared-with-me"] {
            let value = raw(&http, jwt, path, false)
                .await?
                .ok_or("native_admission_inventory")?;
            let rows = value
                .get("data")
                .and_then(|v| v.as_array())
                .ok_or("native_admission_inventory")?;
            for row in rows {
                let uuid = row
                    .get("uuid")
                    .and_then(|v| v.as_str())
                    .ok_or("native_admission_inventory")?;
                check(
                    uuid != c.run_id.to_string(),
                    "native_remote_uuid_already_exists",
                )?;
            }
        }
        let approvals = raw(&http, jwt, "/approvals", false)
            .await?
            .ok_or("native_admission_pending")?;
        check(
            approvals
                .get("approvals")
                .and_then(|v| v.as_array())
                .is_some_and(|v| v.is_empty()),
            "native_admission_pending",
        )?;
        let channel = grpc::create_channel("https://grpc.coincube.io:443")
            .await
            .map_err(|_| "native_admission_pending")?;
        let mut pending = GrpcSessionClient::new(channel, AuthInterceptor::new(jwt));
        check(
            pending
                .list_pending_sessions()
                .await
                .map_err(|_| "native_admission_pending")?
                .sessions
                .is_empty()
                && pending
                    .list_pending_decrypt_requests()
                    .await
                    .map_err(|_| "native_admission_pending")?
                    .requests
                    .is_empty(),
            "native_admission_pending",
        )?;
        // Validate quota shape/owner even once our exact retirement frees a slot.
        let _ = recovery_native::create_capacity_available(c.chain, c.run_id, c.owner_id, client)
            .await?;
        Ok(())
    }

    fn owned_empty_device_lock(metadata: &fs::Metadata, uid: u32) -> bool {
        metadata.is_file()
            && metadata.uid() == uid
            && metadata.permissions().mode() & 0o7777 == 0o600
            && metadata.len() == 0
            && metadata.nlink() == 1
    }

    fn capacity_case_entries(case: &Path) -> Result<Option<fs::File>> {
        let mut device_lock = None;
        for entry in fs::read_dir(case).map_err(|_| "native_abandon_case")? {
            let entry = entry.map_err(|_| "native_abandon_case")?;
            match entry.file_name().to_str() {
                Some("state.json" | "run.lock") => (),
                Some(".device-secret.lock") => {
                    // Product get_or_create leaves this root-level empty lock
                    // after creating the QA canary. Never create/remove it here.
                    let path = entry.path();
                    let before =
                        fs::symlink_metadata(&path).map_err(|_| "native_abandon_device_lock")?;
                    check(
                        owned_empty_device_lock(&before, unsafe { libc::geteuid() }),
                        "native_abandon_device_lock",
                    )?;
                    let file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(&path)
                        .map_err(|_| "native_abandon_device_lock")?;
                    let opened = file.metadata().map_err(|_| "native_abandon_device_lock")?;
                    check(
                        owned_empty_device_lock(&opened, unsafe { libc::geteuid() })
                            && (before.dev(), before.ino()) == (opened.dev(), opened.ino()),
                        "native_abandon_device_lock",
                    )?;
                    check(
                        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }
                            == 0,
                        "native_abandon_device_lock_busy",
                    )?;
                    let current =
                        fs::symlink_metadata(&path).map_err(|_| "native_abandon_device_lock")?;
                    check(
                        owned_empty_device_lock(&current, unsafe { libc::geteuid() })
                            && (current.dev(), current.ino()) == (opened.dev(), opened.ino()),
                        "native_abandon_device_lock",
                    )?;
                    device_lock = Some(file);
                }
                _ => return Err("native_abandon_has_profile"),
            }
        }
        Ok(device_lock)
    }

    fn legacy_case_id(directory: &Path, chain: &str) -> uuid::Uuid {
        use coincube_core::miniscript::bitcoin::hashes::{sha1, Hash};
        let name = format!("native-recovery/{}/{chain}", directory.display());
        let mut material = uuid::Uuid::NAMESPACE_URL.as_bytes().to_vec();
        material.extend_from_slice(name.as_bytes());
        let digest = sha1::Hash::hash(&material).to_byte_array();
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        bytes[6] = (bytes[6] & 0x0f) | 0x50;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        uuid::Uuid::from_bytes(bytes)
    }

    const LEGACY_CAPACITY_SOURCE: &str = "74da94d2848783951ff9b305724340afbb230ab8";

    fn validate_capacity_failure(
        state: &State,
        case: &Path,
        id: uuid::Uuid,
        owner_id: u32,
    ) -> Result<()> {
        let c = &state.config;
        check(
            c.version == 1
                && c.run_id == id
                && c.case_root == case
                && c.chain == ChainId::Testnet4
                && c.owner_id == owner_id
                && c.source_head == LEGACY_CAPACITY_SOURCE
                && c.auth_account == account(c, "auth"),
            "native_abandon_identity",
        )?;
        check(
            state.stage == "new"
                && state.pending.as_deref() == Some("create_export")
                && state.identity.is_none()
                && state.kit.is_none()
                && state.restorer.is_none()
                && state.original_manifest.is_none()
                && state.wipe_manifest.is_none()
                && state.wipe_oracle.is_none()
                && state.retirement_oracle.is_none()
                && state.original_factor.is_none()
                && state.restored_factor.is_none()
                && state.proof_sha256.is_none()
                && !state.remote_delete_started
                && state.canary_uuid.is_some()
                && state
                    .canary
                    .as_ref()
                    .is_some_and(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit())),
            "native_abandon_stage",
        )?;
        Ok(())
    }

    async fn audit_capacity_abandonment(input: &AbandonInput) -> Result<()> {
        preflight(&input.config)?;
        let old = &input.old_directory;
        check(
            old.is_absolute()
                && old.canonicalize().map_err(|_| "native_abandon_directory")? == *old
                && old
                    != input
                        .config
                        .case_root
                        .parent()
                        .ok_or("native_abandon_directory")?,
            "native_abandon_directory",
        )?;
        let metadata = fs::symlink_metadata(old).map_err(|_| "native_abandon_directory")?;
        check(
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.permissions().mode() & 0o077 == 0,
            "native_abandon_directory",
        )?;
        let suite_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(old.join("run.lock"))
            .map_err(|_| "native_abandon_lock")?;
        check(
            unsafe { libc::flock(suite_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "native_abandon_busy",
        )?;
        let id = legacy_case_id(old, "testnet4");
        let b2_id = legacy_case_id(old, "bitcoin-blake2b-testnet4");
        check(
            absent(&old.join(b2_id.to_string()))?,
            "native_abandon_has_other_case",
        )?;
        let case = old.join(id.to_string());
        check(
            case.canonicalize().map_err(|_| "native_abandon_case")? == case,
            "native_abandon_case",
        )?;
        let case_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(case.join("run.lock"))
            .map_err(|_| "native_abandon_lock")?;
        check(
            unsafe { libc::flock(case_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "native_abandon_busy",
        )?;
        let state: State = serde_json::from_slice(&private(&case.join("state.json"))?)
            .map_err(|_| "native_abandon_state")?;
        let c = &state.config;
        validate_capacity_failure(&state, &case, id, input.config.owner_id)?;
        let mut client = CoincubeClient::new();
        client.set_token(&input.access_token);
        check(
            client
                .get_user()
                .await
                .map_err(|_| "native_auth_blocked")?
                .id
                == c.owner_id,
            "native_account_changed",
        )?;
        process_absent(&state.creator)?;
        let _device_lock = capacity_case_entries(&case)?;
        check(
            device_secret::load_optional(&id.to_string())
                .map_err(|_| "native_keychain_blocked")?
                .is_none(),
            "native_abandon_factor_exists",
        )?;
        check(
            device_secret::integration::load(&account(c, "quarantined_factor"))
                .map_err(|_| "native_keychain_blocked")?
                .is_none(),
            "native_abandon_factor_exists",
        )?;
        // The state canary UUID is not trusted to address a production item.
        // Only the fixed, derived QA-service handle is read; all old items stay.
        check(
            state.canary.is_some()
                && state.canary_uuid.is_some()
                && Some(hash(&load(c, "canary")?)) == state.canary,
            "native_abandon_canary",
        )?;
        observe_remote_before_create(c, &input.access_token, &client).await?;
        // Immutable add-only migration claim: this historical no-Cube failure
        // may authorize only one fresh consumer. Existing material is untouched.
        let handle =
            hash(format!("native-capacity-handoff-v1:{}:{}", old.display(), id).as_bytes());
        let value = hash(
            format!(
                "{}:{}:{}:{}:{}",
                input.config.source_head,
                input.config.case_root.display(),
                input.config.owner_id,
                hash(&private(&case.join("state.json"))?),
                hash(&private(&old.join("checkpoint.json"))?)
            )
            .as_bytes(),
        );
        let _ = device_secret::integration::add(&handle, value.as_bytes())
            .map_err(|_| "native_abandon_claim")?;
        device_secret::integration::verify(&handle).map_err(|_| "native_abandon_claim")?;
        check(
            device_secret::integration::load(&handle)
                .map_err(|_| "native_abandon_claim")?
                .is_some_and(|bytes| bytes.as_slice() == value.as_bytes()),
            "native_abandon_already_claimed",
        )?;
        Ok(())
    }

    async fn action(request: Request) -> Result<()> {
        let c = request.config;
        preflight(&c)?;
        let root = &c.case_root;
        let parent = root.parent().ok_or("native_root_parent")?;
        check(
            parent.canonicalize().map_err(|_| "native_root_parent")? == parent
                && !root.components().any(|p| {
                    matches!(
                        p,
                        std::path::Component::ParentDir | std::path::Component::CurDir
                    )
                }),
            "native_root_symlink_parent",
        )?;
        let parent_meta = fs::metadata(parent).map_err(|_| "native_root_parent")?;
        check(
            parent_meta.is_dir()
                && parent_meta.uid() == unsafe { libc::geteuid() }
                && parent_meta.permissions().mode() & 0o077 == 0,
            "native_parent_private",
        )?;
        let original = root.join("original");
        let restored = root.join("restored");
        if request.action == "preflight" {
            // Signed QA-service-only probe. Resumed lifecycle actions do not
            // probe/write production items before central identity validation.
            let bytes = hash(format!("{}:{}", c.source_head, c.run_id).as_bytes());
            let handle = account(&c, "build_probe");
            let _ = device_secret::integration::add(&handle, bytes.as_bytes())
                .map_err(|_| "native_signed_keychain_blocked")?;
            device_secret::integration::verify(&handle)
                .map_err(|_| "native_signed_keychain_protection")?;
            check(
                device_secret::integration::load(&handle)
                    .map_err(|_| "native_signed_keychain_blocked")?
                    .is_some_and(|value| value.as_slice() == bytes.as_bytes()),
                "native_signed_keychain_readback",
            )?;
            return Ok(());
        }
        if request.action == "create_export" {
            check(absent(root)?, "native_run_not_fresh")?;
            let (client, _) = auth(&c)?;
            recovery_native::admit_create(c.chain, c.run_id, c.owner_id, &client).await?;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(root)
                .map_err(|_| "native_run_create")?;
        }
        let m = fs::symlink_metadata(root).map_err(|_| "native_root_missing")?;
        check(
            m.is_dir()
                && m.uid() == unsafe { libc::geteuid() }
                && !m.file_type().is_symlink()
                && m.permissions().mode() & 0o077 == 0,
            "native_root_private",
        )?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root.join("run.lock"))
            .map_err(|_| "native_lock")?;
        check(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "native_run_busy",
        )?;
        let mut state = if request.action == "create_export" {
            State {
                config: c.clone(),
                stage: "new".into(),
                pending: None,
                creator: this_process()?,
                restorer: None,
                identity: None,
                kit: None,
                original_manifest: None,
                wipe_manifest: None,
                wipe_oracle: None,
                retirement_oracle: None,
                original_factor: None,
                canary_uuid: None,
                canary: None,
                restored_factor: None,
                proof_sha256: None,
                remote_delete_started: false,
            }
        } else {
            serde_json::from_slice::<State>(&private(&root.join("state.json"))?)
                .map_err(|_| "native_state_invalid")?
        };
        check(
            state.config == c
                && (state.pending.is_none()
                    || (request.action == "retire_remote"
                        && state.pending.as_deref() == Some("retire_remote"))),
            "native_uncertain_action",
        )?;
        if let Some(identity) = state.identity.as_ref() {
            validate_identity(
                &c,
                identity,
                state.kit.as_ref().ok_or("native_kit_missing")?,
            )?;
            check(
                load(&c, "provenance")?.as_slice() == binding(&state)?.as_slice(),
                "native_provenance_changed",
            )?;
        }
        let expected = match request.action.as_str() {
            "create_export" => "new",
            "wipe_original" => "exported",
            "restore" => "wiped",
            "reopen_sign" => "restored",
            "retire_remote" if state.stage == "retired_unfunded" => "retired_unfunded",
            "retire_remote" => "signed_unfunded",
            _ => return Err("native_action"),
        };
        check(state.stage == expected, "native_stage_order")?;
        if request.action != "create_export" {
            process_absent(&state.creator)?;
        }
        state.pending = Some(request.action.clone());
        save(root, &state)?;
        match request.action.as_str() {
            "create_export" => {
                check(
                    device_secret::load_optional(&c.run_id.to_string())
                        .map_err(|_| "native_keychain_blocked")?
                        .is_none(),
                    "native_original_factor_exists",
                )?;
                let entropy = random(16);
                let phrase =
                    bip39::Mnemonic::from_entropy(&entropy).map_err(|_| "native_entropy")?;
                let words = Zeroizing::new(phrase.words().map(str::to_owned).collect::<Vec<_>>());
                let pin = Zeroizing::new(format!(
                    "{:04}",
                    u32::from_le_bytes(
                        random(4)
                            .as_slice()
                            .try_into()
                            .map_err(|_| "native_entropy")?
                    ) % 10_000
                ));
                let mut next = Zeroizing::new(format!(
                    "{:04}",
                    u32::from_le_bytes(
                        random(4)
                            .as_slice()
                            .try_into()
                            .map_err(|_| "native_entropy")?
                    ) % 10_000
                ));
                if *next == *pin {
                    next = Zeroizing::new(format!(
                        "{:04}",
                        (pin.parse::<u32>().map_err(|_| "native_pin")? + 1) % 10_000
                    ));
                }
                let password = Zeroizing::new(hex::encode(random(32).as_slice()));
                put(&c, "original_pin", pin.as_bytes())?;
                put(&c, "recovery_password", password.as_bytes())?;
                put(&c, "restored_pin", next.as_bytes())?;
                let canary_id = uuid::Uuid::new_v4();
                check(
                    device_secret::load_optional(&canary_id.to_string())
                        .map_err(|_| "native_keychain_blocked")?
                        .is_none(),
                    "native_canary_exists",
                )?;
                let canary = device_secret::get_or_create(root, &canary_id.to_string())
                    .map_err(|_| "native_canary_create")?;
                put(&c, "canary", canary.as_ref())?;
                state.canary_uuid = Some(canary_id);
                state.canary = Some(hash(canary.as_ref()));
                save(root, &state)?;
                let (client, jwt) = auth(&c)?;
                check(
                    client
                        .get_user()
                        .await
                        .map_err(|_| "native_auth_blocked")?
                        .id
                        == c.owner_id,
                    "native_account_changed",
                )?;
                let (identity, kit) = recovery_native::create_export(
                    c.chain,
                    &CoincubeDirectory::new(original.clone()),
                    c.run_id,
                    &pin,
                    &words,
                    &password,
                    &jwt,
                    client,
                )
                .await?;
                check(
                    identity.owner_id == c.owner_id && kit.cube_id == identity.remote_cube_id,
                    "native_remote_binding",
                )?;
                validate_identity(&c, &identity, &Kit::from_api(kit.clone()))?;
                let factor =
                    device_secret::load(&identity.cube.id).map_err(|_| "native_original_factor")?;
                state.original_factor = Some(hash(factor.as_ref()));
                state.original_manifest = Some(manifest(&original)?);
                state.identity = Some(identity);
                state.kit = Some(Kit::from_api(kit));
                put(&c, "provenance", &binding(&state)?)?;
                state.stage = "exported".into();
            }
            "wipe_original" => {
                let identity = state.identity.as_ref().ok_or("native_identity_missing")?;
                // Only this freshly minted unfunded fixture is ever touched.
                check(
                    !restored.exists()
                        && !root.join("quarantined-original").exists()
                        && manifest(&original)?
                            == state
                                .original_manifest
                                .clone()
                                .ok_or("native_inventory_missing")?,
                    "native_original_changed",
                )?;
                let (oracle_client, _) = auth(&c)?;
                state.wipe_oracle = Some(
                    recovery_native::require_unfunded(
                        &CoincubeDirectory::new(original.clone()),
                        identity,
                        oracle_client,
                    )
                    .await?,
                );
                state.wipe_manifest = Some(manifest(&original)?);
                save(root, &state)?;
                let factor =
                    device_secret::load(&identity.cube.id).map_err(|_| "native_original_factor")?;
                check(
                    Some(hash(factor.as_ref())) == state.original_factor,
                    "native_original_factor_changed",
                )?;
                let pw = string(load(&c, "recovery_password")?)?;
                recovery_native::validate_kit(
                    &state.kit.as_ref().ok_or("native_kit_missing")?.api(),
                    &pw,
                    identity,
                )?;
                put(&c, "quarantined_factor", factor.as_ref())?;
                check(
                    load(&c, "quarantined_factor")?.as_slice() == factor.as_slice(),
                    "native_quarantine_readback",
                )?;
                // Exact service/account primitive; no enumeration/personal-item read.
                device_secret::delete(&identity.cube.id)
                    .map_err(|_| "native_factor_delete_uncertain")?;
                check(
                    device_secret::load_optional(&identity.cube.id)
                        .map_err(|_| "native_keychain_blocked")?
                        .is_none(),
                    "native_original_factor_remaining",
                )?;
                fs::rename(&original, root.join("quarantined-original"))
                    .map_err(|_| "native_profile_quarantine_uncertain")?;
                check(
                    !original.exists()
                        && manifest(&root.join("quarantined-original"))?
                            == state
                                .wipe_manifest
                                .clone()
                                .ok_or("native_inventory_missing")?,
                    "native_original_not_absent",
                )?;
                state.stage = "wiped".into();
            }
            "restore" => {
                check(
                    !original.exists() && !restored.exists(),
                    "native_restore_not_fresh",
                )?;
                // This branch never loads original_pin or quarantined_factor.
                let identity = state.identity.as_ref().ok_or("native_identity_missing")?;
                let pw = string(load(&c, "recovery_password")?)?;
                let pin = string(load(&c, "restored_pin")?)?;
                let (client, jwt) = auth(&c)?;
                recovery_native::restore(
                    &CoincubeDirectory::new(restored.clone()),
                    identity,
                    &state.kit.as_ref().ok_or("native_kit_missing")?.api(),
                    &pw,
                    &pin,
                    &jwt,
                    client,
                )
                .await?;
                let factor =
                    device_secret::load(&identity.cube.id).map_err(|_| "native_restored_factor")?;
                check(
                    Some(hash(factor.as_ref())) != state.original_factor,
                    "native_original_factor_reused",
                )?;
                state.restored_factor = Some(hash(factor.as_ref()));
                state.restorer = Some(this_process()?);
                state.stage = "restored".into();
            }
            "reopen_sign" => {
                process_absent(
                    state
                        .restorer
                        .as_ref()
                        .ok_or("native_restore_process_missing")?,
                )?;
                check(!original.exists(), "native_original_reappeared")?;
                let identity = state.identity.as_ref().ok_or("native_identity_missing")?;
                let factor =
                    device_secret::load(&identity.cube.id).map_err(|_| "native_restored_factor")?;
                check(
                    Some(hash(factor.as_ref())) == state.restored_factor
                        && state.restored_factor != state.original_factor,
                    "native_restored_factor_changed",
                )?;
                // C can load only the new PIN; never either recovery/old-PIN/factor handle.
                let pin = string(load(&c, "restored_pin")?)?;
                let wrong = if pin.as_str() == "0000" {
                    "0001"
                } else {
                    "0000"
                };
                let wallet = recovery_native::cold_load(
                    &CoincubeDirectory::new(restored),
                    identity,
                    &pin,
                    wrong,
                )?;
                let psbt = recovery_native::sign_unfunded(&wallet)?;
                let bytes = psbt.serialize();
                write(root, "unfunded-key-control.psbt", &bytes)?;
                state.proof_sha256 = Some(hash(&bytes));
                state.stage = "signed_unfunded".into();
            }
            "retire_remote" => {
                let raw = private(&root.join("unfunded-key-control.psbt"))?;
                check(
                    Some(hash(&raw)) == state.proof_sha256,
                    "native_retirement_export_changed",
                )?;
                check(
                    !original.exists()
                        && root.join("quarantined-original").is_dir()
                        && restored.is_dir(),
                    "native_retirement_recovery_missing",
                )?;
                retire(&c, &mut state, root, &restored).await?;
                state.stage = "retired_unfunded".into();
            }
            _ => return Err("native_action"),
        }
        check(
            Some(hash(&load(&c, "canary")?)) == state.canary,
            "native_canary_changed",
        )?;
        let production_canary = device_secret::load(
            &state
                .canary_uuid
                .ok_or("native_canary_missing")?
                .to_string(),
        )
        .map_err(|_| "native_production_canary_missing")?;
        check(
            Some(hash(production_canary.as_ref())) == state.canary,
            "native_production_canary_changed",
        )?;
        state.pending = None;
        save(root, &state)?;
        Ok(())
    }
    pub fn main() -> Result<()> {
        unsafe {
            libc::umask(0o077);
        }
        let mut input = Zeroizing::new(Vec::new());
        std::io::stdin()
            .take(32769)
            .read_to_end(&mut input)
            .map_err(|_| "native_input")?;
        check(input.len() <= 32768, "native_input_size")?;
        let mode = std::env::args().nth(1);
        if mode.as_deref() == Some("--audit-capacity-abandonment") {
            let request: AbandonInput =
                serde_json::from_slice(&input).map_err(|_| "native_abandon_input")?;
            return tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|_| "native_runtime")?
                .block_on(audit_capacity_abandonment(&request));
        }
        if matches!(
            mode.as_deref(),
            Some("--bind-auth" | "--admit-create" | "--observe-cleanup" | "--begin-cleanup")
        ) {
            let request: AuthInput =
                serde_json::from_slice(&input).map_err(|_| "native_auth_input_schema")?;
            preflight(&request.config)?;
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|_| "native_runtime")?;
            let mut client = CoincubeClient::new();
            client.set_token(&request.access_token);
            let user = runtime
                .block_on(client.get_user())
                .map_err(|_| "native_auth_blocked")?;
            check(user.id == request.config.owner_id, "native_account_changed")?;
            if mode.as_deref() == Some("--begin-cleanup") {
                runtime.block_on(observe_before_create(
                    &request.config,
                    &request.access_token,
                    &client,
                ))?;
                check(
                    !runtime.block_on(recovery_native::create_capacity_available(
                        request.config.chain,
                        request.config.run_id,
                        request.config.owner_id,
                        &client,
                    ))?,
                    "native_capacity_not_full",
                )?;
            } else if mode.as_deref() == Some("--observe-cleanup") {
                runtime.block_on(observe_before_create(
                    &request.config,
                    &request.access_token,
                    &client,
                ))?;
            } else if mode.as_deref() == Some("--admit-create") {
                runtime.block_on(observe_before_create(
                    &request.config,
                    &request.access_token,
                    &client,
                ))?;
                runtime.block_on(recovery_native::admit_create(
                    request.config.chain,
                    request.config.run_id,
                    request.config.owner_id,
                    &client,
                ))?;
            } else {
                put(&request.config, "auth", request.access_token.as_bytes())?;
            }
            return Ok(());
        }
        check(std::env::args().len() == 1, "native_arguments")?;
        let request: Request = serde_json::from_slice(&input).map_err(|_| "native_input_schema")?;
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|_| "native_runtime")?
            .block_on(action(request))
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        fn fixture() -> (Config, Identity, Kit) {
            use coincube_core::miniscript::bitcoin::{
                bip32::DerivationPath, secp256k1::Secp256k1, Network,
            };
            let master = coincube_core::signer::MasterSigner::generate(Network::Testnet4).unwrap();
            let secp = Secp256k1::new();
            let fp = master.fingerprint(&secp).to_string();
            let xpub = master.xpub_at(&DerivationPath::master(), &secp).to_string();
            let run_id = uuid::Uuid::new_v4();
            let mut config = Config {
                version: 1,
                case_root: PathBuf::from("/private/tmp").join(run_id.to_string()),
                run_id,
                chain: ChainId::Testnet4,
                auth_account: String::new(),
                owner_id: 1,
                source_head: "a".repeat(40),
            };
            config.auth_account = account(&config, "auth");
            let cube = coincube_gui::app::settings::CubeSettings::new_with_raw_id(
                run_id.to_string(),
                "Native unit fixture".into(),
                config.chain,
            );
            let identity=Identity{cube,descriptor:format!("wsh(or_d(pk([{fp}]{xpub}/<0;1>/*),and_v(v:pk([{fp}]{xpub}/<2;3>/*),older(144))))"),root_xpub:xpub,fingerprint:fp,remote_cube_id:8,owner_id:1};
            let kit = Kit {
                id: 7,
                cube_id: 8,
                encrypted_seed: "ciphertext-only-unit".into(),
                encrypted_descriptor: "ciphertext-only-unit".into(),
                scheme: "aes-256-gcm".into(),
                created: String::new(),
                updated: String::new(),
            };
            (config, identity, kit)
        }
        #[test]
        fn legitimate_capacity_device_lock_is_held_without_changing_old_files() {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(".device-secret.lock");
            fs::write(directory.path().join("state.json"), b"public-state").unwrap();
            fs::write(directory.path().join("run.lock"), []).unwrap();
            fs::write(&path, []).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let before = fs::symlink_metadata(&path).unwrap();
            let held = capacity_case_entries(directory.path()).unwrap().unwrap();
            let other = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            assert_ne!(
                unsafe { libc::flock(other.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
                0
            );
            assert_eq!(
                fs::read(directory.path().join("state.json")).unwrap(),
                b"public-state"
            );
            let after = fs::symlink_metadata(&path).unwrap();
            assert_eq!(
                (
                    before.dev(),
                    before.ino(),
                    before.len(),
                    before.mtime(),
                    before.mtime_nsec()
                ),
                (
                    after.dev(),
                    after.ino(),
                    after.len(),
                    after.mtime(),
                    after.mtime_nsec()
                )
            );
            drop(held);
            assert_eq!(
                unsafe { libc::flock(other.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
                0
            );
        }

        #[test]
        fn capacity_device_lock_refuses_unsafe_or_unexpected_entries() {
            use std::os::unix::fs::symlink;
            for case in ["nonempty", "symlink", "public", "busy", "unexpected"] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join(".device-secret.lock");
                let mut held = None;
                if case == "symlink" {
                    let target = directory.path().join("target");
                    fs::write(&target, []).unwrap();
                    symlink(target, &path).unwrap();
                } else {
                    fs::write(
                        &path,
                        if case == "nonempty" {
                            b"x".as_slice()
                        } else {
                            b""
                        },
                    )
                    .unwrap();
                    fs::set_permissions(
                        &path,
                        fs::Permissions::from_mode(if case == "public" { 0o644 } else { 0o600 }),
                    )
                    .unwrap();
                    if case == "unexpected" {
                        fs::write(directory.path().join("original"), b"not-a-lock").unwrap();
                    }
                    if case == "busy" {
                        let file = OpenOptions::new()
                            .read(true)
                            .write(true)
                            .open(&path)
                            .unwrap();
                        assert_eq!(
                            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
                            0
                        );
                        held = Some(file);
                    }
                }
                assert!(capacity_case_entries(directory.path()).is_err(), "{}", case);
                drop(held);
            }
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(".device-secret.lock");
            fs::write(&path, []).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let metadata = fs::metadata(path).unwrap();
            assert!(!owned_empty_device_lock(
                &metadata,
                unsafe { libc::geteuid() }.wrapping_add(1)
            ));
        }

        #[test]
        fn legacy_public_case_ids_match_python_uuid5() {
            let root = Path::new("/private/tmp/native-v5-unit");
            assert_eq!(
                legacy_case_id(root, "testnet4").to_string(),
                "67f9b8fa-1570-5b0c-afdb-7648827f6ec1"
            );
            assert_eq!(
                legacy_case_id(root, "bitcoin-blake2b-testnet4").to_string(),
                "e5df97ed-9560-55c8-b8ae-966217d5a93e"
            );
        }

        #[test]
        fn capacity_failure_refuses_material_or_identity_changes_before_any_secret_api() {
            let (mut c, identity, kit) = fixture();
            c.source_head = LEGACY_CAPACITY_SOURCE.into();
            let value = serde_json::json!({"config":c,"stage":"new","pending":"create_export",
                "creator":{"pid":1,"start_id":"unit-dead-process"},"restorer":null,
                "identity":null,"kit":null,"original_manifest":null,"wipe_manifest":null,
                "wipe_oracle":null,"retirement_oracle":null,"original_factor":null,
                "canary_uuid":uuid::Uuid::new_v4(),"canary":"a".repeat(64),
                "restored_factor":null,"proof_sha256":null,"remote_delete_started":false});
            let valid: State = serde_json::from_value(value.clone()).unwrap();
            assert!(validate_capacity_failure(&valid, &c.case_root, c.run_id, c.owner_id).is_ok());
            for field in [
                "uuid", "owner", "chain", "source", "identity", "kit", "profile", "factor",
                "canary", "stage", "pending", "delete",
            ] {
                let mut bad = value.clone();
                match field {
                    "uuid" => bad["config"]["run_id"] = serde_json::json!(uuid::Uuid::new_v4()),
                    "owner" => bad["config"]["owner_id"] = serde_json::json!(99),
                    "chain" => {
                        bad["config"]["chain"] = serde_json::json!(ChainId::BitcoinBlake2bTestnet4)
                    }
                    "source" => bad["config"]["source_head"] = serde_json::json!("b".repeat(40)),
                    "identity" => bad["identity"] = serde_json::to_value(&identity).unwrap(),
                    "kit" => bad["kit"] = serde_json::to_value(&kit).unwrap(),
                    "profile" => bad["original_manifest"] = serde_json::json!("exists"),
                    "factor" => bad["original_factor"] = serde_json::json!("exists"),
                    "canary" => bad["canary"] = serde_json::Value::Null,
                    "stage" => bad["stage"] = serde_json::json!("exported"),
                    "pending" => bad["pending"] = serde_json::Value::Null,
                    _ => bad["remote_delete_started"] = serde_json::json!(true),
                }
                let bad: State = serde_json::from_value(bad).unwrap();
                assert!(
                    validate_capacity_failure(&bad, &c.case_root, c.run_id, c.owner_id).is_err(),
                    "{field}"
                );
            }
        }

        #[test]
        fn changed_identity_is_refused_before_mutation_boundary() {
            let (c, identity, kit) = fixture();
            assert!(validate_identity(&c, &identity, &kit).is_ok());
            for field in [
                "uuid",
                "chain",
                "owner",
                "remote",
                "fingerprint",
                "root",
                "descriptor",
            ] {
                let mut value = identity.clone();
                match field {
                    "uuid" => value.cube.id = uuid::Uuid::new_v4().to_string(),
                    "chain" => value.cube.network = ChainId::BitcoinBlake2bTestnet4,
                    "owner" => value.owner_id = 99,
                    "remote" => value.remote_cube_id = 99,
                    "fingerprint" => value.fingerprint = "00000000".into(),
                    "root" => value.root_xpub = "invalid".into(),
                    _ => value.descriptor = "invalid".into(),
                }
                let mut mutations = 0;
                if validate_identity(&c, &value, &kit).is_ok() {
                    mutations += 1;
                }
                assert_eq!(mutations, 0);
            }
            let mut wrong = kit.clone();
            wrong.cube_id = 9;
            assert!(validate_identity(&c, &identity, &wrong).is_err());
        }
        #[test]
        fn restore_request_has_no_original_secret_or_profile_input() {
            let (c, _, _) = fixture();
            for field in [
                "mnemonic",
                "original_pin",
                "original_profile",
                "device_factor",
                "quarantine_locator",
                "binary",
                "receipt",
            ] {
                let mut value = serde_json::json!({"action":"restore","config":c});
                value[field] = serde_json::json!("forbidden");
                assert!(serde_json::from_value::<Request>(value).is_err());
            }
        }
        fn inventory(identity: &Identity) -> serde_json::Value {
            serde_json::json!({"success":true,"data":{"id":identity.remote_cube_id,"uuid":identity.cube.id,"userId":identity.owner_id,"network":identity.cube.network.api_str(),"name":identity.cube.name,"status":"active","hasRecoveryKit":true,"members":[],"pendingInvites":[],"vault":null,"lightningAddress":null,"sparkStableBalance":null,"encryptionPubkey":null}})
        }
        #[test]
        fn retirement_refuses_unknown_failed_shared_or_referenced_inventory() {
            let (_, identity, _) = fixture();
            let cube = inventory(&identity);
            let keys = serde_json::json!({"success":true,"data":[]});
            let approvals = serde_json::json!({"approvals":[]});
            assert!(no_references(&cube, &keys, &approvals, &identity).is_ok());
            for field in [
                "members",
                "pendingInvites",
                "vault",
                "lightningAddress",
                "sparkStableBalance",
                "encryptionPubkey",
                "userId",
                "uuid",
                "network",
                "hasRecoveryKit",
            ] {
                let mut bad = cube.clone();
                bad["data"].as_object_mut().unwrap().remove(field);
                assert!(no_references(&bad, &keys, &approvals, &identity).is_err());
            }
            for field in ["members", "pendingInvites", "keys"] {
                let mut bad = cube.clone();
                bad["data"][field] = serde_json::json!([{"id":99}]);
                assert!(no_references(&bad, &keys, &approvals, &identity).is_err());
            }
            for success in [
                serde_json::Value::Null,
                serde_json::json!(false),
                serde_json::json!("true"),
            ] {
                let mut bad = cube.clone();
                bad["success"] = success.clone();
                assert!(no_references(&bad, &keys, &approvals, &identity).is_err());
                let mut bad_keys = keys.clone();
                bad_keys["success"] = success;
                assert!(no_references(&cube, &bad_keys, &approvals, &identity).is_err());
            }
            assert!(no_references(&cube, &keys, &serde_json::json!({}), &identity).is_err());
            assert!(no_references(
                &cube,
                &keys,
                &serde_json::json!({"approvals":[{"id":1}]}),
                &identity
            )
            .is_err());
        }
        #[test]
        fn absence_requires_durable_delete_intent_not_only_pending_action() {
            assert!(!may_reconcile_absence(Some("retire_remote"), false));
            assert!(may_reconcile_absence(Some("retire_remote"), true));
            assert!(!may_reconcile_absence(None, true));
            assert!(!may_reconcile_absence(Some("restore"), true));
        }
        #[test]
        fn private_reader_refuses_symlinks_and_public_files() {
            let root = tempfile::tempdir().unwrap();
            let file = root.path().join("fixture");
            std::fs::write(&file, b"public test metadata").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(private(&file).is_ok());
            let link = root.path().join("alias");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert!(private(&link).is_err());
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(private(&file).is_err());
        }
    }
}
#[cfg(all(feature = "integration-tests", target_os = "macos"))]
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(native::main).unwrap_or(Err("native_panic"));
    match result{Ok(())=>println!("{{\"ok\":true,\"provenance\":\"native_product_integration\",\"funded\":false,\"broadcast\":false}}"),Err(code)=>{println!("{{\"ok\":false,\"code\":\"{}\"}}",code);std::process::exit(2);}}
}
