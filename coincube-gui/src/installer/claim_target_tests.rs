//! The Bitcoin Blake2b **claim target**: what the flow admits, what it hands
//! into the fork construction path, and what lands on disk.
//!
//! Synthetic datadirs and a mock Connect server only — no OS secrets, no real
//! node, no existing wallet.
// The session slot is process-global, so every test here holds
// `session::test_guard()` for its whole body — including across `.await`. That
// is the point: the guard is what stops two tests sharing the one slot, and
// these are `#[tokio::test]` single-threaded runtimes, so there is no other
// task to starve.
#![allow(clippy::await_holding_lock)]

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

/// The source Cube's PIN. The target's seed file is written under it — that is
/// the whole of invariant I10's "unlock behaves identically".
const SOURCE_PIN: &str = "2468";

fn authenticated_client(server: &MockServer) -> CoincubeClient {
    let mut client = CoincubeClient::new();
    client.base_url = server.base_url();
    client.set_token("synthetic-claim-token");
    client
}

/// A source Cube with an open session, which is what a claim launches from —
/// `try_new_for_chain` refuses one without a PIN in this session.
fn source(name: &str) -> (ClaimSource, Fingerprint) {
    let signer = Signer::generate(Network::Bitcoin).unwrap();
    let fingerprint = signer.fingerprint();
    let cube_id = uuid::Uuid::new_v4().to_string();
    crate::app::session::open(
        cube_id.clone(),
        zeroize::Zeroizing::new(SOURCE_PIN.to_string()),
    );
    (
        ClaimSource {
            cube: {
                let mut cube = crate::app::settings::CubeSettings::new_with_raw_id(
                    cube_id,
                    name.to_string(),
                    ChainId::Bitcoin,
                );
                // A source Cube whose mnemonic the user has written down: the
                // target inherits that rather than nagging for the same words.
                cube.backed_up = true;
                cube
            },
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
    // One process-global session slot: serialise with every other test that
    // opens one, or a sibling's `open` replaces this source's PIN.
    let _guard = crate::app::session::test_guard();
    let server = MockServer::start_async().await;
    let (source, _) = source("Savings");
    let source_cube_id = source.cube_id().to_string();
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
    // One process-global session slot: serialise with every other test that
    // opens one, or a sibling's `open` replaces this source's PIN.
    let _guard = crate::app::session::test_guard();
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
/// running Bitcoin Cube does — must not have them reach the Cube being built
/// on the fork chain. Deleting the narrowing in `try_new_for_chain` fails this
/// test. What the installer *may* hold is the source Cube's own handles,
/// parked for the return trip; that is asserted here too, because the two are
/// easy to conflate and one of them is an invariant.
#[tokio::test]
async fn a_fork_installer_is_never_handed_the_callers_breez_or_spark_clients() {
    // One process-global session slot: serialise with every other test that
    // opens one, or a sibling's `open` replaces this source's PIN.
    let _guard = crate::app::session::test_guard();
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
    // Parked, not handed in — and the distinction is the whole point. The
    // installer holds the source Cube's handle so backing out can rebuild that
    // Cube; what it must never do is give it to the Cube being built. On the
    // success path the installer is dropped when the target's unlock screen
    // replaces it, releasing the last `Arc` and shutting the Spark bridge down
    // — correct behaviour for leaving a Cube, not a leak.
    assert_eq!(
        Arc::strong_count(&breez),
        2,
        "the caller's handle and the parked one"
    );
    assert!(installer
        .source_cube
        .as_ref()
        .is_some_and(|s| s.breez_client.is_some()));
}

/// The account gate is re-checked at admission. A card press, or a rail item
/// that was rendered a moment ago, is not a permission.
#[tokio::test]
async fn an_unauthenticated_claim_is_refused_before_anything_is_built() {
    // One process-global session slot: serialise with every other test that
    // opens one, or a sibling's `open` replaces this source's PIN.
    let _guard = crate::app::session::test_guard();
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
    // One process-global session slot: serialise with every other test that
    // opens one, or a sibling's `open` replaces this source's PIN.
    let _guard = crate::app::session::test_guard();
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
    // One process-global session slot: serialise with every other test that
    // opens one, or a sibling's `open` replaces this source's PIN.
    let _guard = crate::app::session::test_guard();
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
    // One process-global session slot: serialise with every other test that
    // opens one, or a sibling's `open` replaces this source's PIN.
    let _guard = crate::app::session::test_guard();
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

/// Every path that opens a Cube consumes an armed intent, and every path that
/// ends a session drops it. Both are asserted against the *production*
/// functions rather than against `claim_intent` alone: the module's guarantee
/// is only as wide as its call sites, and one of them was missing.
#[test]
fn an_armed_intent_does_not_survive_an_unrelated_open_or_a_revoked_session() {
    let _guard = crate::app::session::test_guard();

    // A walletless Cube open, driven through the production function rather
    // than through `claim_intent` directly — the guarantee is only as wide as
    // its call sites, and this one was missing. The take is the first line of
    // `new_without_wallet`, ahead of every other branch, so a Cube this build
    // refuses still consumes: that is the point, since a refused open is still
    // an open the user performed.
    crate::app::claim_intent::arm("cube-a");
    let refused = crate::app::App::new_without_wallet(
        std::sync::Arc::new(crate::app::breez_liquid::BreezClient::disconnected(
            Network::Bitcoin,
        )),
        None,
        crate::app::Config {
            log_level: None,
            debug: None,
            start_internal_bitcoind: false,
        },
        temp_root("walletless"),
        Network::Bitcoin,
        crate::app::settings::CubeSettings::new_with_raw_id(
            "walletless-cube-b".to_string(),
            "Walletless".to_string(),
            ChainId::BitcoinBlake2b,
        ),
    );
    assert!(
        refused.is_err(),
        "this build refuses a walletless fork Cube"
    );
    assert!(
        !crate::app::claim_intent::take("cube-a"),
        "an intent must not survive the open of another Cube"
    );

    // The per-Cube revocation path (`invalidate_fork_session` → `close_cube`).
    crate::app::claim_intent::arm("cube-a");
    crate::app::session::close_cube("some-other-cube");
    assert!(
        !crate::app::claim_intent::take("cube-a"),
        "a revoked session must not leave a claim intent armed"
    );

    // And a lock / duress close.
    crate::app::claim_intent::arm("cube-a");
    crate::app::session::close();
    assert!(!crate::app::claim_intent::take("cube-a"));
}

/// **The end-to-end producer path.** Every other test here asserts a piece;
/// this one runs the install the user's press actually triggers, then the exit
/// seam that mints the Cube, and checks the two ends agree.
///
/// The gap this closes: the earlier seed test handed `persist_cube_master_seed`
/// a literal PIN, so it could not see that a claim reached `seed_password` with
/// no credentials at all — the source Cube's PIN is resolved from its session,
/// not from `cube_settings`, which a claim deliberately leaves `None`.
#[tokio::test]
async fn a_claim_install_writes_a_seed_the_minted_cube_can_open() {
    let _guard = crate::app::session::test_guard();
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path_contains("/connect/");
            then.status(200)
                .json_body(serde_json::json!({"success": true, "data": {}}));
        })
        .await;
    let (source, source_fingerprint) = source("Savings");
    let root = temp_root("e2e");

    let (installer, _) = Installer::try_new_for_chain(
        root.clone(),
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

    // 1. The credentials the install will encrypt under exist at all. This is
    //    the assertion whose absence let the gap through.
    let password = installer
        .context
        .seed_password()
        .expect("a claim must reach the seed write with the source Cube's PIN");
    assert_eq!(password.as_str(), SOURCE_PIN);

    // 2. The seed the install writes, through the production helper and the
    //    production context — not a literal.
    let target_cube_id = installer.context.seed_cube_id().to_owned();
    let seed = claim::target_master_seed(installer.context.claim_source.as_ref().unwrap()).unwrap();
    persist_cube_master_seed(
        &seed,
        &installer.context.coincube_directory,
        ChainId::BitcoinBlake2b,
        installer.context.seed_password().unwrap().as_str(),
        &target_cube_id,
        None,
    )
    .unwrap();

    // 3. The Cube the exit seam would mint. `find_or_create_cube` falls back to
    //    a fresh UUID when the identity is absent, which would leave the seed
    //    written above bound to a Cube id nothing has.
    let identity = crate::gui::tab::installer_exit_identity(&installer)
        .expect("the claim target's own identity must reach the exit seam");
    assert_eq!(
        identity.uuid, target_cube_id,
        "the minted Cube must be the Cube the seed file is bound to"
    );
    assert_eq!(identity.name, "Savings · BTCB2");

    let exit_seed = crate::gui::tab::installer_exit_seed(&installer)
        .expect("the minted Cube must record which master signer it holds");
    assert_eq!(exit_seed.master_signer_fingerprint, source_fingerprint);
    assert_eq!(exit_seed.pin.as_str(), SOURCE_PIN);
    assert!(
        exit_seed.seed_backed_up,
        "the source Cube's backup state is inherited, not reset"
    );

    // 4. And the file opens with exactly those credentials.
    let path = coincube_core::signer::MasterSigner::mnemonics_folder_for_chain(
        root.path(),
        ChainId::BitcoinBlake2b,
    )
    .join(
        coincube_core::signer::MnemonicFileName {
            fingerprint: source_fingerprint,
            descriptor_info: None,
        }
        .to_string(),
    );
    assert!(coincube_core::seed_crypt::decrypt_with(
        &std::fs::read(&path).unwrap(),
        exit_seed.pin.as_str(),
        &identity.uuid,
        None,
    )
    .is_ok());
    let _ = std::fs::remove_dir_all(root.path());
}

/// A passkey source Cube has no PIN, and the fork refuses a passkey unlock
/// outright — so a claim from one would mint a Cube nothing could open. Refused
/// at admission rather than at the seed write.
#[tokio::test]
async fn a_claim_is_refused_when_the_source_cube_has_no_pin_in_this_session() {
    let _guard = crate::app::session::test_guard();
    let server = MockServer::start_async().await;
    let (mut passkey_source, _) = source("Passkey Cube");
    passkey_source.cube.passkey_metadata = Some(crate::app::settings::PasskeyMetadata {
        credential_id: "synthetic-credential".to_string(),
        rp_id: "coincube.io".to_string(),
        created_at: 0,
        label: None,
    });
    assert!(passkey_source.cube.is_passkey_cube());
    // A passkey Cube's session: unlocked, but `pin_for` answers `None`.
    crate::app::session::open_without_pin(passkey_source.cube_id().to_string());
    let root = temp_root("nopin");
    let root_path = root.path().to_path_buf();

    let refused = Installer::try_new_for_chain(
        root,
        ChainId::BitcoinBlake2b,
        None,
        UserFlow::ClaimBlake2b {
            from_cube: Box::new(passkey_source),
        },
        true,
        None,
        None,
        None,
        false,
        Some(authenticated_client(&server)),
    );
    let message = refused.err().unwrap().to_string();
    assert!(
        message.contains("passkey"),
        "a passkey owner must not be told to reopen their Cube — there is no \
         PIN to reopen it with, so that advice loops forever: {}",
        message
    );
    assert!(!root_path.exists(), "and nothing on disk");

    // The same condition, the other audience: a PIN Cube whose session lapsed
    // gets advice that works.
    let (lapsed, _) = source("PIN Cube");
    crate::app::session::close();
    let root = temp_root("lapsed");
    let refused = Installer::try_new_for_chain(
        root,
        ChainId::BitcoinBlake2b,
        None,
        UserFlow::ClaimBlake2b {
            from_cube: Box::new(lapsed),
        },
        true,
        None,
        None,
        None,
        false,
        Some(authenticated_client(&server)),
    );
    let message = refused.err().unwrap().to_string();
    assert!(message.contains("Reopen this Cube"), "{}", message);
    assert!(!message.contains("passkey"), "{}", message);
}

/// Backing out of a claim must put the user back in the Cube they started in.
///
/// `BackToApp` restores from `cube_settings`, which a claim deliberately leaves
/// `None` (the installer is building a Cube on another chain), so without the
/// parked source it falls through to Home — the user presses Claim, changes
/// their mind, and is logged out of their Cube for it. Driven through the real
/// `Tab::update` arm, not through the parked field.
#[tokio::test]
async fn backing_out_of_a_claim_returns_to_the_source_cube_not_to_home() {
    let _guard = crate::app::session::test_guard();
    let server = MockServer::start_async().await;
    let (source, _) = source("Savings");
    let source_cube_id = source.cube_id().to_string();
    let root = temp_root("cancel");
    // `BackToApp` reads the source network's gui config.
    let source_dir = root.network_directory(ChainId::Bitcoin);
    std::fs::create_dir_all(source_dir.path()).unwrap();
    std::fs::write(
        source_dir
            .path()
            .join(crate::app::config::DEFAULT_FILE_NAME),
        b"",
    )
    .unwrap();

    let (installer, _) = Installer::try_new_for_chain(
        root.clone(),
        ChainId::BitcoinBlake2b,
        None,
        UserFlow::ClaimBlake2b {
            from_cube: Box::new(source),
        },
        true,
        None,
        Some(Arc::new(
            crate::app::breez_liquid::BreezClient::disconnected(Network::Bitcoin),
        )),
        None,
        false,
        Some(authenticated_client(&server)),
    )
    .unwrap();

    // The fork build still sees nothing: parking is not handing in.
    assert!(installer.breez_client.is_none());
    assert!(installer.spark_backend.is_none());
    assert!(installer.context.cube_encryption_key.is_none());
    assert!(
        installer
            .source_cube
            .as_ref()
            .is_some_and(|s| s.breez_client.is_some()),
        "the source Cube's handle is parked for the return trip"
    );

    let mut tab = crate::gui::tab::Tab::new(0, crate::gui::tab::State::Installer(installer));
    let _ = tab.update(crate::gui::tab::Message::Install(Message::BackToApp(
        Network::Bitcoin,
    )));

    match &tab.state {
        crate::gui::tab::State::Loader(loader) => assert_eq!(
            loader.cube_settings.id, source_cube_id,
            "cancel must land back in the source Cube"
        ),
        other => panic!(
            "cancelling a claim must return to the source Cube, not {:?}",
            std::mem::discriminant(other)
        ),
    }
    let _ = std::fs::remove_dir_all(root.path());
}
