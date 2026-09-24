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
