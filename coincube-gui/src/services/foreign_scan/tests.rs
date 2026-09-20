use super::*;
use bitcoin::{
    bip32::{Xpriv, Xpub},
    hashes::Hash,
    secp256k1::Secp256k1,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};
fn ranged() -> String {
    let secp = Secp256k1::new();
    let xpub = Xpub::from_priv(
        &secp,
        &Xpriv::new_master(bitcoin::Network::Bitcoin, &[42; 32]).unwrap(),
    );
    format!("wpkh({}/0/*)", xpub)
}
fn plan(end: u32) -> ScanPlan {
    ScanPlan {
        chain: ChainId::BitcoinBlake2b,
        branches: vec![BranchRange {
            descriptor: ScanDescriptor::parse(Branch::External, &ranged()).unwrap(),
            start: 0,
            end_exclusive: end,
        }],
        gap: 2,
    }
}
struct Fake {
    stats_calls: AtomicUsize,
    used_first: bool,
    changed: bool,
    tips: AtomicUsize,
    coins: Mutex<Vec<Utxo>>,
    previous: Option<Transaction>,
}
impl Fake {
    fn empty() -> Self {
        Self {
            stats_calls: AtomicUsize::new(0),
            used_first: false,
            changed: false,
            tips: AtomicUsize::new(0),
            coins: Mutex::new(vec![]),
            previous: None,
        }
    }
}
#[async_trait]
impl Source for Fake {
    async fn tip(&self, _: ChainId) -> Result<BlockHash, ScanError> {
        let n = self.tips.fetch_add(1, Ordering::SeqCst);
        Ok(BlockHash::from_byte_array(
            [if self.changed && n > 0 { 2 } else { 1 }; 32],
        ))
    }
    async fn anchor(&self) -> Result<BlockHash, ScanError> {
        Ok(BlockHash::from_byte_array([1; 32]))
    }
    async fn stats(&self, _: ChainId, _: &str) -> Result<Stats, ScanError> {
        let n = self.stats_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Stats {
            chain_stats: Count {
                tx_count: u64::from((self.used_first && n < 2) || self.previous.is_some()),
            },
            mempool_stats: Count { tx_count: 0 },
        })
    }
    async fn utxos(&self, _: ChainId, _: &str) -> Result<Vec<Utxo>, ScanError> {
        Ok(self.coins.lock().unwrap().clone())
    }
    async fn transaction(&self, _: ChainId, _: Txid) -> Result<Transaction, ScanError> {
        self.previous.clone().ok_or(ScanError::Unavailable)
    }
}
#[tokio::test]
async fn spent_history_is_not_an_unused_address_and_limits_are_incomplete() {
    let mut source = Fake::empty();
    source.used_first = true;
    let report = collect(&source, &plan(4), 7).await.unwrap();
    assert_eq!(report.addresses_scanned(), 3);
    assert!(report.coins().is_empty());
    assert_eq!(report.generation(), 7);
    let mut source = Fake::empty();
    source.used_first = true;
    assert!(matches!(
        collect(&source, &plan(2), 7).await,
        Err(ScanError::RangeLimit)
    ));
    let mut source = Fake::empty();
    source.changed = true;
    assert!(matches!(
        collect(&source, &plan(4), 7).await,
        Err(ScanError::Changed)
    ));
}
#[test]
fn descriptor_capabilities_are_scan_only_and_ambiguous_or_secret_paths_refuse() {
    let text = ranged();
    let public = text.trim_start_matches("wpkh(").trim_end_matches(')');
    for text in [
        text.clone(),
        format!("pkh({})", public),
        format!("sh(wpkh({}))", public),
        format!("tr({})", public),
        format!("wsh(pk({}))", public),
    ] {
        let d = ScanDescriptor::parse(Branch::External, &text).unwrap();
        assert_eq!(
            d.capabilities(),
            Capabilities {
                scan: true,
                unified_signing: false,
                claim_authorization: false
            }
        );
    }
    for text in [
        text.replace("/0/*", "/<0;1>/*"),
        text.replace("/0/*", "/0'/*"),
        text.replace("/0/*", "/0/*'"),
        "raw(51)".into(),
        "wpkh(not-a-key)".into(),
    ] {
        assert!(ScanDescriptor::parse(Branch::External, &text).is_err());
    }
    let secret = Xpriv::new_master(bitcoin::Network::Bitcoin, &[42; 32]).unwrap();
    assert!(ScanDescriptor::parse(Branch::External, &format!("wpkh({}/0/*)", secret)).is_err());
    let mut p = plan(3);
    p.chain = ChainId::BitcoinBlake2bTestnet4;
    assert_eq!(p.validate(), Err(ScanError::UnsupportedChain));
}
#[tokio::test]
async fn full_prevout_binding_and_duplicate_outpoints_refuse() {
    let mut p = plan(1);
    p.branches[0].descriptor =
        ScanDescriptor::parse(Branch::External, &ranged().replace("/*", "/0")).unwrap();
    p.gap = 1;
    let script = p.branches[0].descriptor.script(0).unwrap();
    let previous = Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![],
        output: vec![bitcoin::TxOut {
            value: bitcoin::Amount::from_sat(1000),
            script_pubkey: script,
        }],
    };
    let coin = Utxo {
        txid: previous.compute_txid(),
        vout: 0,
        value: 1000,
        status: Status {
            confirmed: false,
            block_height: None,
            block_hash: None,
        },
    };
    let mut source = Fake::empty();
    source.previous = Some(previous.clone());
    *source.coins.lock().unwrap() = vec![coin.clone()];
    assert_eq!(
        collect(&source, &p, 0).await.unwrap().coins()[0].previous,
        previous
    );
    source.coins.lock().unwrap()[0].value = 1001;
    assert!(matches!(
        collect(&source, &p, 0).await,
        Err(ScanError::Prevout)
    ));
    *source.coins.lock().unwrap() = vec![coin.clone(), coin];
    assert!(matches!(
        collect(&source, &p, 0).await,
        Err(ScanError::Malformed)
    ));
}
#[tokio::test]
async fn cancelled_generation_refuses_before_any_request() {
    let (tx, rx) = watch::channel(2);
    let client = CoincubeClient::for_test("http://127.0.0.1:1".to_owned());
    assert!(matches!(
        scan(client, plan(3), 1, rx).await,
        Err(ScanError::Cancelled)
    ));
    drop(tx);
}

#[tokio::test]
async fn http_fresh_address_privacy_no_fallback_and_failure_matrix() {
    use httpmock::prelude::*;
    for (status, markers, expected) in [
        (503, true, ScanError::Http(503)),
        (200, false, ScanError::Freshness),
        (200, true, ScanError::Malformed),
    ] {
        let server = MockServer::start();
        let mut client = CoincubeClient::for_test(server.base_url());
        client.set_token("synthetic-scan-token");
        let source = http::HttpSource::new(client).unwrap();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/esplora/bitcoin-blake2b/mainnet/address/abc")
                .header("x-coincube-observation", "fresh")
                .matches(|r| {
                    r.headers.as_ref().is_none_or(|hs| {
                        hs.iter().all(|(n, _)| {
                            ![
                                "authorization",
                                "cookie",
                                "x-device-fingerprint",
                                "x-device-name",
                            ]
                            .iter()
                            .any(|bad| n.eq_ignore_ascii_case(bad))
                        })
                    })
                });
            let then = then.status(status).body("malformed");
            if markers {
                then.header("X-Coincube-Observation", "fresh")
                    .header("X-Cache", "BYPASS")
                    .header("Cache-Control", "no-store");
            }
        });
        assert_eq!(
            source.stats(ChainId::BitcoinBlake2b, "abc").await,
            Err(expected)
        );
        mock.assert();
    }
}
#[tokio::test]
async fn http_complete_bounded_scan_and_inflight_generation_cancel() {
    use httpmock::prelude::*;
    let server = MockServer::start();
    let p = plan(3);
    let tip = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/esplora/bitcoin/mainnet/blocks/tip/hash");
        then.status(200)
            .header("X-Coincube-Observation", "fresh")
            .header("X-Cache", "BYPASS")
            .header("Cache-Control", "no-store")
            .body("11".repeat(32));
    });
    let mut mocks = Vec::new();
    for index in 0..2 {
        let addr = bitcoin::Address::from_script(
            &p.branches[0].descriptor.script(index).unwrap(),
            bitcoin::Network::Bitcoin,
        )
        .unwrap()
        .to_string();
        for (suffix, body) in [
            (
                "",
                r#"{"chain_stats":{"tx_count":0},"mempool_stats":{"tx_count":0}}"#,
            ),
            ("/utxo", "[]"),
        ] {
            mocks.push(server.mock(|when, then| {
                when.method(GET).path(format!(
                    "/api/v1/esplora/bitcoin/mainnet/address/{}{}",
                    addr, suffix
                ));
                then.status(200)
                    .header("X-Coincube-Observation", "fresh")
                    .header("X-Cache", "BYPASS")
                    .header("Cache-Control", "no-store")
                    .body(body);
            }));
        }
    }
    let mut client = CoincubeClient::for_test(server.base_url());
    client.set_token("synthetic");
    let (tx, rx) = watch::channel(1);
    let mut p = p;
    p.chain = ChainId::Bitcoin;
    let result = scan(client.clone(), p, 1, rx).await.unwrap();
    assert_eq!(result.addresses_scanned(), 2);
    tip.assert_hits(2);
    for m in mocks {
        m.assert_hits(2);
    }
    // Cancel an actively polled network operation, not an unpolled future.
    let delayed = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/esplora/bitcoin-blake2b/mainnet/blocks/tip/hash");
        then.status(200)
            .delay(Duration::from_secs(10))
            .body("11".repeat(32));
    });
    let task = tokio::spawn(scan(client, plan(3), 1, tx.subscribe()));
    for _ in 0..100 {
        if delayed.hits() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(delayed.hits(), 1);
    tx.send(2).unwrap();
    assert!(matches!(task.await.unwrap(), Err(ScanError::Cancelled)));
}

#[tokio::test]
async fn response_limits_and_redirects_refuse_without_following() {
    use httpmock::prelude::*;
    let server = MockServer::start();
    let mut client = CoincubeClient::for_test(server.base_url());
    client.set_token("synthetic");
    let source = http::HttpSource::new(client).unwrap();
    let oversized = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/esplora/bitcoin/mainnet/address/large");
        then.status(200)
            .header("X-Coincube-Observation", "fresh")
            .header("X-Cache", "BYPASS")
            .header("Cache-Control", "no-store")
            .body(" ".repeat(2 * 1024 * 1024 + 1));
    });
    assert_eq!(
        source.stats(ChainId::Bitcoin, "large").await,
        Err(ScanError::BodyLimit)
    );
    oversized.assert();
    let target = server.mock(|when, then| {
        when.method(GET).path("/leak");
        then.status(200);
    });
    let redirect = server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/esplora/bitcoin/mainnet/address/redirect");
        then.status(302)
            .header("Location", format!("{}/leak", server.base_url()));
    });
    assert_eq!(
        source.stats(ChainId::Bitcoin, "redirect").await,
        Err(ScanError::Http(302))
    );
    redirect.assert();
    target.assert_hits(0);
}
