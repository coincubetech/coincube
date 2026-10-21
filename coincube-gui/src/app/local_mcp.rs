//! Trusted Cube settings and read-only projections. Never serializes wallet objects.
use super::{view, wallet::Wallet};
use crate::daemon::Daemon;
#[cfg(unix)]
use coincube_mcp as wire;
use coincube_ui::{
    component::text::{h3, text},
    widget::Element,
};
use iced::widget::{button, checkbox, column, text_input};
use std::sync::Arc;
#[cfg(unix)]
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum Message {
    Enable(bool),
    Wallet(bool),
    Capability(usize, bool),
    ClientName(String),
    Approve,
    Revoke,
    CopyConfig,
}
#[derive(Default)]
pub struct Settings {
    enabled: bool,
    wallet_allowed: bool,
    capabilities: [bool; 3],
    client_name: String,
    approved_name: String,
    failed: bool,
    #[cfg(unix)]
    server: Option<wire::ipc::Server>,
}
impl Settings {
    pub fn revoke(&mut self) {
        #[cfg(unix)]
        {
            self.server.take();
        }
        self.approved_name.clear();
    }
    pub fn update(
        &mut self,
        message: Message,
        wallet: Option<Arc<Wallet>>,
        daemon: Option<Arc<dyn Daemon + Send + Sync>>,
        supported: bool,
        cube_id: &str,
    ) -> Option<String> {
        match message {
            Message::Enable(value) => {
                self.revoke();
                self.enabled = value && cfg!(unix);
            }
            Message::Wallet(value) => {
                self.revoke();
                self.wallet_allowed = value;
            }
            Message::Capability(index, value) if index < 3 => {
                self.revoke();
                self.capabilities[index] = value;
            }
            Message::ClientName(value) => {
                self.revoke();
                self.client_name = value.chars().filter(|c| !c.is_control()).take(64).collect();
            }
            Message::Revoke => self.revoke(),
            Message::Approve => {
                self.revoke();
                self.failed = false;
                if !supported
                    || !self.enabled
                    || !self.wallet_allowed
                    || self.client_name.trim().is_empty()
                    || !self.capabilities.iter().any(|v| *v)
                {
                    return None;
                }
                #[cfg(unix)]
                if let (Some(wallet), Some(daemon)) = (wallet, daemon) {
                    if wallet.main_descriptor.as_vault().is_none()
                        || !daemon.backend().is_embedded()
                    {
                        return None;
                    }
                    let generation = super::session::generation_for(cube_id)?;
                    let cube_id = cube_id.to_owned();
                    let wallet_id = uuid::Uuid::new_v4().to_string();
                    let grant = wire::ipc::Grant::new(
                        wallet_id.clone(),
                        self.capabilities,
                        Duration::from_secs(3600),
                    )
                    .with_session_guard(move || {
                        super::session::generation_for(&cube_id) == Some(generation)
                    });
                    let reader = Arc::new(VaultReader {
                        read_slots: Arc::new(tokio::sync::Semaphore::new(2)),
                        descriptor: wallet.main_descriptor.as_vault()?.clone(),
                        chain: wallet.chain,
                        daemon,
                        wallet_id,
                    });
                    match wire::ipc::Server::start(reader, grant) {
                        Ok(server) => {
                            self.server = Some(server);
                            self.approved_name = self.client_name.clone();
                        }
                        Err(_) => self.failed = true,
                    }
                }
                #[cfg(not(unix))]
                let _ = (wallet, daemon, cube_id);
            }
            Message::CopyConfig => {
                #[cfg(unix)]
                if let Some(server) = self.server.as_ref().filter(|s| s.grant.active()) {
                    let executable = std::env::current_exe().ok()?.parent()?.join("tenshu-mcp");
                    return serde_json::to_string_pretty(&serde_json::json!({"mcpServers": {"tenshu": {
                        "command": executable,
                        "env": {"TENSHU_MCP_SOCKET": server.socket, "TENSHU_MCP_CREDENTIAL": server.grant.credential()}
                    }}})).ok();
                }
            }
            _ => {}
        }
        None
    }
    pub fn view(&self, supported: bool) -> Element<'_, view::Message> {
        let message = |m| view::Message::LocalMcp(m);
        let mut content = column![
            h3("AI access"),
            text("Connect a local AI client to this Cube's Vault. Your AI provider may receive financial information. Only enable access for a client you trust."),
            text("Read-only. No keys, signing, payments or wallet changes. Grants expire after one hour and end when the Cube locks or closes."),
        ].spacing(16);
        if !cfg!(unix) || !supported {
            return content.push(text("MCP currently supports Bitcoin Vaults on macOS and Linux. This wallet is unavailable for MCP.")).into();
        }
        content = content.push(
            checkbox(self.enabled)
                .label("Enable MCP for this unlocked session")
                .on_toggle(move |v| message(Message::Enable(v))),
        );
        if self.enabled {
            content = content
                .push(
                    text_input("Client label (for your reference)", &self.client_name)
                        .on_input(move |v| message(Message::ClientName(v))),
                )
                .push(
                    checkbox(self.wallet_allowed)
                        .label("Allow this client to access this Cube's Vault")
                        .on_toggle(move |v| message(Message::Wallet(v))),
                );
            for (i, label) in [
                "Read balances",
                "Read confirmed transaction history",
                "Read spending policy summary",
            ]
            .iter()
            .copied()
            .enumerate()
            {
                content = content.push(
                    checkbox(self.capabilities[i])
                        .label(label)
                        .on_toggle(move |v| message(Message::Capability(i, v))),
                );
            }
            content = content.push(
                button("Approve client for one hour").on_press_maybe(
                    (self.wallet_allowed
                        && self.capabilities.iter().any(|v| *v)
                        && !self.client_name.trim().is_empty())
                    .then(|| message(Message::Approve)),
                ),
            );
            #[cfg(unix)]
            if let Some(server) = &self.server {
                let calls = server
                    .grant
                    .successful_calls
                    .load(std::sync::atomic::Ordering::Relaxed);
                content = content
                    .push(text(format!(
                        "Client: {} · {} · successful reads: {}",
                        self.approved_name,
                        if server.grant.active() {
                            "Authorized"
                        } else {
                            "Inactive — approve again"
                        },
                        calls
                    )))
                    .push(button("Revoke access now").on_press(message(Message::Revoke)));
                if server.grant.active() {
                    content = content.push(button("Copy private client configuration").on_press(message(Message::CopyConfig)))
                        .push(text("Paste into your MCP client's local configuration, then clear the clipboard. It contains a temporary access credential: do not paste it into a chat. Restart the client after replacing a grant. See docs/mcp/README.md."));
                }
            } else {
                content = content.push(text("No client is authorized."));
            }
        }
        if self.failed {
            content = content.push(text("Could not start local MCP. No access was granted."));
        }
        content.into()
    }
}

#[cfg(unix)]
#[derive(Clone)]
struct VaultReader {
    read_slots: Arc<tokio::sync::Semaphore>,
    descriptor: coincube_core::descriptors::CoincubeDescriptor,
    chain: crate::chain::ChainId,
    daemon: Arc<dyn Daemon + Send + Sync>,
    wallet_id: String,
}
#[cfg(unix)]
impl wire::Reader for VaultReader {
    fn read(
        &self,
        request: wire::Request,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<wire::Response, wire::Error>> + Send + '_>,
    > {
        Box::pin(async move {
            // Existing daemon reads may block on SQLite. Keep them off the async
            // executor so deadlines and revocation remain responsive. The permit
            // belongs to the blocking worker: timing out must not admit unlimited
            // replacement reads while old database calls are still running.
            let permit = self
                .read_slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| wire::Error::Unavailable)?;
            let reader = self.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                tokio::runtime::Handle::current().block_on(reader.read_wallet(request))
            })
            .await
            .map_err(|_| wire::Error::Unavailable)?
        })
    }
}
#[cfg(unix)]
impl VaultReader {
    async fn read_wallet(&self, request: wire::Request) -> Result<wire::Response, wire::Error> {
        // Defense in depth: the IPC gate enforces the same opaque identity.
        if request.scope().is_some_and(|(id, _)| id != self.wallet_id) {
            return Err(wire::Error::AccessDenied);
        }
        if !request.valid() {
            return Err(wire::Error::InvalidInput);
        }
        let descriptor = &self.descriptor;
        let info = self
            .daemon
            .get_info()
            .await
            .map_err(|_| wire::Error::Unavailable)?;
        if info.descriptors.main.as_vault() != Some(descriptor) || info.chain_id != Some(self.chain)
        {
            return Err(wire::Error::Unavailable);
        }
        match request {
            wire::Request::List(_) => Ok(wire::Response::List(wire::WalletList {
                wallets: vec![wire::WalletMetadata {
                    wallet_id: self.wallet_id.clone(),
                    display_name: "Vault".into(),
                    wallet_type: "vault".into(),
                }],
            })),
            wire::Request::Summary(_) => {
                let coins = self
                    .daemon
                    .list_coins(&[], &[])
                    .await
                    .map_err(|_| wire::Error::Unavailable)?
                    .coins;
                let sync = super::wallet::sync_status(
                    self.daemon.backend(),
                    info.block_height,
                    info.sync,
                    info.last_poll_timestamp,
                    None,
                    &info.history_sync,
                );
                let sync_state = match sync {
                    super::wallet::SyncStatus::Synced => "synced",
                    super::wallet::SyncStatus::SyncFailing { .. } => "sync_failing",
                    _ => "syncing",
                }
                .to_owned();
                let unspent = || coins.iter().filter(|c| c.spend_info.is_none());
                Ok(wire::Response::Summary(wire::Summary {
                    wallet_id: self.wallet_id.clone(),
                    network: info.network.to_string(),
                    wallet_type: "vault".into(),
                    confirmed_sats: unspent()
                        .filter(|c| c.block_height.is_some())
                        .map(|c| c.amount.to_sat())
                        .sum(),
                    unconfirmed_sats: unspent()
                        .filter(|c| c.block_height.is_none())
                        .map(|c| c.amount.to_sat())
                        .sum(),
                    available_sats: unspent()
                        .filter(|c| !c.is_immature && crate::daemon::model::coin_is_owned(c))
                        .map(|c| c.amount.to_sat())
                        .sum(),
                    sync_state,
                }))
            }
            wire::Request::History(input) => {
                let transactions = self
                    .daemon
                    .list_history_txs(0, u32::MAX, u64::from(input.limit))
                    .await
                    .map_err(|_| wire::Error::Unavailable)?;
                Ok(wire::Response::History(wire::History {
                    wallet_id: self.wallet_id.clone(),
                    confirmed_only: true,
                    transactions: transactions
                        .into_iter()
                        .take(usize::from(input.limit))
                        .map(|tx| wire::Transaction {
                            txid: tx.txid.to_string(),
                            incoming_sats: tx.incoming_amount.to_sat(),
                            outgoing_sats: tx.outgoing_amount.to_sat(),
                            timestamp: tx.time,
                            height: tx.height,
                        })
                        .collect(),
                }))
            }
            wire::Request::Policy(_) => {
                let policy = descriptor.policy();
                let path = |p: &coincube_core::descriptors::PathInfo, delay| {
                    let (threshold, key_count) = match p {
                        coincube_core::descriptors::PathInfo::Single(_) => (1, 1),
                        coincube_core::descriptors::PathInfo::Multi(k, keys) => (*k, keys.len()),
                    };
                    wire::PolicyPath {
                        threshold,
                        key_count,
                        relative_delay_blocks: delay,
                    }
                };
                Ok(wire::Response::Policy(wire::Policy {
                    wallet_id: self.wallet_id.clone(),
                    deterministic: true,
                    script_type: if descriptor.is_taproot() {
                        "p2tr"
                    } else {
                        "p2wsh"
                    }
                    .into(),
                    primary: path(policy.primary_path(), 0),
                    recovery: policy
                        .recovery_paths()
                        .iter()
                        .map(|(delay, p)| path(p, *delay))
                        .collect(),
                }))
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use coincube_core::{
        descriptors::{CoincubeDescriptor, CoincubePolicy, PathInfo},
        miniscript::descriptor::DescriptorPublicKey,
    };
    use std::str::FromStr;

    fn fixture() -> (
        std::path::PathBuf,
        Arc<Wallet>,
        Arc<dyn Daemon + Send + Sync>,
    ) {
        // Existing public-only test keys. No seed, xprv, production wallet or signing.
        let owner = DescriptorPublicKey::from_str("[aabbccdd]xpub68JJTXc1MWK8KLW4HGLXZBJknja7kDUJuFHnM424LbziEXsfkh1WQCiEjjHw4zLqSUm4rvhgyGkkuRowE9tCJSgt3TQB5J3SKAbZ2SdcKST/<0;1>/*").unwrap();
        let recovery = DescriptorPublicKey::from_str("[aabbccdd]xpub68JJTXc1MWK8PEQozKsRatrUHXKFNkD1Cb1BuQU9Xr5moCv87anqGyXLyUd4KpnDyZgo3gz4aN1r3NiaoweFW8UutBsBbgKHzaD5HkTkifK/<0;1>/*").unwrap();
        let descriptor = CoincubeDescriptor::new(
            CoincubePolicy::new_legacy(
                PathInfo::Single(owner),
                [(144, PathInfo::Single(recovery))]
                    .iter()
                    .cloned()
                    .collect(),
            )
            .unwrap(),
        );
        let root = std::env::temp_dir().join(format!("mcp-wallet-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let (config, handle) = coincubed::mcp_test_fixture::wallet(&root, descriptor.clone());
        let daemon =
            Arc::new(crate::daemon::embedded::EmbeddedDaemon::from_regtest_handle(config, handle));
        (root, Arc::new(Wallet::new(descriptor)), daemon)
    }
    fn input(id: &str) -> wire::WalletInput {
        wire::WalletInput {
            wallet_id: id.into(),
        }
    }

    // Runs separately after building the installation's companion executable.
    // Unlike the isolated protocol/IPC tests, this crosses both process and
    // wallet-service boundaries, using the configuration issued by Settings.
    #[test]
    #[ignore = "build tenshu-mcp and set TENSHU_MCP_TEST_ADAPTER; run explicitly in CI"]
    fn stdio_client_reads_real_wallet_and_observes_revocation() {
        use serde_json::{json, Value};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let _session_guard = super::super::session::test_guard();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            super::super::session::open_without_pin("mcp-fixture");
            let (root, wallet, daemon) = fixture();
            let mut settings = Settings::default();
            for message in [
                Message::Enable(true), Message::ClientName("stdio fixture".into()),
                Message::Wallet(true), Message::Capability(0, true),
                Message::Capability(1, true), Message::Capability(2, true), Message::Approve,
            ] {
                settings.update(message, Some(wallet.clone()), Some(daemon.clone()), true, "mcp-fixture");
            }
            let config: Value = serde_json::from_str(&settings.update(
                Message::CopyConfig, None, None, true, "mcp-fixture"
            ).unwrap()).unwrap();
            let environment = config["mcpServers"]["tenshu"]["env"].as_object().unwrap();
            let executable = std::env::var_os("TENSHU_MCP_TEST_ADAPTER")
                .expect("set TENSHU_MCP_TEST_ADAPTER to the built adapter");
            let mut child = tokio::process::Command::new(executable)
                .envs(environment.iter().map(|(k,v)| (k, v.as_str().unwrap())))
                .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
            async fn response(
                stdin: &mut tokio::process::ChildStdin,
                stdout: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
                request: Value,
            ) -> Value {
                let mut bytes = serde_json::to_vec(&request).unwrap();
                bytes.push(b'\n');
                stdin.write_all(&bytes).await.unwrap();
                let line = tokio::time::timeout(Duration::from_secs(10), stdout.next_line())
                    .await.unwrap().unwrap().unwrap();
                let value: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(value["id"], request["id"]);
                assert!(value.get("error").is_none(), "protocol error");
                value["result"].clone()
            }
            let initialized = response(&mut stdin, &mut stdout, json!({
                "jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
                    "protocolVersion":"2024-11-05", "capabilities":{},
                    "clientInfo":{"name":"wallet-acceptance", "version":"1"}
                }
            })).await;
            assert_eq!(initialized["serverInfo"]["name"], "tenshu-mcp");
            stdin.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n").await.unwrap();
            let discovered = response(&mut stdin, &mut stdout, json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"})).await;
            assert_eq!(discovered["tools"].as_array().unwrap().len(), 4);
            let call = |id, name: &str, arguments: Value| json!({"jsonrpc":"2.0", "id":id, "method":"tools/call", "params":{"name":name,"arguments":arguments}});
            let listed = response(&mut stdin, &mut stdout, call(3, "tenshu_list_wallets", json!({}))).await;
            let id = listed["structuredContent"]["wallets"][0]["wallet_id"].as_str().unwrap();
            let summary = response(&mut stdin, &mut stdout, call(4, "tenshu_get_wallet_summary", json!({"wallet_id":id}))).await;
            assert_eq!(summary["structuredContent"]["confirmed_sats"], 55_000);
            let history = response(&mut stdin, &mut stdout, call(5, "tenshu_get_transactions", json!({"wallet_id":id,"limit":10}))).await;
            assert_eq!(history["structuredContent"]["transactions"][0]["incoming_sats"], 55_000);
            let policy = response(&mut stdin, &mut stdout, call(6, "tenshu_get_wallet_policy_summary", json!({"wallet_id":id}))).await;
            assert_eq!(policy["structuredContent"]["recovery"][0]["relative_delay_blocks"], 144);
            let denied = response(&mut stdin, &mut stdout, call(7, "tenshu_get_wallet_summary", json!({"wallet_id":"another-wallet"}))).await;
            assert_eq!(denied["isError"], true);
            assert_eq!(denied["structuredContent"]["error"]["code"], "access_denied");
            super::super::session::close();
            let locked = response(&mut stdin, &mut stdout, call(8, "tenshu_get_wallet_summary", json!({"wallet_id":id}))).await;
            assert_eq!(locked["structuredContent"]["error"]["code"], "access_denied");
            settings.update(Message::Revoke, None, None, true, "mcp-fixture");
            let revoked = response(&mut stdin, &mut stdout, call(9, "tenshu_list_wallets", json!({}))).await;
            assert_eq!(revoked["isError"], true);
            assert!(revoked["structuredContent"].get("wallets").is_none());
            drop(stdin);
            let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await.unwrap().unwrap();
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
            drop(settings);
            daemon.stop().await.unwrap();
            drop(daemon);
            drop(wallet);
            std::fs::remove_dir_all(root).unwrap();
        });
    }

    #[test]
    fn real_sqlite_wallet_reads_require_ui_grant_and_revoke_on_settings_drop() {
        let _session_guard = super::super::session::test_guard();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            super::super::session::open_without_pin("mcp-fixture");
            let (root, wallet, daemon) = fixture();
            let mut settings = Settings::default();
            assert!(!settings.enabled && settings.server.is_none());
            settings.update(
                Message::Approve,
                Some(wallet.clone()),
                Some(daemon.clone()),
                true,
                "mcp-fixture",
            );
            assert!(settings.server.is_none());
            for message in [
                Message::Enable(true),
                Message::ClientName("fixture client".into()),
                Message::Wallet(true),
                Message::Capability(0, true),
                Message::Capability(1, true),
                Message::Capability(2, true),
                Message::Approve,
            ] {
                settings.update(
                    message,
                    Some(wallet.clone()),
                    Some(daemon.clone()),
                    true,
                    "mcp-fixture",
                );
            }
            let server = settings.server.as_ref().unwrap();
            let socket = server.socket.clone();
            let credential = server.grant.credential().to_owned();
            let response =
                wire::ipc::call(&socket, &credential, wire::Request::List(wire::Empty {}))
                    .await
                    .unwrap();
            let wire::Response::List(list) = response else {
                panic!("expected list")
            };
            assert_eq!(list.wallets.len(), 1);
            let id = &list.wallets[0].wallet_id;
            let summary = wire::ipc::call(&socket, &credential, wire::Request::Summary(input(id)))
                .await
                .unwrap();
            let wire::Response::Summary(summary) = summary else {
                panic!("expected summary")
            };
            assert_eq!(
                (
                    summary.confirmed_sats,
                    summary.available_sats,
                    summary.unconfirmed_sats
                ),
                (55_000, 55_000, 0)
            );
            let history = wire::ipc::call(
                &socket,
                &credential,
                wire::Request::History(wire::HistoryInput {
                    wallet_id: id.clone(),
                    limit: 10,
                }),
            )
            .await
            .unwrap();
            let wire::Response::History(history) = history else {
                panic!("expected history")
            };
            assert_eq!(history.transactions.len(), 1);
            assert_eq!(history.transactions[0].incoming_sats, 55_000);
            let policy = wire::ipc::call(&socket, &credential, wire::Request::Policy(input(id)))
                .await
                .unwrap();
            let wire::Response::Policy(policy) = policy else {
                panic!("expected policy")
            };
            assert_eq!((policy.primary.threshold, policy.primary.key_count), (1, 1));
            assert_eq!(policy.recovery[0].relative_delay_blocks, 144);
            let serialized = serde_json::to_string(&(summary, history, policy)).unwrap();
            for private in [
                "xpub",
                "descriptor",
                "script_pubkey",
                "labels",
                "seed",
                "private_key",
                "credential",
            ] {
                assert!(!serialized.contains(private));
            }
            assert_eq!(
                wire::ipc::call(
                    &socket,
                    &credential,
                    wire::Request::Summary(input("other-wallet"))
                )
                .await
                .unwrap_err(),
                wire::Error::AccessDenied
            );
            super::super::session::close();
            assert_eq!(
                wire::ipc::call(&socket, &credential, wire::Request::Summary(input(id)))
                    .await
                    .unwrap_err(),
                wire::Error::AccessDenied
            );
            super::super::session::open_without_pin("mcp-fixture");
            assert_eq!(
                wire::ipc::call(&socket, &credential, wire::Request::Summary(input(id)))
                    .await
                    .unwrap_err(),
                wire::Error::AccessDenied
            );
            // App owns Settings; locking/dropping the App drops this exact guard.
            drop(settings);
            assert!(!socket.exists());
            assert!(
                wire::ipc::call(&socket, &credential, wire::Request::Summary(input(id)))
                    .await
                    .is_err()
            );
            daemon.stop().await.unwrap();
            drop(daemon);
            drop(wallet);
            super::super::session::close();
            std::fs::remove_dir_all(root).unwrap();
        });
    }

    #[test]
    fn disable_and_capability_edit_invalidate_old_configuration() {
        let _session_guard = super::super::session::test_guard();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            super::super::session::open_without_pin("mcp-fixture");
            let (root, wallet, daemon) = fixture();
            let mut settings = Settings::default();
            for message in [
                Message::Enable(true),
                Message::ClientName("client".into()),
                Message::Wallet(true),
                Message::Capability(0, true),
                Message::Approve,
            ] {
                settings.update(
                    message,
                    Some(wallet.clone()),
                    Some(daemon.clone()),
                    true,
                    "mcp-fixture",
                );
            }
            let permission = settings.server.as_ref().unwrap().grant.clone();
            settings.update(
                Message::Capability(1, true),
                None,
                None,
                true,
                "mcp-fixture",
            );
            assert!(!permission.active());
            assert!(settings.server.is_none());
            settings.update(
                Message::Approve,
                Some(wallet.clone()),
                Some(daemon.clone()),
                true,
                "mcp-fixture",
            );
            let permission = settings.server.as_ref().unwrap().grant.clone();
            settings.update(Message::Enable(false), None, None, true, "mcp-fixture");
            assert!(!permission.active());
            assert!(!settings.enabled);
            settings.update(
                Message::Approve,
                Some(wallet.clone()),
                Some(daemon.clone()),
                true,
                "mcp-fixture",
            );
            assert!(settings.server.is_none());
            daemon.stop().await.unwrap();
            drop(daemon);
            drop(wallet);
            drop(settings);
            super::super::session::close();
            std::fs::remove_dir_all(root).unwrap();
        });
    }

    #[test]
    fn app_preserves_grant_on_connect_refresh_but_revokes_on_backend_switch() {
        use super::super::{message::Message as AppMessage, App};
        let _session_guard = super::super::session::test_guard();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _enter = runtime.enter();
        let (root, wallet, daemon) = fixture();
        let (mut app, startup) = App::new_inner(
            super::super::Cache::default(),
            wallet,
            None,
            None,
            super::super::Config::new(false),
            daemon.clone(),
            super::super::CoincubeDirectory::new(root.clone()),
            None,
            super::super::settings::CubeSettings::new(
                "MCP fixture".into(),
                crate::chain::ChainId::Bitcoin,
            ),
            None,
        );
        drop(startup);
        super::super::session::open_without_pin(app.cube_settings.id.clone());
        for message in [
            Message::Enable(true),
            Message::Wallet(true),
            Message::ClientName("client".into()),
            Message::Capability(0, true),
            Message::Approve,
        ] {
            drop(app.update(AppMessage::View(view::Message::LocalMcp(message))));
        }
        let grant = app.local_mcp.server.as_ref().unwrap().grant.clone();
        let generation = app.panels.connect.account.session_generation();
        drop(app.update(AppMessage::View(view::Message::ConnectAccount(
            view::ConnectAccountMessage::FeaturesLoaded(None, generation),
        ))));
        assert!(
            grant.active(),
            "routine Connect refresh must preserve the local grant"
        );
        app.revoke_claim();
        drop(app.update(AppMessage::SettingsSaved));
        assert!(
            grant.active(),
            "unrelated claim/settings changes must preserve access"
        );
        let config = daemon.config().unwrap().clone();
        drop(app.spawn_daemon_switch(config));
        assert!(
            !grant.active(),
            "backend switch must revoke before dispatch"
        );
        app.stop();
        drop(app);
        drop(daemon);
        super::super::session::close();
        std::fs::remove_dir_all(root).unwrap();
    }
}
