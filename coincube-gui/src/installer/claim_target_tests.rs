//! The Bitcoin Blake2b **claim target**: what the flow admits, what it hands
//! into the fork construction path, and what lands on disk.
//!
//! Synthetic datadirs and a mock Connect server only — no OS secrets, no real
//! node, no existing wallet.
use super::*;
use crate::chain::ChainId;
use crate::services::coincube::CoincubeClient;
use coincube_core::miniscript::bitcoin::bip32::Fingerprint;
use httpmock::prelude::*;

/// A real mainnet Vault descriptor. A claim reuses the source Cube's verbatim,
/// so the test asserts on the string rather than on its contents.
const SOURCE_DESCRIPTOR: &str = concat!(
    "wsh(andor(pk([aabbccdd]xpub68JJTXc1MWK8KLW4HGLXZBJknja7kDUJuFHnM424LbziEXsfkh1WQCiEjjHw4z",
    "LqSUm4rvhgyGkkuRowE9tCJSgt3TQB5J3SKAbZ2SdcKST/<0;1>/*),older(10000),pk([aabbccdd]xpub68JJT",
    "Xc1MWK8PEQozKsRatrUHXKFNkD1Cb1BuQU9Xr5moCv87anqGyXLyUd4KpnDyZgo3gz4aN1r3NiaoweFW8Uut",
    "BsBbgKHzaD5HkTkifK/<0;1>/*)))#3xh8xmhn"
);

fn authenticated_client(server: &MockServer) -> CoincubeClient {
    let mut client = CoincubeClient::new();
    client.base_url = server.base_url();
    client.set_token("synthetic-claim-token");
    client
}

fn source(name: &str) -> (ClaimSource, Fingerprint) {
    let signer = Signer::generate(Network::Bitcoin).unwrap();
    let fingerprint = signer.fingerprint();
    (
        ClaimSource {
            cube_id: uuid::Uuid::new_v4().to_string(),
            cube_name: name.to_string(),
            descriptor: SOURCE_DESCRIPTOR.parse().unwrap(),
            signer: Arc::new(signer),
        },
        fingerprint,
    )
}

fn temp_root(tag: &str) -> CoincubeDirectory {
    CoincubeDirectory::new(
        std::env::temp_dir().join(format!("claim-{tag}-{}", uuid::Uuid::new_v4())),
    )
}

/// The target is built from the source Cube's descriptor and gets an identity
/// of its own: its own Cube id (I7) and an alias naming the chain.
#[tokio::test]
async fn a_claim_target_reuses_the_descriptor_and_takes_its_own_cube_identity() {
    let server = MockServer::start_async().await;
    let (source, _) = source("Savings");
    let source_cube_id = source.cube_id.clone();
    let root = temp_root("identity");

    let (installer, _) = Installer::try_new_for_chain(
        root,
        ChainId::BitcoinBlake2b,
        None,
        UserFlow::ClaimBlake2b {
            from_cube: Box::new(source),
        },
        true,
        None,
        None,
        None,
        false,
        Some(authenticated_client(&server)),
    )
    .expect("an authenticated claim is admitted on the fork chain");

    assert_eq!(
        installer.context.descriptor.as_ref().unwrap().to_string(),
        SOURCE_DESCRIPTOR,
        "a claim that changed the descriptor would watch different addresses"
    );
    assert_eq!(installer.context.wallet_alias, "Savings · BTCB2");
    let target_cube_id = installer.context.seed_cube_id();
    assert!(!target_cube_id.is_empty(), "the target is its own Cube");
    assert_ne!(
        target_cube_id, source_cube_id,
        "sharing the source Cube's id would collapse the two Cubes (I7)"
    );
    assert!(
        !installer.context.fresh_fork_cube,
        "a claim target's seed comes from the source Cube, not from a fresh one"
    );
}

/// The seed decision, asserted where it lands rather than where it is made:
/// the file written under the fork chain decrypts to the **source** Cube's
/// mnemonic, and sits beside — never inside — the Bitcoin folder.
#[test]
fn the_target_master_seed_file_is_the_source_cube_mnemonic_on_the_fork_chain() {
    let root = temp_root("seed");
    let (source, fingerprint) = source("Savings");
    let expected = zeroize::Zeroizing::new(source.signer.mnemonic().join(" "));
    let target_cube_id = uuid::Uuid::new_v4().to_string();

    let seed = claim::target_master_seed(&source).expect("the recorded decision reuses the seed");
    persist_cube_master_seed(
        &seed,
        &root,
        ChainId::BitcoinBlake2b,
        "1234",
        &target_cube_id,
        None,
    )
    .unwrap();

    let path = coincube_core::signer::MasterSigner::mnemonics_folder_for_chain(
        root.path(),
        ChainId::BitcoinBlake2b,
    )
    .join(
        coincube_core::signer::MnemonicFileName {
            fingerprint,
            descriptor_info: None,
        }
        .to_string(),
    );
    let plaintext = coincube_core::seed_crypt::decrypt_with(
        &std::fs::read(&path).unwrap(),
        "1234",
        &target_cube_id,
        None,
    )
    .unwrap();
    assert_eq!(
        plaintext.as_slice(),
        expected.as_bytes(),
        "the target must be able to sign for the source descriptor"
    );
    assert!(
        !root
            .path()
            .join("bitcoin")
            .join("mnemonics")
            .join(
                coincube_core::signer::MnemonicFileName {
                    fingerprint,
                    descriptor_info: None,
                }
                .to_string()
            )
            .exists(),
        "the fork's copy must never be written into the Bitcoin family's folder"
    );
    let _ = std::fs::remove_dir_all(root.path());
}

/// Item 6, at the construction boundary rather than through an extracted
/// predicate: a caller that holds live Breez and Spark handles — which every
/// running Bitcoin Cube does — must not have them reach a fork installer.
/// Deleting the narrowing in `try_new_for_chain` fails this test.
#[tokio::test]
async fn a_fork_installer_is_never_handed_the_callers_breez_or_spark_clients() {
    let server = MockServer::start_async().await;
    let breez = Arc::new(crate::app::breez_liquid::BreezClient::disconnected(
        Network::Bitcoin,
    ));
    let (source, _) = source("Savings");

    let (installer, _) = Installer::try_new_for_chain(
        temp_root("clients"),
        ChainId::BitcoinBlake2b,
        None,
        UserFlow::ClaimBlake2b {
            from_cube: Box::new(source),
        },
        true,
        None,
        Some(breez.clone()),
        None,
        false,
        Some(authenticated_client(&server)),
    )
    .expect("holding a Liquid client is what a running Bitcoin Cube looks like");

    assert!(
        installer.breez_client.is_none(),
        "a Bitcoin Blake2b Cube must never be given a Liquid client"
    );
    assert!(installer.spark_backend.is_none());
    assert!(
        installer.context.cube_encryption_key.is_none(),
        "nor anything derived from the source Cube's Liquid signer"
    );
    // The source Cube keeps its own handle: dropping it is what re-spawns the
    // Spark bridge subprocess on the round trip.
    assert_eq!(Arc::strong_count(&breez), 1);
}

/// The account gate is re-checked at admission. A card press, or a rail item
/// that was rendered a moment ago, is not a permission.
#[tokio::test]
async fn an_unauthenticated_claim_is_refused_before_anything_is_built() {
    let (source, _) = source("Savings");
    let root = temp_root("unauth");
    let root_path = root.path().to_path_buf();

    let refused = Installer::try_new_for_chain(
        root,
        ChainId::BitcoinBlake2b,
        None,
        UserFlow::ClaimBlake2b {
            from_cube: Box::new(source),
        },
        true,
        None,
        None,
        None,
        false,
        None,
    );
    assert!(refused.is_err(), "no Connect session, no claim");
    assert!(!root_path.exists(), "and nothing on disk");
}

/// Every other flow stays refused on the fork chain — the gate was widened to
/// exactly one more flow, not opened.
#[tokio::test]
async fn the_fork_chain_still_refuses_every_flow_but_creation_and_claim() {
    let server = MockServer::start_async().await;
    for flow in [
        UserFlow::AddWallet,
        UserFlow::RestoreVaultFromRecoveryKit,
        UserFlow::RestoreFromRecoveryKit { cube_uuid: None },
        UserFlow::RecoverInheritedVault {
            cube_id: 1,
            full_cube: true,
        },
        UserFlow::RecoverOwnCubeWithPhone {
            cube_id: 1,
            full_cube: false,
        },
    ] {
        let refused = Installer::try_new_for_chain(
            temp_root("flows"),
            ChainId::BitcoinBlake2b,
            None,
            flow.clone(),
            false,
            None,
            None,
            None,
            false,
            Some(authenticated_client(&server)),
        );
        assert!(refused.is_err(), "{:?} must stay refused on the fork", flow);
    }
}

/// A claim target's flow collects nothing the source Cube already answers, and
/// in particular never shows the mnemonic again: the source Cube backed it up,
/// and a second showing is a second physical copy of one secret.
#[tokio::test]
async fn the_claim_flow_never_asks_for_a_pin_a_descriptor_or_the_mnemonic_again() {
    let server = MockServer::start_async().await;
    let (source, _) = source("Savings");
    let (installer, _) = Installer::try_new_for_chain(
        temp_root("steps"),
        ChainId::BitcoinBlake2b,
        None,
        UserFlow::ClaimBlake2b {
            from_cube: Box::new(source),
        },
        true,
        None,
        None,
        None,
        false,
        Some(authenticated_client(&server)),
    )
    .unwrap();

    // Asserted through the context rather than step identity: the descriptor
    // and the alias are already settled, and no PIN step can run because the
    // target inherits the source Cube's.
    assert_eq!(
        installer.steps.len(),
        5,
        "descriptor → Connect → node → alias → done"
    );
    assert!(installer.context.descriptor.is_some());
    assert!(installer.context.restore_pin.is_none());
    assert!(
        !installer.context.fresh_fork_seed_backed_up,
        "no backup step runs, so nothing marks one as done"
    );
}

/// The "already claimed" input to the visibility matrix, read from the fork
/// chain's settings file rather than from the Connect session — a target that
/// exists must not look absent because the API is unreachable.
#[test]
fn an_existing_target_is_found_by_descriptor_on_the_fork_chain_only() {
    let root = temp_root("already");
    let fork_dir = root.network_directory(ChainId::BitcoinBlake2b);
    let bitcoin_dir = root.network_directory(ChainId::Bitcoin);
    let descriptor: coincube_core::descriptors::CoincubeDescriptor =
        SOURCE_DESCRIPTOR.parse().unwrap();
    let checksum = WalletId::generate(&descriptor).descriptor_checksum;

    // Nothing on disk at all.
    assert!(!crate::app::claim_target_exists(&root, &checksum));

    // The source Cube's own Vault, on the Bitcoin side, is not a claim target.
    let wallet = crate::app::settings::WalletSettings {
        name: "Vault".to_string(),
        alias: None,
        descriptor_checksum: checksum.clone(),
        pinned_at: None,
        keys: Vec::new(),
        hardware_wallets: Vec::new(),
        remote_backend_auth: None,
        start_internal_bitcoind: None,
        pending_rescan: None,
    };
    for dir in [&bitcoin_dir, &fork_dir] {
        std::fs::create_dir_all(dir.path()).unwrap();
    }
    let settings = crate::app::settings::Settings {
        wallets: vec![wallet.clone()],
        ..Default::default()
    };
    std::fs::write(
        bitcoin_dir
            .path()
            .join(crate::app::settings::SETTINGS_FILE_NAME),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    assert!(
        !crate::app::claim_target_exists(&root, &checksum),
        "the Bitcoin Cube's own Vault is the source, not a target"
    );

    // The same descriptor on the fork chain is one.
    std::fs::write(
        fork_dir
            .path()
            .join(crate::app::settings::SETTINGS_FILE_NAME),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    assert!(crate::app::claim_target_exists(&root, &checksum));
    assert!(
        !crate::app::claim_target_exists(&root, "someone-elses-checksum"),
        "answered per descriptor, not per device"
    );
    let _ = std::fs::remove_dir_all(root.path());
}
