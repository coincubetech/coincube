//! Synthetic end-to-end installer/PIN/embedded reopen boundary. No OS secrets.
use super::*;
use crate::chain::ChainId;
use crate::services::coincube::CoincubeClient;
use httpmock::prelude::*;
use serde_json::json;

const DESCRIPTOR: &str = concat!(
    "wsh(andor(pk([aabbccdd]xpub68JJTXc1MWK8KLW4HGLXZBJknja7kDUJuFHnM424LbziEXsfkh1WQCiEjjHw4z",
    "LqSUm4rvhgyGkkuRowE9tCJSgt3TQB5J3SKAbZ2SdcKST/<0;1>/*),older(10000),pk([aabbccdd]xpub68JJT",
    "Xc1MWK8PEQozKsRatrUHXKFNkD1Cb1BuQU9Xr5moCv87anqGyXLyUd4KpnDyZgo3gz4aN1r3NiaoweFW8Uut",
    "BsBbgKHzaD5HkTkifK/<0;1>/*)))#3xh8xmhn"
);

fn client(server: &MockServer) -> CoincubeClient {
    let mut client = CoincubeClient::new();
    client.base_url = server.base_url();
    client.set_token("synthetic-activation-token");
    client
}

async fn output<T: Send + 'static>(task: Task<T>) -> Vec<T> {
    use iced_runtime::futures::futures::StreamExt;
    match iced_runtime::task::into_stream(task) {
        Some(stream) => {
            stream
                .filter_map(|action| async move {
                    match action {
                        iced_runtime::Action::Output(value) => Some(value),
                        _ => None,
                    }
                })
                .collect()
                .await
        }
        None => Vec::new(),
    }
}

#[tokio::test]
async fn actual_fork_install_pin_unlock_and_authenticated_reopen_are_chain_bound() {
    let server = MockServer::start_async().await;
    let features = server
        .mock_async(|when, then| {
            when.method(GET)
                .path("/api/v1/connect/features")
                .header("authorization", "Bearer synthetic-activation-token");
            then.status(200).json_body(
                json!({"success":true,"data":{"plans":[],"bitcoinBlake2bEnabled":true}}),
            );
        })
        .await;
    server.mock_async(|when, then| {
        when.method(GET).path("/api/v1/connect/networks/bitcoin-blake2b/anchor")
            .header("authorization", "Bearer synthetic-activation-token");
        then.status(200).json_body(json!({"success":true,"data":{"network":"bitcoin-blake2b","state":"available","anchor":{
            "tip_hash":"11".repeat(32),"tip_height":973029,"tip_median_time_past":1800000000,
            "observed_at":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
            "observation":{"tip_height":973029,"fork":{"height":972000,"active":true},
                "rdts":{"state":"flagday","flagday":{"height":972000,"expiry_time":1800010000_i64,"active":false}}}
        }}}));
    }).await;
    let prefix = "/api/v1/esplora/bitcoin-blake2b/mainnet";
    for (path, body) in [
        (
            format!("{prefix}/block-height/0"),
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f".into(),
        ),
        (format!("{prefix}/block-height/973029"), "11".repeat(32)),
        (format!("{prefix}/blocks/tip/hash"), "11".repeat(32)),
        (
            format!("{prefix}/block/{}/status", "11".repeat(32)),
            "{\"in_best_chain\":true,\"height\":973029,\"next_best\":null}".into(),
        ),
    ] {
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path(path)
                    .header("authorization", "Bearer synthetic-activation-token");
                then.status(200).body(body);
            })
            .await;
    }
    let forbidden_bitcoin = server
        .mock_async(|when, then| {
            when.path_contains("/esplora/bitcoin/");
            then.status(500);
        })
        .await;
    let root_path =
        std::env::temp_dir().join(format!("btcb2-create-reopen-{}", uuid::Uuid::new_v4()));
    let root = CoincubeDirectory::new(root_path.clone());
    let client = client(&server);
    let (mut installer, _) = Installer::try_new_for_chain(
        root.clone(),
        ChainId::BitcoinBlake2b,
        None,
        UserFlow::CreateWallet,
        false,
        None,
        None,
        None,
        false,
        Some(client.clone()),
    )
    .unwrap();
    assert!(installer.context.fresh_fork_cube);
    installer.context.restore_pin = Some(zeroize::Zeroizing::new("2468".to_string()));
    installer.context.fresh_fork_seed_backed_up = true;
    installer.context.descriptor = Some(DESCRIPTOR.parse().unwrap());
    installer.context.bitcoin_backend =
        Some(BitcoinBackend::Esplora(coincubed::config::EsploraConfig {
            addr: format!("{}{prefix}", server.base_url()),
            token: None,
            fallback_addr: None,
            fallback_token: None,
            secondary_fallback_addr: None,
            secondary_fallback_token: None,
        }));
    let cube_id = installer.context.seed_cube_id().to_owned();
    let fingerprint = installer.master_signer_fingerprint();
    let wallet_id = WalletId::generate(installer.context.descriptor.as_ref().unwrap());
    assert!(!root_path.exists());
    let settings = install_local_wallet(
        installer.context.clone(),
        wallet_id,
        installer.signer.clone(),
    )
    .await
    .unwrap();
    assert!(!root_path.join("bitcoin").exists());
    let cfg_path = root
        .network_directory(ChainId::BitcoinBlake2b)
        .coincubed_data_directory(&settings.wallet_id())
        .path()
        .join("daemon.toml");
    assert!(!std::fs::read_to_string(cfg_path)
        .unwrap()
        .contains("synthetic-activation-token"));

    let mut cube = crate::app::settings::CubeSettings::new_with_raw_id(
        cube_id,
        "Synthetic".into(),
        ChainId::BitcoinBlake2b,
    );
    cube.master_signer_fingerprint = Some(fingerprint);
    cube.backed_up = true;
    let mut entry = crate::pin_entry::PinEntry::new(
        cube,
        root_path.clone(),
        crate::pin_entry::PinEntrySuccess::LoadApp {
            datadir: root.clone(),
            config: crate::app::Config::new(false),
            network: bitcoin::Network::Bitcoin,
            internal_bitcoind: None,
            backup: None,
            wallet_settings: Some(settings.clone()),
            connect_client: Some(client.clone()),
        },
        None,
    );
    for (i, digit) in "2468".chars().enumerate() {
        let _ = entry.update(crate::pin_entry::Message::PinInput(
            crate::pin_input::Message::DigitChanged(i, digit.to_string()),
        ));
    }
    let messages = output(entry.update(crate::pin_entry::Message::Submit)).await;
    assert!(matches!(
        messages.as_slice(),
        [crate::pin_entry::Message::Classified(Ok(
            crate::pin_entry::Verdict::Unlock
        ))]
    ));
    assert!(entry.take_fork_signer().is_some());
    assert!(entry.take_fork_signer().is_none());
    let (daemon, node, info) = crate::loader::start_connect_daemon(
        root.clone(),
        ChainId::BitcoinBlake2b,
        settings,
        client,
    )
    .await
    .unwrap();
    assert!(node.is_none());
    assert_eq!(info.network, bitcoin::Network::Bitcoin);
    daemon.stop().await.unwrap();
    features.assert_hits_async(3).await;
    forbidden_bitcoin.assert_hits_async(0).await;
    std::fs::remove_dir_all(root_path).unwrap();
}

#[tokio::test]
async fn feature_refusals_precede_anchor_requests_and_all_fork_filesystem_writes() {
    for chain in [ChainId::BitcoinBlake2b, ChainId::BitcoinBlake2bTestnet4] {
        for (status, flag) in [
            (200, None),
            (200, Some(false)),
            (401, None),
            (429, None),
            (503, None),
        ] {
            let server = MockServer::start_async().await;
            let features = server
                .mock_async(|when, then| {
                    when.method(GET).path("/api/v1/connect/features");
                    then.status(status).json_body(
                        json!({"success":true,"data":{"plans":[],"bitcoinBlake2bEnabled":flag}}),
                    );
                })
                .await;
            let anchor = server
                .mock_async(|when, then| {
                    when.path_contains("/anchor");
                    then.status(500);
                })
                .await;
            let root_path =
                std::env::temp_dir().join(format!("btcb2-no-write-{}", uuid::Uuid::new_v4()));
            let (mut installer, _) = Installer::try_new_for_chain(
                CoincubeDirectory::new(root_path.clone()),
                chain,
                None,
                UserFlow::CreateWallet,
                false,
                None,
                None,
                None,
                false,
                Some(client(&server)),
            )
            .unwrap();
            installer.context.restore_pin = Some(zeroize::Zeroizing::new("2468".to_string()));
            installer.context.fresh_fork_seed_backed_up = true;
            installer.context.descriptor = Some(DESCRIPTOR.parse().unwrap());
            installer.context.bitcoin_backend =
                Some(BitcoinBackend::Esplora(coincubed::config::EsploraConfig {
                    addr: format!("{}{}", server.base_url(), connect_esplora_path(chain)),
                    token: None,
                    fallback_addr: None,
                    fallback_token: None,
                    secondary_fallback_addr: None,
                    secondary_fallback_token: None,
                }));
            let wallet_id = WalletId::generate(installer.context.descriptor.as_ref().unwrap());
            assert!(
                install_local_wallet(installer.context, wallet_id, installer.signer)
                    .await
                    .is_err()
            );
            assert!(!root_path.exists(), "{chain:?} HTTP{status} flag{flag:?}");
            features.assert_hits_async(1).await;
            anchor.assert_hits_async(0).await;
        }
    }
}
