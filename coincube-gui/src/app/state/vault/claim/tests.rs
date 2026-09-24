use super::*;
use crate::services::coincube::{
    network_anchor::{AnchorState, NetworkAnchor, NetworkAnchorStatus},
    network_status::{ForkActivation, NetworkObservation, RdtsFlagday, RdtsStatus},
};
use coincube_core::{
    descriptors::CoincubeDescriptor,
    miniscript::bitcoin::{Amount, OutPoint},
};
use std::{str::FromStr, sync::Mutex};

/// Single-key primary path, native SegWit: what the coordinator admits.
const SINGLE_WSH: &str = "wsh(or_d(pk([f5acc2fd]tpubD6NzVbkrYhZ4YgUx2ZLNt2rLYAMTdYysCRzKoLu2BeSHKvzqPaBDvf17GeBPnExUVPkuBpx4kniP964e2MxyzzazcXLptxLXModSVCVEV1T/<0;1>/*),and_v(v:pkh([8a64f2a9]tpubD6NzVbkrYhZ4WmzFjvQrp7sDa4ECUxTi9oby8K4FZkd3XCBtEdKwUiQyYJaxiJo5y42gyDWEczrFpozEjeLxMPxjf2WtkfcbpUdfvNnozWF/<0;1>/*),older(10))))#d72le4dr";
/// A 1-of-2 primary path: refused by the coordinator (`Coordinator::open`).
const MULTI_WSH: &str = "wsh(or_d(multi(1,[573fb35b/48'/1'/0'/2']tpubDFKp9T7WAYDcENSjoifkrpq1gMDF47KGJcJrpxzX23Qor8wuGbrEVs9utNq1MDS8E2WXJSBk1qoPQLpwyokW7DiUNPwFuxQkL7owNkLAb9W/<0;1>/*,[573fb35c/48'/1'/1'/2']tpubDFGezyzuHJPhdP3jHGW7v7Hwes4Hihqv5W2yyCmRY9VZJCRchETvxrMC8uECeJZdxQ14V4iD4DecoArkUSDwj8ogYE9WEv4MNZr12thNHCs/<0;1>/*),and_v(v:multi(2,[573fb35b/48'/1'/2'/2']tpubDDwxQauiaU964vPzt5Vd7jnDHEUtp2Vc34PaWpEXg5TQ3bRccxnc1MKKh88Hi7xiMeZo9Tm6fBcq4UGXqnDtGUniJLjqAD8SjQ8Eci3aSR7/<0;1>/*,[573fb35c/48'/1'/3'/2']tpubDE37XAVB5CQ1x85md3BQ5uHCoMwT5fgT8X13zzCUQ3x5o2jskYxKjj7Qcxt1Jpj4QB8tqspn2dooPCekRuQDYrDHov7J1ueUNu2wcvgRDxr/<0;1>/*),older(1000))))#fccaqlhh";
/// Taproot: refused by the coordinator and by the core builder.
const TAPROOT: &str = "tr([abcdef01]xpub6Eze7yAT3Y1wGrnzedCNVYDXUqa9NmHVWck5emBaTbXtURbe1NWZbK9bsz1TiVE7Cz341PMTfYgFw1KdLWdzcM1UMFTcdQfCYhhXZ2HJvTW/<0;1>/*,and_v(v:pk([abcdef01]xpub688Hn4wScQAAiYJLPg9yH27hUpfZAUnmJejRQBCiwfP5PEDzjWMNW1wChcninxr5gyavFqbbDjdV1aK5USJz8NDVjUy7FRQaaqqXHh5SbXe/<0;1>/*),older(52560)))#0mt7e93c";

fn wallet(descriptor: &str) -> Arc<Wallet> {
    Arc::new(Wallet::new(
        CoincubeDescriptor::from_str(descriptor).unwrap(),
    ))
}

fn coin(height: Option<i32>, spent: bool, immature: bool, n: u32) -> Coin {
    Coin {
        amount: Amount::from_sat(10_000 + u64::from(n)),
        outpoint: OutPoint::new(Txid::from_byte_array([n as u8; 32]), n),
        address: Address::from_str("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4")
            .unwrap()
            .assume_checked(),
        block_height: height,
        derivation_index: ChildNumber::from_normal_idx(n).unwrap(),
        spend_info: spent.then_some(coincubed::commands::LCSpendInfo {
            txid: Txid::from_byte_array([9; 32]),
            height: None,
        }),
        is_immature: immature,
        is_change: false,
        is_from_self: false,
    }
}

#[test]
fn pre_fork_coins_are_those_confirmed_below_the_fork_height() {
    let fork = 90;
    let set = partition_coins(
        vec![
            coin(Some(89), false, false, 1), // pre-fork
            coin(Some(90), false, false, 2), // the fork block: Bitcoin-only
            coin(Some(91), false, false, 3), // post-fork
            coin(None, false, false, 4),     // unconfirmed: neither
            coin(Some(10), true, false, 5),  // spent
            coin(Some(10), false, true, 6),  // immature coinbase
            coin(Some(1), false, false, 7),  // pre-fork
        ],
        fork,
        105,
    );
    let pre: Vec<u32> = set.pre_fork.iter().map(|c| c.outpoint.vout).collect();
    assert_eq!(
        pre,
        vec![1, 7],
        "sorted by outpoint, below the fork height only"
    );
    assert_eq!(set.post_fork, 2, "at and above the fork height");
    assert_eq!(set.tip_height, 105);
}

#[test]
fn vault_shape_refusal_admits_single_key_p2wsh_only() {
    assert_eq!(vault_shape_refusal(&wallet(SINGLE_WSH)), None);
    let multi = vault_shape_refusal(&wallet(MULTI_WSH)).expect("multisig primary refused");
    assert!(multi.contains("single-key"), "{}", multi);
    let taproot = vault_shape_refusal(&wallet(TAPROOT)).expect("taproot refused");
    assert!(taproot.contains("Taproot"), "{}", taproot);
}

fn anchor(mtp: i64, expiry: i64, active: bool, observed_at: i64) -> NetworkAnchorStatus {
    NetworkAnchorStatus {
        network: ChainId::BitcoinBlake2b,
        state: AnchorState::Available,
        anchor: Some(NetworkAnchor {
            tip_hash: BlockHash::from_byte_array([2; 32]),
            tip_height: 100,
            tip_median_time_past: mtp,
            observed_at,
            observation: NetworkObservation {
                tip_height: 100,
                fork: Some(ForkActivation {
                    height: 90,
                    active: true,
                }),
                rdts: RdtsStatus::Flagday {
                    flagday: RdtsFlagday {
                        height: 90,
                        expiry_time: expiry,
                        active,
                    },
                },
            },
        }),
    }
}

/// The window's verdict is core's `assess_deployment`, so the wizard refuses
/// exactly when the coordinator would: inside the margin, expired, inactive.
#[test]
fn rdts_window_verdicts_come_from_core() {
    let now = 1_000_000;
    let fork_hash = BlockHash::from_byte_array([7; 32]);
    let open = evaluate_anchor(
        anchor(now, now + EXPIRY_MARGIN_SECONDS + 1, true, now),
        fork_hash,
        now,
    )
    .unwrap();
    assert_eq!(open.rdts, Ok(()));
    assert_eq!(open.fork_height, 90);
    assert_eq!(open.fork_hash, fork_hash);
    assert_eq!(open.expires_at, now + EXPIRY_MARGIN_SECONDS + 1);

    let margin = evaluate_anchor(
        anchor(now, now + EXPIRY_MARGIN_SECONDS, true, now),
        fork_hash,
        now,
    )
    .unwrap();
    assert_eq!(margin.rdts, Err(Assessment::ExpiryMargin));

    let expired = evaluate_anchor(anchor(now, now - 1, false, now), fork_hash, now).unwrap();
    assert_eq!(expired.rdts, Err(Assessment::RdtsExpired));

    let inactive =
        evaluate_anchor(anchor(now, now + 10_000_000, false, now), fork_hash, now).unwrap();
    assert_eq!(inactive.rdts, Err(Assessment::RdtsInactive));

    // A stale anchor is not a verdict at all.
    let stale = evaluate_anchor(
        anchor(now, now + 10_000_000, true, now - 1000),
        fork_hash,
        now,
    );
    assert!(stale.unwrap_err().contains("Stale"));
}

#[test]
fn durations_read_naturally() {
    assert_eq!(describe_duration(36 * 3600), "36 hours");
    assert_eq!(describe_duration(48 * 3600), "2 days");
    assert_eq!(describe_duration(30 * 60), "30 minutes");
}

fn panel(descriptor: &str) -> ClaimStep1Panel {
    let root = std::env::temp_dir().join(format!("claim-panel-{}", uuid::Uuid::new_v4()));
    ClaimStep1Panel::new(
        wallet(descriptor),
        CoincubeDirectory::new(root),
        "bitcoin-cube".into(),
        watch::channel(1).1,
        None,
    )
}

fn checked_ok(now: i64) -> Checked {
    Checked {
        window: evaluate_anchor(
            anchor(now, now + EXPIRY_MARGIN_SECONDS + 1, true, now),
            BlockHash::from_byte_array([7; 32]),
            now,
        ),
        coins: Ok(partition_coins(
            vec![coin(Some(1), false, false, 1)],
            90,
            105,
        )),
        feerate_vb: Ok(5),
        backend: Ok(()),
    }
}

/// Refusals come in the order a user can act on them, and a refusal that no
/// probe can change never offers a retry.
#[test]
fn refusals_come_in_actionable_order() {
    let mut p = panel(MULTI_WSH);
    let target = p.refusal().unwrap();
    assert!(
        target.reason.contains("Create the claim target"),
        "{}",
        target.reason
    );
    assert!(!target.retry);

    p.pre.target = Some("fork-cube".into());
    let shape = p.refusal().unwrap();
    assert!(shape.reason.contains("single-key"), "{}", shape.reason);
    assert!(!shape.retry);

    let mut p = panel(SINGLE_WSH);
    p.pre.target = Some("fork-cube".into());
    let connect = p.refusal().unwrap();
    assert!(
        connect.reason.contains("Sign in to Connect"),
        "{}",
        connect.reason
    );

    p.connect = Some(ConnectSession {
        client: CoincubeClient::new(),
        account: "7".into(),
    });
    assert_eq!(
        p.refusal(),
        None,
        "nothing checked yet: no refusal, no build"
    );
    assert!(!p.can_build());

    let now = 1_000_000;
    p.pre.checked = Some(checked_ok(now));
    assert_eq!(p.refusal(), None);
    assert!(p.can_build());

    let mut backend = checked_ok(now);
    backend.backend = Err("wrong backend".into());
    p.pre.checked = Some(backend);
    let refused = p.refusal().unwrap();
    assert_eq!(refused.reason, "wrong backend");
    assert!(!refused.retry);

    let mut margin = checked_ok(now);
    margin.window = evaluate_anchor(
        anchor(now, now + 60, true, now),
        BlockHash::from_byte_array([7; 32]),
        now,
    );
    p.pre.checked = Some(margin);
    let refused = p.refusal().unwrap();
    assert!(
        refused.reason.contains("expires too soon"),
        "{}",
        refused.reason
    );
    assert!(
        refused.reason.contains("36 hours needed"),
        "{}",
        refused.reason
    );
    assert!(!refused.retry);

    let mut empty = checked_ok(now);
    empty.coins = Ok(partition_coins(
        vec![coin(Some(95), false, false, 1)],
        90,
        105,
    ));
    p.pre.checked = Some(empty);
    let refused = p.refusal().unwrap();
    assert!(
        refused.reason.contains("Nothing to split"),
        "{}",
        refused.reason
    );
    assert!(refused.retry);

    let mut fee = checked_ok(now);
    fee.feerate_vb = Err("offline".into());
    p.pre.checked = Some(fee);
    let refused = p.refusal().unwrap();
    assert!(refused.reason.contains("fee rate"), "{}", refused.reason);
    assert!(refused.retry);
    assert!(!p.can_build());
}

/// Records which calls reach it. Every method that is not a recorded one is
/// unreachable here: the test only dispatches the calls it names.
#[derive(Debug, Default)]
struct Recording {
    hits: Mutex<Vec<&'static str>>,
}
impl Recording {
    fn hit(&self, name: &'static str) {
        self.hits.lock().unwrap().push(name);
    }
}

#[async_trait::async_trait]
impl Daemon for Recording {
    fn backend(&self) -> DaemonBackend {
        DaemonBackend::EmbeddedCoincubed(None)
    }
    fn config(&self) -> Option<&coincubed::config::Config> {
        None
    }
    async fn is_alive(&self, _: &CoincubeDirectory, _: Network) -> Result<(), DaemonError> {
        unreachable!()
    }
    async fn stop(&self) -> Result<(), DaemonError> {
        unreachable!()
    }
    async fn get_info(&self) -> Result<model::GetInfoResult, DaemonError> {
        unreachable!()
    }
    async fn request_sync(&self) -> Result<(), DaemonError> {
        unreachable!()
    }
    async fn get_new_address(&self) -> Result<model::GetAddressResult, DaemonError> {
        unreachable!()
    }
    async fn list_revealed_addresses(
        &self,
        _: bool,
        _: bool,
        _: usize,
        _: Option<ChildNumber>,
    ) -> Result<model::ListRevealedAddressesResult, DaemonError> {
        unreachable!()
    }
    async fn update_deriv_indexes(
        &self,
        _: Option<u32>,
        _: Option<u32>,
    ) -> Result<UpdateDerivIndexesResult, DaemonError> {
        unreachable!()
    }
    async fn list_coins(
        &self,
        _: &[CoinStatus],
        _: &[OutPoint],
    ) -> Result<model::ListCoinsResult, DaemonError> {
        self.hit("list_coins");
        Ok(model::ListCoinsResult { coins: Vec::new() })
    }
    async fn list_spend_txs(&self) -> Result<model::ListSpendResult, DaemonError> {
        unreachable!()
    }
    async fn create_spend_tx(
        &self,
        _: &[OutPoint],
        _: &HashMap<Address<address::NetworkUnchecked>, u64>,
        _: u64,
        _: Option<Address<address::NetworkUnchecked>>,
    ) -> Result<model::CreateSpendResult, DaemonError> {
        unreachable!()
    }
    async fn rbf_psbt(
        &self,
        _: &Txid,
        _: bool,
        _: Option<u64>,
    ) -> Result<model::CreateSpendResult, DaemonError> {
        unreachable!()
    }
    async fn update_spend_tx(&self, _: &Psbt) -> Result<(), DaemonError> {
        self.hit("update_spend_tx");
        Ok(())
    }
    async fn delete_spend_tx(&self, _: &Txid) -> Result<(), DaemonError> {
        self.hit("delete_spend_tx");
        Ok(())
    }
    async fn broadcast_spend_tx(&self, _: &Txid) -> Result<(), DaemonError> {
        self.hit("broadcast_spend_tx");
        Ok(())
    }
    async fn start_rescan(&self, _: u32) -> Result<(), DaemonError> {
        unreachable!()
    }
    async fn list_confirmed_txs(
        &self,
        _: u32,
        _: u32,
        _: u64,
    ) -> Result<model::ListTransactionsResult, DaemonError> {
        unreachable!()
    }
    async fn create_recovery(
        &self,
        _: Address<address::NetworkUnchecked>,
        _: &[OutPoint],
        _: u64,
        _: Option<u16>,
    ) -> Result<Psbt, DaemonError> {
        unreachable!()
    }
    async fn list_txs(&self, _: &[Txid]) -> Result<model::ListTransactionsResult, DaemonError> {
        self.hit("list_txs");
        Ok(model::ListTransactionsResult {
            transactions: Vec::new(),
        })
    }
    async fn get_labels(
        &self,
        _: &HashSet<LabelItem>,
    ) -> Result<HashMap<String, String>, DaemonError> {
        unreachable!()
    }
    async fn update_labels(
        &self,
        _: &HashMap<LabelItem, Option<String>>,
    ) -> Result<(), DaemonError> {
        self.hit("update_labels");
        Ok(())
    }
    async fn get_labels_bip329(&self, _: u32, _: u32) -> Result<Labels, DaemonError> {
        unreachable!()
    }
}

/// The signing flow's daemon forwards reads and refuses every write that
/// would put the claim PSBT into the Vault's spend list or broadcast it.
#[tokio::test]
async fn the_signing_daemon_forwards_reads_and_swallows_spend_store_writes() {
    let inner = Arc::new(Recording::default());
    let signing = SigningOnlyDaemon(inner.clone());
    let psbt = crate::app::state::vault::test_support::empty_psbt();
    let txid = Txid::from_byte_array([1; 32]);

    assert!(signing.update_spend_tx(&psbt).await.is_ok());
    assert!(signing.update_labels(&HashMap::new()).await.is_ok());
    assert!(signing.delete_spend_tx(&txid).await.is_ok());
    assert!(matches!(
        signing.broadcast_spend_tx(&txid).await,
        Err(DaemonError::ClientNotSupported)
    ));
    assert!(
        inner.hits.lock().unwrap().is_empty(),
        "no write reached the daemon: {:?}",
        inner.hits.lock().unwrap()
    );

    // Control: reads do reach it.
    signing.list_coins(&[], &[]).await.unwrap();
    signing.list_txs(&[]).await.unwrap();
    assert_eq!(*inner.hits.lock().unwrap(), vec!["list_coins", "list_txs"]);
}

/// Gandalf's reviewer regression (#518 review, finding 4): the App has no
/// daemon after a failed backend switch, and the direct Claim route delivers
/// completions with whatever it has. A completion must be processed, not
/// panic — a session may travel inside it.
#[test]
fn reviewer_claim_completion_without_daemon_must_not_panic() {
    let mut p = panel(SINGLE_WSH);
    let seq = p.check_seq;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(p.update(
            None,
            &Cache::default(),
            Message::Claim(ClaimEvent::Checked(seq, Box::new(checked_ok(1_000_000)))),
        ));
    }));
    assert!(result.is_ok(), "claim completion panics in daemon-less App");
    assert!(p.coins().is_some(), "and the completion was applied");
}

/// The whole slice, end to end, through the real panel: a fake embedded
/// daemon answers the wallet reads and carries the submission, an httpmock
/// Connect answers the anchor, the Esplora reads and the preflight, and the
/// Vault's hot key signs through the panel's own `PsbtState`. Every stage
/// transition is the panel's; every async step is the task the panel
/// returned, run to completion and fed back.
mod flow {
    use super::*;
    use crate::{daemon::model::GetInfoResult, signer::Signer};
    use coincube_core::{
        bip39::Mnemonic,
        claim_finalize::finalize_poison_transfer,
        descriptors::{CoincubeDescriptor, CoincubePolicy, PathInfo},
        miniscript::{
            bitcoin::{
                absolute, bip32::DerivationPath, transaction, Amount, OutPoint, TxIn, TxOut,
            },
            DescriptorPublicKey,
        },
        signer::MasterSigner,
    };
    use coincubed::commands::GetInfoDescriptors;
    use coincubed::poison_broadcast::{SubmissionGate, SubmissionOutcome};
    use httpmock::prelude::*;
    use iced::futures::StreamExt;
    use serde_json::json;
    use std::{path::PathBuf, sync::Mutex};

    fn signer(byte: u8) -> MasterSigner {
        MasterSigner::from_mnemonic(
            Network::Bitcoin,
            Mnemonic::from_entropy(&[byte; 16]).unwrap(),
        )
        .unwrap()
    }

    fn key(s: &MasterSigner, secp: &secp256k1::Secp256k1<secp256k1::All>) -> DescriptorPublicKey {
        DescriptorPublicKey::from_str(&format!(
            "[{}]{}/<0;1>/*",
            s.fingerprint(secp),
            s.xpub_at(&DerivationPath::default(), secp)
        ))
        .unwrap()
    }

    /// A single-key P2WSH Vault (primary: the hot key; recovery: another key
    /// after 46 blocks) and one 100 000-sat coin it received at height 50.
    struct Fixture {
        descriptor: CoincubeDescriptor,
        hot: MasterSigner,
        previous: Transaction,
        coin: Coin,
    }

    fn fixture() -> Fixture {
        let secp = secp256k1::Secp256k1::new();
        let hot = signer(40);
        let recovery = signer(42);
        let descriptor = CoincubeDescriptor::new(
            CoincubePolicy::new_legacy(
                PathInfo::Single(key(&hot, &secp)),
                std::iter::once((46, PathInfo::Single(key(&recovery, &secp)))).collect(),
            )
            .unwrap(),
        );
        let verify = secp256k1::Secp256k1::verification_only();
        let script_pubkey = descriptor
            .receive_descriptor()
            .derive(0.into(), &verify)
            .script_pubkey();
        let previous = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(100_000),
                script_pubkey: script_pubkey.clone(),
            }],
        };
        let coin = Coin {
            amount: Amount::from_sat(100_000),
            outpoint: OutPoint::new(previous.compute_txid(), 0),
            address: Address::from_script(&script_pubkey, Network::Bitcoin).unwrap(),
            block_height: Some(50),
            derivation_index: ChildNumber::from_normal_idx(0).unwrap(),
            spend_info: None,
            is_immature: false,
            is_change: false,
            is_from_self: false,
        };
        Fixture {
            descriptor,
            hot,
            previous,
            coin,
        }
    }

    /// The embedded daemon as the panel sees it: wallet reads answered from
    /// the fixture, a change reservation, and the exact-byte submission.
    /// Records every write so the test can prove which reached it.
    #[derive(Debug)]
    struct FlowDaemon {
        config: coincubed::config::Config,
        coin: Coin,
        previous: Transaction,
        hits: Mutex<Vec<&'static str>>,
    }
    impl FlowDaemon {
        fn hit(&self, name: &'static str) {
            self.hits.lock().unwrap().push(name);
        }
        fn hits(&self) -> Vec<&'static str> {
            self.hits.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Daemon for FlowDaemon {
        fn backend(&self) -> DaemonBackend {
            DaemonBackend::EmbeddedCoincubed(Some(crate::node::NodeType::Esplora))
        }
        fn config(&self) -> Option<&coincubed::config::Config> {
            Some(&self.config)
        }
        async fn is_alive(&self, _: &CoincubeDirectory, _: Network) -> Result<(), DaemonError> {
            Ok(())
        }
        async fn stop(&self) -> Result<(), DaemonError> {
            unreachable!()
        }
        async fn get_info(&self) -> Result<model::GetInfoResult, DaemonError> {
            self.hit("get_info");
            Ok(GetInfoResult {
                version: String::new(),
                network: Network::Bitcoin,
                block_height: 105,
                sync: 1.0,
                descriptors: GetInfoDescriptors {
                    main: self.config.main_descriptor.clone(),
                },
                rescan_progress: None,
                refused_reorg_depth: None,
                chain_divergence: false,
                timestamp: 0,
                last_poll_timestamp: None,
                receive_index: 1,
                change_index: 0,
            })
        }
        async fn request_sync(&self) -> Result<(), DaemonError> {
            Ok(())
        }
        async fn get_new_address(&self) -> Result<model::GetAddressResult, DaemonError> {
            unreachable!()
        }
        async fn list_revealed_addresses(
            &self,
            _: bool,
            _: bool,
            _: usize,
            _: Option<ChildNumber>,
        ) -> Result<model::ListRevealedAddressesResult, DaemonError> {
            unreachable!()
        }
        async fn update_deriv_indexes(
            &self,
            _: Option<u32>,
            _: Option<u32>,
        ) -> Result<UpdateDerivIndexesResult, DaemonError> {
            unreachable!()
        }
        async fn list_coins(
            &self,
            statuses: &[CoinStatus],
            _: &[OutPoint],
        ) -> Result<model::ListCoinsResult, DaemonError> {
            self.hit("list_coins");
            assert_eq!(statuses, &[CoinStatus::Confirmed]);
            Ok(model::ListCoinsResult {
                coins: vec![self.coin.clone()],
            })
        }
        async fn list_spend_txs(&self) -> Result<model::ListSpendResult, DaemonError> {
            self.hit("list_spend_txs");
            Ok(model::ListSpendResult {
                spend_txs: Vec::new(),
            })
        }
        async fn create_spend_tx(
            &self,
            _: &[OutPoint],
            _: &HashMap<Address<address::NetworkUnchecked>, u64>,
            _: u64,
            _: Option<Address<address::NetworkUnchecked>>,
        ) -> Result<model::CreateSpendResult, DaemonError> {
            unreachable!()
        }
        async fn rbf_psbt(
            &self,
            _: &Txid,
            _: bool,
            _: Option<u64>,
        ) -> Result<model::CreateSpendResult, DaemonError> {
            unreachable!()
        }
        async fn update_spend_tx(&self, _: &Psbt) -> Result<(), DaemonError> {
            self.hit("update_spend_tx");
            Ok(())
        }
        async fn delete_spend_tx(&self, _: &Txid) -> Result<(), DaemonError> {
            self.hit("delete_spend_tx");
            Ok(())
        }
        async fn broadcast_spend_tx(&self, _: &Txid) -> Result<(), DaemonError> {
            self.hit("broadcast_spend_tx");
            Ok(())
        }
        async fn reserve_change(&self) -> Result<ChildNumber, DaemonError> {
            self.hit("reserve_change");
            Ok(ChildNumber::from_normal_idx(12).unwrap())
        }
        async fn submit_verified_poison(
            &self,
            verified: Arc<coincube_core::claim_finalize::VerifiedPoisonTransfer>,
            _gate: Arc<SubmissionGate>,
        ) -> Result<SubmissionOutcome, DaemonError> {
            self.hit("submit_verified_poison");
            Ok(SubmissionOutcome::UpstreamAccepted {
                txid: verified.transaction().compute_txid(),
                wtxid: verified.transaction().compute_wtxid(),
            })
        }
        async fn start_rescan(&self, _: u32) -> Result<(), DaemonError> {
            unreachable!()
        }
        async fn list_confirmed_txs(
            &self,
            _: u32,
            _: u32,
            _: u64,
        ) -> Result<model::ListTransactionsResult, DaemonError> {
            unreachable!()
        }
        async fn create_recovery(
            &self,
            _: Address<address::NetworkUnchecked>,
            _: &[OutPoint],
            _: u64,
            _: Option<u16>,
        ) -> Result<Psbt, DaemonError> {
            unreachable!()
        }
        async fn list_txs(
            &self,
            txids: &[Txid],
        ) -> Result<model::ListTransactionsResult, DaemonError> {
            self.hit("list_txs");
            assert_eq!(txids, &[self.previous.compute_txid()]);
            Ok(model::ListTransactionsResult {
                transactions: vec![coincubed::commands::TransactionInfo {
                    tx: self.previous.clone(),
                    height: Some(50),
                    time: None,
                }],
            })
        }
        async fn get_labels(
            &self,
            _: &HashSet<LabelItem>,
        ) -> Result<HashMap<String, String>, DaemonError> {
            Ok(HashMap::new())
        }
        async fn update_labels(
            &self,
            _: &HashMap<LabelItem, Option<String>>,
        ) -> Result<(), DaemonError> {
            self.hit("update_labels");
            Ok(())
        }
        async fn get_labels_bip329(&self, _: u32, _: u32) -> Result<Labels, DaemonError> {
            unreachable!()
        }
    }

    /// Put a claim target for `wallet` on the fork chain's settings file, as
    /// the target installer leaves it.
    fn write_claim_target(root: &CoincubeDirectory, wallet: &Wallet) {
        use crate::app::settings::{CubeSettings, Settings, WalletId, SETTINGS_FILE_NAME};
        let fork_dir = root.network_directory(ChainId::BitcoinBlake2b);
        std::fs::create_dir_all(fork_dir.path()).unwrap();
        let mut target = CubeSettings::new_with_raw_id(
            "fork-cube".to_string(),
            "Fixture · BTCB2".to_string(),
            ChainId::BitcoinBlake2b,
        );
        target.vault_wallet_id = Some(WalletId::new(wallet.descriptor_checksum.clone(), Some(1)));
        let settings = Settings {
            cubes: vec![target],
            ..Default::default()
        };
        std::fs::write(
            fork_dir.path().join(SETTINGS_FILE_NAME),
            serde_json::to_vec(&settings).unwrap(),
        )
        .unwrap();
    }

    /// Run a task the panel returned and hand back everything it produced.
    async fn outputs(task: Task<Message>) -> Vec<Message> {
        let mut out = Vec::new();
        let Some(mut stream) = iced_runtime::task::into_stream(task) else {
            return out;
        };
        while let Some(action) = stream.next().await {
            if let iced_runtime::Action::Output(message) = action {
                out.push(message);
            }
        }
        out
    }

    fn fresh(then: httpmock::Then) -> httpmock::Then {
        then.header("X-Coincube-Observation", "fresh")
            .header("X-Cache", "BYPASS")
            .header("Cache-Control", "no-store")
    }

    fn unix_now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// Everything the flow needs after the panel has reached the review.
    struct Flow {
        p: ClaimStep1Panel,
        daemon: Arc<FlowDaemon>,
        dyn_daemon: Arc<dyn Daemon + Sync + Send>,
        cache: Cache,
        _server: MockServer,
        datadir: CoincubeDirectory,
        wallet: Arc<Wallet>,
        root: PathBuf,
        sender: watch::Sender<u64>,
        unsigned_txid: Txid,
    }

    /// Preconditions → build → sign (the hot key through the panel's own
    /// `PsbtState`), asserting each stage, up to the finalise task — handed
    /// back unpolled, so a test can change the panel's context between its
    /// dispatch and its result, as a sign-out does.
    async fn reach_signed() -> (Flow, Task<Message>) {
        let f = fixture();
        let server = MockServer::start_async().await;
        let now = unix_now();
        let fork_hash = "07".repeat(32);
        let fork_tip_hash = "02".repeat(32);
        let btc_tip_hash = "01".repeat(32);

        // Connect: the authenticated anchor, then the anonymous Esplora reads.
        server.mock_async(|when, then| {
            when.method(GET).path("/api/v1/connect/networks/bitcoin-blake2b/anchor")
                .header("authorization", "Bearer flow-token");
            then.status(200).json_body(json!({"success":true,"data":{
                "network":"bitcoin-blake2b","state":"available","anchor":{
                    "tip_hash":fork_tip_hash,"tip_height":100,"tip_median_time_past":now,
                    "observed_at":now,
                    "observation":{"tip_height":100,"fork":{"height":90,"active":true},
                        "rdts":{"state":"flagday","flagday":{"height":90,"expiry_time":now + EXPIRY_MARGIN_SECONDS + 3600,"active":true}}}
                }}}));
        }).await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api/v1/esplora/bitcoin-blake2b/mainnet/block-height/90");
                fresh(then.status(200)).body(&fork_hash);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api/v1/esplora/bitcoin-blake2b/mainnet/block-height/100");
                fresh(then.status(200)).body(&fork_tip_hash);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api/v1/esplora/bitcoin/mainnet/blocks/tip/hash");
                fresh(then.status(200)).body(&btc_tip_hash);
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET).path(format!(
                    "/api/v1/esplora/bitcoin/mainnet/block/{btc_tip_hash}/status"
                ));
                fresh(then.status(200)).json_body(json!({"in_best_chain":true,"height":105}));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api/v1/esplora/bitcoin/mainnet/block-height/105");
                fresh(then.status(200)).body(&btc_tip_hash);
            })
            .await;
        // The transaction is on neither chain before submission — and, with
        // an upstream acknowledgement but no mined block, after it either.
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path_contains("/api/v1/esplora/bitcoin/mainnet/tx/");
                fresh(then.status(404));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path_contains("/api/v1/esplora/bitcoin-blake2b/mainnet/tx/");
                fresh(then.status(404));
            })
            .await;

        let endpoint = format!("{}/api/v1/esplora/bitcoin/mainnet", server.base_url());
        let root = std::env::temp_dir().join(format!("claim-flow-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let config: coincubed::config::Config = toml::from_str(&format!(
            "main_descriptor = '{}'\ndata_directory = '{}'\n[bitcoin_config]\nnetwork = 'bitcoin'\n[esplora_config]\naddr = '{}'\n",
            f.descriptor,
            root.display(),
            endpoint
        ))
        .unwrap();
        let daemon = Arc::new(FlowDaemon {
            config,
            coin: f.coin.clone(),
            previous: f.previous.clone(),
            hits: Mutex::new(Vec::new()),
        });
        let dyn_daemon: Arc<dyn Daemon + Sync + Send> = daemon.clone();
        let mut wallet = Wallet::new(f.descriptor.clone());
        wallet.signer = Some(Arc::new(Signer::new(f.hot)));
        let wallet = Arc::new(wallet);
        let datadir = CoincubeDirectory::new(root.clone());
        let mut client = CoincubeClient::for_test(server.base_url());
        client.set_token("flow-token");
        let cache = Cache {
            network: Network::Bitcoin,
            fiat_chain: ChainId::Bitcoin,
            ..Cache::default()
        };
        let (sender, generation) = watch::channel(1);

        let mut p = ClaimStep1Panel::new(
            wallet.clone(),
            datadir.clone(),
            "bitcoin-cube".into(),
            generation,
            Some(ConnectSession {
                client,
                account: "7".into(),
            }),
        )
        .with_feerate_source(FeerateSource::Fixed(5));
        // The target, as the installer leaves it on disk: a Bitcoin Blake2b
        // Cube in the fork chain's settings file reusing this descriptor. The
        // probe re-reads it on every entry, so a poked field would not do.
        write_claim_target(&datadir, &wallet);

        // Preconditions: the probe runs on entry.
        let probe = p.reload(Some(dyn_daemon.clone()), Some(wallet.clone()));
        let mut produced = outputs(probe).await;
        assert_eq!(produced.len(), 1, "one probe result");
        let _ = p.update(Some(dyn_daemon.clone()), &cache, produced.remove(0));
        assert_eq!(p.refusal(), None, "{:?}", p.pre.checked);
        assert!(p.can_build());
        let coins = p.coins().unwrap();
        assert_eq!(coins.pre_fork.len(), 1);
        assert_eq!(
            p.window().unwrap().fork_hash,
            BlockHash::from_str(&fork_hash).unwrap()
        );

        // Build: the change index is the daemon's reservation, the marker is
        // an OP_RETURN over BIP-110's 83 bytes.
        let build = p.update(
            Some(dyn_daemon.clone()),
            &cache,
            Message::View(view::Message::Claim(view::ClaimMessage::Build)),
        );
        let mut produced = outputs(build).await;
        assert_eq!(produced.len(), 1);
        let _ = p.update(Some(dyn_daemon.clone()), &cache, produced.remove(0));
        let (unsigned_txid, marker_len, change_index) = match &p.stage {
            Stage::Plan { built } => (
                built.psbt().unsigned_tx.compute_txid(),
                built
                    .psbt()
                    .unsigned_tx
                    .output
                    .iter()
                    .find(|o| o.script_pubkey.is_op_return())
                    .map(|o| o.script_pubkey.len())
                    .unwrap(),
                built.change_index(),
            ),
            _ => panic!("expected the plan stage"),
        };
        assert_eq!(change_index, ChildNumber::from_normal_idx(12).unwrap());
        assert!(marker_len > 83, "{}", marker_len);
        assert_eq!(
            daemon.hits(),
            vec!["list_coins", "get_info", "reserve_change", "list_txs"]
        );
        // The plan renders (Gandalf's reviewer probe: `every_stage_renders`
        // reaches Plan and Sign only through here).
        let menu = Menu::Vault(crate::app::menu::VaultSubMenu::Claim);
        drop(view::vault::claim::view(&menu, &cache, &p));

        // Sign: the Vault's own flow. Open the picker, then merge the hot
        // key's signature exactly as the picker would receive it.
        let _ = p.update(
            Some(dyn_daemon.clone()),
            &cache,
            Message::View(view::Message::Claim(view::ClaimMessage::Sign)),
        );
        let opened = p.update(
            Some(dyn_daemon.clone()),
            &cache,
            Message::View(view::Message::Spend(view::SpendTxMessage::Sign)),
        );
        drop(opened);
        let (unsigned, fingerprint) = match &p.stage {
            Stage::Sign { psbt, .. } => {
                assert!(psbt.modal.is_some(), "the picker is open");
                (
                    psbt.tx.psbt.clone(),
                    wallet.signer.as_ref().unwrap().fingerprint(),
                )
            }
            _ => panic!("expected the sign stage"),
        };
        drop(view::vault::claim::view(&menu, &cache, &p));
        let signed = wallet.signer.as_ref().unwrap().sign_psbt(unsigned).unwrap();
        // The preflight answers for exactly the transaction that will be
        // submitted, so it is registered once the witness is known.
        let final_tx = match &p.stage {
            Stage::Sign {
                built: Some(built), ..
            } => {
                finalize_poison_transfer(built, &signed, &secp256k1::Secp256k1::verification_only())
                    .unwrap()
                    .transaction()
                    .clone()
            }
            _ => panic!("the construction is still in the stage"),
        };
        assert_eq!(final_tx.compute_txid(), unsigned_txid);
        server.mock_async(|when, then| {
            when.method(POST).path("/api/v1/esplora/bitcoin/mainnet/tx/preflight");
            then.status(200).header("cache-control", "no-store").json_body(json!({
                "success":true,"data":{"network":"mainnet","state":"available","result":{
                    "txid":final_tx.compute_txid(),"wtxid":final_tx.compute_wtxid(),
                    "tip_hash":btc_tip_hash,"observed_at":unix_now(),"allowed":true,"reject_reason":null}}}));
        }).await;

        let merged = p.update(
            Some(dyn_daemon.clone()),
            &cache,
            Message::Signed(fingerprint, Ok(signed)),
        );
        // The persist the picker asks for goes to the signing-only daemon:
        // it answers, and nothing reaches the real one.
        let mut produced = outputs(merged).await;
        assert!(
            produced
                .iter()
                .any(|m| matches!(m, Message::Updated(Ok(())))),
            "{:?}",
            produced
        );
        let updated = produced
            .iter()
            .position(|m| matches!(m, Message::Updated(Ok(()))))
            .unwrap();
        let ready = p.update(Some(dyn_daemon.clone()), &cache, produced.remove(updated));
        assert!(
            !daemon.hits().contains(&"update_spend_tx"),
            "the claim PSBT never reaches the spend store: {:?}",
            daemon.hits()
        );
        // Threshold met, picker closed: the finalise task is out, holding
        // the construction; the stage says so.
        assert!(
            matches!(
                &p.stage,
                Stage::Sign {
                    finalizing: Some(_),
                    built: None,
                    ..
                }
            ),
            "the finalise task holds the construction"
        );
        (
            Flow {
                p,
                daemon,
                dyn_daemon,
                cache,
                _server: server,
                datadir,
                wallet,
                root,
                sender,
                unsigned_txid,
            },
            ready,
        )
    }

    /// `reach_signed`, then finalise → journal → review, asserting each.
    async fn reach_review() -> Flow {
        let (mut f, ready) = reach_signed().await;
        let mut produced = outputs(ready).await;
        assert_eq!(produced.len(), 1, "{:?}", produced);
        assert!(
            matches!(&produced[0], Message::Claim(ClaimEvent::Ready(_, Ok(_)))),
            "{:?}",
            produced[0]
        );
        let review =
            f.p.update(Some(f.dyn_daemon.clone()), &f.cache, produced.remove(0));
        assert!(
            journal_directory(&f.datadir, &f.wallet)
                .join("intent.json")
                .is_file(),
            "the intent is journaled before any review"
        );
        assert!(f.p.revoker.is_some());
        assert!(!f.p.revoked);

        // Review: the snapshot is what the user confirms.
        let mut produced = outputs(review).await;
        assert_eq!(produced.len(), 1, "{:?}", produced);
        let _ =
            f.p.update(Some(f.dyn_daemon.clone()), &f.cache, produced.remove(0));
        let snapshot = review_snapshot(&f.p);
        assert_eq!(snapshot.txid, f.unsigned_txid);
        assert_eq!(snapshot.observations.bitcoin.tip.height, 105);
        assert_eq!(snapshot.observations.fork.tip.height, 100);
        f
    }

    /// `reach_review`, then confirm → submit → track, asserting each.
    async fn reach_track(f: &mut Flow) {
        let submit = f.p.update(
            Some(f.dyn_daemon.clone()),
            &f.cache,
            Message::View(view::Message::Claim(view::ClaimMessage::Confirm)),
        );
        let mut produced = outputs(submit).await;
        assert_eq!(produced.len(), 1, "{:?}", produced);
        assert!(
            matches!(
                &produced[0],
                Message::Claim(ClaimEvent::Submitted(
                    _,
                    Ok(Outcome::UpstreamAccepted { .. })
                ))
            ),
            "{:?}",
            produced[0]
        );
        let track =
            f.p.update(Some(f.dyn_daemon.clone()), &f.cache, produced.remove(0));
        let mut produced = outputs(track).await;
        assert_eq!(produced.len(), 1, "{:?}", produced);
        let _ =
            f.p.update(Some(f.dyn_daemon.clone()), &f.cache, produced.remove(0));
        assert!(matches!(
            &f.p.stage,
            Stage::Track {
                status: Some(Status::Observation(Assessment::WaitingForConfirmation)),
                busy: false,
                error: None,
                ..
            }
        ));
        assert_eq!(submissions(f), 1);
    }

    /// The review on screen, or a panic that says what the stage is.
    fn review_snapshot(p: &ClaimStep1Panel) -> ReviewSnapshot {
        match &p.stage {
            Stage::Review {
                snapshot: Some(s),
                busy: false,
                error: None,
                ..
            } => s.clone(),
            Stage::Review {
                snapshot,
                busy,
                error,
                ..
            } => panic!(
                "expected a review: {:?} busy={} error={:?}",
                snapshot, busy, error
            ),
            _ => panic!("expected the review stage"),
        }
    }

    /// (has a snapshot, error, busy, session bound) at Review or Track.
    fn session_state(p: &ClaimStep1Panel) -> (bool, Option<String>, bool, bool) {
        match &p.stage {
            Stage::Review {
                snapshot,
                error,
                busy,
                session,
            } => (
                snapshot.is_some(),
                error.clone(),
                *busy,
                session.as_ref().is_some_and(|s| s.is_bound()),
            ),
            Stage::Track {
                error,
                busy,
                session,
                ..
            } => (
                false,
                error.clone(),
                *busy,
                session.as_ref().is_some_and(|s| s.is_bound()),
            ),
            _ => panic!("expected the review or track stage"),
        }
    }

    /// The journal's recorded phase, as written.
    fn journaled_phase(f: &Flow) -> String {
        let journal = journal_directory(&f.datadir, &f.wallet);
        let journaled: serde_json::Value =
            serde_json::from_slice(&std::fs::read(journal.join("intent.json")).unwrap()).unwrap();
        journaled["phase"].as_str().unwrap().to_string()
    }

    fn submissions(f: &Flow) -> usize {
        f.daemon
            .hits()
            .iter()
            .filter(|h| **h == "submit_verified_poison")
            .count()
    }

    /// The fixture's Connect session for `account`: the same endpoint and
    /// credential `reach_signed` signed in with.
    fn session(f: &Flow, account: &str) -> ConnectSession {
        let mut client = CoincubeClient::for_test(f._server.base_url());
        client.set_token("flow-token");
        ConnectSession {
            client,
            account: account.into(),
        }
    }

    /// What the App does at a Connect sign-out, in its order: the panel
    /// loses its session (which revokes), then the App revokes and advances
    /// the generation.
    fn sign_out(f: &mut Flow) {
        f.p.set_connect(None);
        f.p.revoke();
        f.sender.send_modify(|g| *g += 1);
    }

    /// Run one panel task to completion and apply everything it produced,
    /// returning what was applied.
    async fn drive(f: &mut Flow, task: Task<Message>) -> Vec<Message> {
        let produced = outputs(task).await;
        let mut applied = Vec::new();
        for message in produced {
            let next = f.p.update(Some(f.dyn_daemon.clone()), &f.cache, message);
            applied.push(next);
        }
        let mut seen = Vec::new();
        for next in applied {
            for message in outputs(next).await {
                seen.push(message);
            }
        }
        seen
    }

    #[tokio::test]
    async fn step_one_runs_from_preconditions_to_tracking_through_the_panel() {
        // `sender` stays alive: closing the generation channel is itself a
        // revocation (the coordinator treats a closed watch as cancelled).
        let Flow {
            mut p,
            daemon,
            dyn_daemon,
            cache,
            datadir,
            wallet,
            root,
            sender: _sender,
            unsigned_txid,
            ..
        } = reach_review().await;
        let journal = journal_directory(&datadir, &wallet);

        // Submit: explicit confirmation; the journal records the intent
        // before the daemon carries the exact bytes; the outcome is tracked.
        let submit = p.update(
            Some(dyn_daemon.clone()),
            &cache,
            Message::View(view::Message::Claim(view::ClaimMessage::Confirm)),
        );
        let mut produced = outputs(submit).await;
        assert_eq!(produced.len(), 1, "{:?}", produced);
        assert!(
            matches!(
                &produced[0],
                Message::Claim(ClaimEvent::Submitted(
                    _,
                    Ok(Outcome::UpstreamAccepted { .. })
                ))
            ),
            "{:?}",
            produced[0]
        );
        let track = p.update(Some(dyn_daemon.clone()), &cache, produced.remove(0));
        assert_eq!(
            daemon
                .hits()
                .iter()
                .filter(|h| **h == "submit_verified_poison")
                .count(),
            1
        );
        assert!(!daemon.hits().contains(&"broadcast_spend_tx"));
        let journaled: serde_json::Value =
            serde_json::from_slice(&std::fs::read(journal.join("intent.json")).unwrap()).unwrap();
        assert_eq!(journaled["phase"], "BroadcastUncertain");
        assert_eq!(journaled["signed_txid"], unsigned_txid.to_string());

        let mut produced = outputs(track).await;
        assert_eq!(produced.len(), 1, "{:?}", produced);
        let _ = p.update(Some(dyn_daemon.clone()), &cache, produced.remove(0));
        match &p.stage {
            Stage::Track {
                outcome,
                status,
                busy: false,
                error: None,
                session: Some(session),
            } => {
                assert!(
                    matches!(outcome, Outcome::UpstreamAccepted { txid, .. } if *txid == unsigned_txid)
                );
                assert_eq!(
                    *status,
                    Some(Status::Observation(Assessment::WaitingForConfirmation))
                );
                assert_eq!(session.phase(), Phase::BroadcastUncertain);
            }
            _ => panic!("expected tracking"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A Connect sign-out between the review and the confirmation — the App
    /// hands the panel no session and bumps the generation — withdraws the
    /// review at once: the coordinator is revoked synchronously, a
    /// confirmation has nothing to act on, the daemon is never asked to
    /// submit, and the journal stays at intent. With and without the bump.
    #[tokio::test]
    async fn a_sign_out_between_review_and_confirm_refuses_the_submission() {
        for bump_generation in [false, true] {
            let mut f = reach_review().await;
            assert!(f.p.set_connect(None), "a sign-out replaces the session");
            if bump_generation {
                f.sender.send_modify(|g| *g += 1);
            }
            assert_eq!(
                session_state(&f.p),
                (false, Some(SESSION_ENDED.to_string()), false, true),
                "the review is withdrawn, the session stays (bound, revoked)"
            );
            assert!(f.p.revoked);
            let submit = f.p.update(
                Some(f.dyn_daemon.clone()),
                &f.cache,
                Message::View(view::Message::Claim(view::ClaimMessage::Confirm)),
            );
            let produced = outputs(submit).await;
            assert!(produced.is_empty(), "{:?}", produced);
            assert_eq!(submissions(&f), 0, "{:?}", f.daemon.hits());
            assert_eq!(journaled_phase(&f), "Intent");
            let _ = std::fs::remove_dir_all(&f.root);
        }
    }

    /// Gandalf's reviewer regression (#518 review, finding 1): the App signs
    /// out after the finalise task is dispatched and before it is first
    /// polled. The task must not bind the old client under the new
    /// generation: it is refused before anything is journaled, the
    /// construction and its signatures come back to the Sign stage, and a
    /// confirmation has nothing to submit. Signing in again records it.
    #[tokio::test]
    async fn reviewer_logout_before_finalize_must_not_reauthorize_submission() {
        let (mut f, ready) = reach_signed().await;
        sign_out(&mut f);
        assert!(f.p.connect.is_none());
        let mut produced = outputs(ready).await;
        assert_eq!(produced.len(), 1, "{:?}", produced);
        match &produced[0] {
            Message::Claim(ClaimEvent::Ready(_, Err((_, reason)))) => {
                assert_eq!(reason, SIGNED_OUT_BEFORE_RECORD);
            }
            other => panic!("expected a refused finalisation, got {:?}", other),
        }
        let journal = journal_directory(&f.datadir, &f.wallet);
        assert!(
            !journal.join("intent.json").exists(),
            "refused before the journal write"
        );
        let _ =
            f.p.update(Some(f.dyn_daemon.clone()), &f.cache, produced.remove(0));
        assert!(
            matches!(
                &f.p.stage,
                Stage::Sign {
                    built: Some(_),
                    finalizing: None,
                    error: Some(_),
                    ..
                }
            ),
            "the construction is back at Sign, with its signatures"
        );
        let task = f.p.update(
            Some(f.dyn_daemon.clone()),
            &f.cache,
            Message::View(view::Message::Claim(view::ClaimMessage::Confirm)),
        );
        let output = outputs(task).await;
        assert_eq!(
            submissions(&f),
            0,
            "signed out before finalization was polled, yet submitted: {output:?}"
        );

        // Signing in again: the App hands the session in and calls
        // `recover`, which finalises under the new generation.
        assert!(!f.p.set_connect(Some(session(&f, "7"))));
        let finalize = f.p.recover(Some(f.dyn_daemon.clone()));
        let mut produced = outputs(finalize).await;
        assert_eq!(produced.len(), 1, "{:?}", produced);
        assert!(
            matches!(&produced[0], Message::Claim(ClaimEvent::Ready(_, Ok(_)))),
            "{:?}",
            produced[0]
        );
        let review =
            f.p.update(Some(f.dyn_daemon.clone()), &f.cache, produced.remove(0));
        assert!(journal.join("intent.json").is_file());
        assert!(!f.p.revoked);
        let mut produced = outputs(review).await;
        assert_eq!(produced.len(), 1, "{:?}", produced);
        let _ =
            f.p.update(Some(f.dyn_daemon.clone()), &f.cache, produced.remove(0));
        assert_eq!(review_snapshot(&f.p).txid, f.unsigned_txid);
        let _ = std::fs::remove_dir_all(&f.root);
    }

    /// Finding 1, the other window: the finalise task has already journaled
    /// the intent when the App signs out, and its result is applied after.
    /// The session is installed revoked — the journal is the record — with
    /// no review to confirm and none prepared; a confirmation, and a "review
    /// again" without a session, submit nothing.
    #[tokio::test]
    async fn a_sign_out_after_the_intent_is_journaled_installs_a_revoked_review() {
        let (mut f, ready) = reach_signed().await;
        let mut produced = outputs(ready).await;
        assert!(matches!(
            &produced[0],
            Message::Claim(ClaimEvent::Ready(_, Ok(_)))
        ));
        assert_eq!(journaled_phase(&f), "Intent");
        sign_out(&mut f);
        let after =
            f.p.update(Some(f.dyn_daemon.clone()), &f.cache, produced.remove(0));
        assert!(
            outputs(after).await.is_empty(),
            "no review is prepared for a revoked session"
        );
        assert!(f.p.revoked);
        assert_eq!(
            session_state(&f.p),
            (false, Some(SESSION_ENDED.to_string()), false, true)
        );
        for intent in [view::ClaimMessage::Confirm, view::ClaimMessage::Refresh] {
            let task = f.p.update(
                Some(f.dyn_daemon.clone()),
                &f.cache,
                Message::View(view::Message::Claim(intent)),
            );
            let produced = outputs(task).await;
            assert!(produced.is_empty(), "{:?}", produced);
        }
        assert_eq!(submissions(&f), 0, "{:?}", f.daemon.hits());
        assert_eq!(journaled_phase(&f), "Intent");
        let _ = std::fs::remove_dir_all(&f.root);
    }

    /// Finding 5: after an ordinary sign-out at Review, signing in again
    /// with the same account re-binds the journaled intent under the new
    /// generation — `Coordinator::resume` reopens it (the journal's identity
    /// digest is account and provider, not generation), the construction is
    /// re-validated, the signatures re-verified — and the claim goes on to
    /// a fresh review and a submission, exactly once.
    #[tokio::test]
    async fn signing_in_again_with_the_same_account_rebinds_and_submits() {
        let mut f = reach_review().await;
        sign_out(&mut f);
        assert_eq!(journaled_phase(&f), "Intent");
        // Signing in again: the App hands the session in and calls `recover`.
        assert!(
            !f.p.set_connect(Some(session(&f, "7"))),
            "a first sign-in replaces nothing"
        );
        let rebind = f.p.recover(Some(f.dyn_daemon.clone()));
        let mut produced = outputs(rebind).await;
        assert_eq!(produced.len(), 1, "{:?}", produced);
        assert!(
            matches!(
                &produced[0],
                Message::Claim(ClaimEvent::Rebound(_, _, Ok(())))
            ),
            "{:?}",
            produced[0]
        );
        let review =
            f.p.update(Some(f.dyn_daemon.clone()), &f.cache, produced.remove(0));
        assert!(!f.p.revoked);
        // Busy: the re-bound session is out with the review task it started.
        assert_eq!(session_state(&f.p), (false, None, true, false));
        let mut produced = outputs(review).await;
        assert_eq!(produced.len(), 1, "{:?}", produced);
        let _ =
            f.p.update(Some(f.dyn_daemon.clone()), &f.cache, produced.remove(0));
        assert_eq!(review_snapshot(&f.p).txid, f.unsigned_txid);
        reach_track(&mut f).await;
        assert_eq!(journaled_phase(&f), "BroadcastUncertain");
        let _ = std::fs::remove_dir_all(&f.root);
    }

    /// Finding 5, refused: another account cannot take over the journal —
    /// the re-bind is refused by the journal's identity check, the session
    /// stays unbound and the intent untouched; the right account then
    /// continues.
    #[tokio::test]
    async fn signing_in_with_another_account_is_refused() {
        let mut f = reach_review().await;
        sign_out(&mut f);
        assert!(!f.p.set_connect(Some(session(&f, "8"))));
        let rebind = f.p.recover(Some(f.dyn_daemon.clone()));
        let mut produced = outputs(rebind).await;
        assert_eq!(produced.len(), 1, "{:?}", produced);
        match &produced[0] {
            Message::Claim(ClaimEvent::Rebound(_, _, Err(reason))) => {
                assert_eq!(reason, OTHER_ACCOUNT);
            }
            other => panic!("expected a refused re-bind, got {:?}", other),
        }
        let after =
            f.p.update(Some(f.dyn_daemon.clone()), &f.cache, produced.remove(0));
        assert!(outputs(after).await.is_empty());
        assert!(f.p.revoked);
        assert_eq!(
            session_state(&f.p),
            (false, Some(OTHER_ACCOUNT.to_string()), false, false),
            "unbound: the refused re-bind released the revoked coordinator"
        );
        let task = f.p.update(
            Some(f.dyn_daemon.clone()),
            &f.cache,
            Message::View(view::Message::Claim(view::ClaimMessage::Confirm)),
        );
        assert!(outputs(task).await.is_empty());
        assert_eq!(submissions(&f), 0);
        assert_eq!(journaled_phase(&f), "Intent");

        // The account the claim was recorded under replaces the other one:
        // revoked (nothing to revoke), bumped by the App, then re-bound.
        assert!(f.p.set_connect(Some(session(&f, "7"))));
        f.sender.send_modify(|g| *g += 1);
        let rebind = f.p.recover(Some(f.dyn_daemon.clone()));
        let mut produced = outputs(rebind).await;
        assert!(
            matches!(
                &produced[0],
                Message::Claim(ClaimEvent::Rebound(_, _, Ok(())))
            ),
            "{:?}",
            produced[0]
        );
        let review =
            f.p.update(Some(f.dyn_daemon.clone()), &f.cache, produced.remove(0));
        let mut produced = outputs(review).await;
        let _ =
            f.p.update(Some(f.dyn_daemon.clone()), &f.cache, produced.remove(0));
        assert_eq!(review_snapshot(&f.p).txid, f.unsigned_txid);
        let _ = std::fs::remove_dir_all(&f.root);
    }

    /// Finding 2's Some→Some half, at the panel: a session replaced by
    /// another credential revokes the coordinator and withdraws the review;
    /// the same session again replaces nothing.
    #[tokio::test]
    async fn a_replaced_session_revokes_and_withdraws_the_review() {
        let mut f = reach_review().await;
        assert!(
            !f.p.set_connect(Some(session(&f, "7"))),
            "same account, endpoint and credential"
        );
        review_snapshot(&f.p);
        let mut other = session(&f, "7");
        other.client.set_token("another-token");
        assert!(f.p.set_connect(Some(other)), "another credential");
        assert!(f.p.revoked);
        assert_eq!(
            session_state(&f.p),
            (false, Some(SESSION_ENDED.to_string()), false, true)
        );
        let task = f.p.update(
            Some(f.dyn_daemon.clone()),
            &f.cache,
            Message::View(view::Message::Claim(view::ClaimMessage::Confirm)),
        );
        assert!(outputs(task).await.is_empty());
        assert_eq!(submissions(&f), 0);
        assert_eq!(journaled_phase(&f), "Intent");
        let _ = std::fs::remove_dir_all(&f.root);
    }

    /// Finding 5 at Track: a sign-out after the submission, then the same
    /// account back — re-bound and reconciled, never resubmitted (the
    /// coordinator refuses a review once a submission is recorded).
    #[tokio::test]
    async fn signing_in_again_at_tracking_reconciles_without_resubmitting() {
        let mut f = reach_review().await;
        reach_track(&mut f).await;
        sign_out(&mut f);
        assert_eq!(
            session_state(&f.p),
            (false, Some(SESSION_ENDED.to_string()), false, true)
        );
        assert!(!f.p.set_connect(Some(session(&f, "7"))));
        let rebind = f.p.recover(Some(f.dyn_daemon.clone()));
        let seen = drive(&mut f, rebind).await;
        assert!(
            matches!(&seen[..], [Message::Claim(ClaimEvent::Tracked(_, Ok(_)))]),
            "{:?}",
            seen
        );
        // (`drive` applied the re-bind and ran the reconcile it started; the
        // tracked status is applied here.)
        let mut seen = seen;
        let _ =
            f.p.update(Some(f.dyn_daemon.clone()), &f.cache, seen.remove(0));
        assert!(!f.p.revoked);
        match &f.p.stage {
            Stage::Track {
                status,
                session: Some(session),
                busy: false,
                error: None,
                ..
            } => {
                assert_eq!(
                    *status,
                    Some(Status::Observation(Assessment::WaitingForConfirmation))
                );
                assert_eq!(session.phase(), Phase::BroadcastUncertain);
                assert!(session.is_bound());
            }
            _ => panic!("expected tracking"),
        }
        assert_eq!(submissions(&f), 1, "{:?}", f.daemon.hits());
        assert_eq!(journaled_phase(&f), "BroadcastUncertain");
        let _ = std::fs::remove_dir_all(&f.root);
    }

    /// A cancel while the finalise task holds the construction is refused:
    /// the result decides whether the attempt was journaled.
    #[tokio::test]
    async fn cancel_is_refused_while_finalising() {
        let (mut f, ready) = reach_signed().await;
        let _ = f.p.update(
            Some(f.dyn_daemon.clone()),
            &f.cache,
            Message::View(view::Message::Claim(view::ClaimMessage::Cancel)),
        );
        assert!(matches!(
            &f.p.stage,
            Stage::Sign {
                finalizing: Some(_),
                ..
            }
        ));
        let mut produced = outputs(ready).await;
        let _ =
            f.p.update(Some(f.dyn_daemon.clone()), &f.cache, produced.remove(0));
        assert!(matches!(&f.p.stage, Stage::Review { .. }));
        let _ = std::fs::remove_dir_all(&f.root);
    }

    /// Finding 4, the intents: a step that needs the daemon is refused with
    /// the reason, visibly, never with a panic.
    #[tokio::test]
    async fn daemon_needing_intents_without_a_daemon_refuse_visibly() {
        let mut p = panel(SINGLE_WSH);
        for intent in [view::ClaimMessage::Recheck, view::ClaimMessage::Build] {
            let task = p.update(
                None,
                &Cache::default(),
                Message::View(view::Message::Claim(intent)),
            );
            let produced = outputs(task).await;
            assert!(
                matches!(
                    &produced[..],
                    [Message::View(view::Message::ShowError(reason))] if reason == NODE_UNAVAILABLE
                ),
                "{:?}",
                produced
            );
        }
    }

    /// Control for the test above: with the session intact the same
    /// confirmation submits (the full flow proves it), and `revoke` alone —
    /// what Cube lock, tab close and `Drop` call — is enough to refuse.
    #[tokio::test]
    async fn revoke_alone_refuses_the_submission() {
        let Flow {
            mut p,
            daemon,
            dyn_daemon,
            cache,
            root,
            sender: _sender,
            ..
        } = reach_review().await;
        p.revoke();
        let submit = p.update(
            Some(dyn_daemon.clone()),
            &cache,
            Message::View(view::Message::Claim(view::ClaimMessage::Confirm)),
        );
        let produced = outputs(submit).await;
        assert!(
            matches!(
                &produced[..],
                [Message::Claim(ClaimEvent::Submitted(_, Err(_)))]
            ),
            "{:?}",
            produced
        );
        assert!(!daemon.hits().contains(&"submit_verified_poison"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every stage renders: the view functions build their element trees for
    /// preconditions (checking, refused, ready), the review with and without
    /// a snapshot, and tracking with each outcome; the plan and the signing
    /// flow with the picker open are rendered inside `reach_signed`, on the
    /// way (Gandalf's reviewer probe: this test had only traversed them).
    /// Not a pixel test — a guard against a view that panics on a state the
    /// panel can reach.
    #[tokio::test]
    async fn every_stage_renders() {
        let menu = Menu::Vault(crate::app::menu::VaultSubMenu::Claim);
        let render = |p: &ClaimStep1Panel, cache: &Cache| {
            let _element = view::vault::claim::view(&menu, cache, p);
        };
        // Preconditions: nothing checked, then refused, then ready.
        let mut fresh = panel(SINGLE_WSH);
        let cache = Cache {
            network: Network::Bitcoin,
            fiat_chain: ChainId::Bitcoin,
            ..Cache::default()
        };
        render(&fresh, &cache);
        fresh.pre.target = Some("fork-cube".into());
        fresh.connect = Some(ConnectSession {
            client: CoincubeClient::new(),
            account: "7".into(),
        });
        let mut refused = checked_ok(1_000_000);
        refused.backend = Err("wrong backend".into());
        fresh.pre.checked = Some(refused);
        render(&fresh, &cache);
        fresh.pre.checked = Some(checked_ok(1_000_000));
        render(&fresh, &cache);

        // The live stages, reached through the real flow.
        let Flow {
            mut p,
            dyn_daemon,
            cache,
            root,
            ..
        } = reach_review().await;
        render(&p, &cache);
        // Review without a snapshot (a refused review) and while busy.
        if let Stage::Review {
            snapshot, error, ..
        } = &mut p.stage
        {
            *snapshot = None;
            *error = Some("What you reviewed has changed since.".into());
        }
        render(&p, &cache);
        if let Stage::Review { busy, .. } = &mut p.stage {
            *busy = true;
        }
        render(&p, &cache);
        // Tracking, each outcome, each status.
        let txid = Txid::from_byte_array([5; 32]);
        let wtxid = coincube_core::miniscript::bitcoin::Wtxid::from_byte_array([6; 32]);
        for outcome in [
            Outcome::UpstreamAccepted { txid, wtxid },
            Outcome::Uncertain { txid, wtxid },
        ] {
            for status in [
                None,
                Some(Status::Unchecked),
                Some(Status::Unavailable),
                Some(Status::Observation(Assessment::WaitingForConfirmation)),
                Some(Status::Observation(Assessment::WaitingForDepth {
                    confirmations: 2,
                })),
                Some(Status::Observation(Assessment::Reorged)),
                Some(Status::Observation(Assessment::Step1AlreadyOnFork)),
                Some(Status::Observation(
                    Assessment::ObservationsEligibleForPreflight,
                )),
            ] {
                let session = match std::mem::replace(&mut p.stage, Stage::Preconditions) {
                    Stage::Review { session, .. } | Stage::Track { session, .. } => session,
                    _ => None,
                };
                p.stage = Stage::Track {
                    session,
                    outcome,
                    status,
                    busy: false,
                    error: None,
                };
                render(&p, &cache);
            }
        }
        drop(dyn_daemon);
        let _ = std::fs::remove_dir_all(&root);
    }
}
