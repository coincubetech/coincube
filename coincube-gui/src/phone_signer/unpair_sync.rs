//! Keeps pairing removals in step between this desktop and its phones.
//!
//! Removing a pairing takes effect locally first, on either side. The other
//! side is then told with an `Unpaired` frame over the pinned TLS channel:
//!
//! - desktop → phone: [`deliver_pending`] dials each phone with a queued
//!   [`PendingUnpair`] and sends the frame. A phone that is asleep or off the
//!   network keeps its notice queued until it is reachable again.
//! - phone → desktop: the phone only listens, so it can't reach this desktop.
//!   It answers the desktop's next connection with `Unpaired` instead. The
//!   Pair panel asks with [`probe`]; on the signing screens the hw refresh
//!   loop learns it from the signer's reader
//!   ([`super::PhoneSigner::peer_unpaired`]).
//!
//! Neither direction grants anything: the frame only ends a pairing, which
//! either side can already do alone.

use std::net::SocketAddr;
use std::time::Duration;

use coincube_core::miniscript::bitcoin::bip32::Fingerprint;

use crate::dir::CoincubeDirectory;
use crate::phone_signer::identity::{self, DesktopIdentity};
use crate::phone_signer::mdns::DiscoveredPhone;
use crate::phone_signer::pairing_store::{self, PairingStoreFile};
use crate::phone_signer::protocol::{local_v1, unpaired_envelope, LocalEnvelope};
use crate::phone_signer::transport::{PairedReader, PairedTransport, PairedWriter};

/// How long [`probe`] waits for the phone's answer to its `Ping`. A phone
/// that removed this desktop answers at once, before reading anything.
const PROBE_REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a finished exchange waits for the phone's side of the close.
/// Reading to the end before dropping the socket stops the OS from resetting
/// a connection with unread bytes, which can discard a frame the phone has
/// not read yet.
const CLOSE_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

/// What a paired phone said when asked whether it still has this desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The phone answered and still has this pairing.
    Paired,
    /// The phone removed this pairing.
    Unpaired,
    /// No answer: offline, asleep, or a build that predates `Unpaired`.
    /// Says nothing about the pairing.
    Unreachable,
}

/// The result of one [`sync`] pass, for the Pair panel.
#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    /// Names of phones that had removed this desktop. Their rows are gone.
    pub removed_by_phone: Vec<String>,
}

/// Where to dial a phone: the address it advertises over mDNS, else the
/// user-entered `host:port` fallback for networks that block mDNS.
pub(crate) fn resolve_target(
    fp8: &str,
    fallback_addr: Option<&str>,
    discovered: &[DiscoveredPhone],
) -> Option<SocketAddr> {
    discovered
        .iter()
        .find(|d| d.cert_fp8 == fp8)
        .map(|d| d.addr)
        .or_else(|| fallback_addr.and_then(|s| s.parse().ok()))
}

/// Tell one phone that this desktop removed it. `Ok` once the frame has been
/// written, which is all the phone needs.
pub async fn notify(
    target: SocketAddr,
    identity: &DesktopIdentity,
    phone_pin: [u8; 32],
) -> Result<(), async_hwi::Error> {
    let transport = PairedTransport::connect(target, identity, phone_pin).await?;
    let (reader, mut writer) = transport.split();
    writer
        .send(&unpaired_envelope("removed on desktop"))
        .await?;
    close(reader, writer).await;
    Ok(())
}

/// Ask one phone whether it still has this desktop.
pub async fn probe(
    target: SocketAddr,
    identity: &DesktopIdentity,
    phone_pin: [u8; 32],
) -> ProbeOutcome {
    let Ok(transport) = PairedTransport::connect(target, identity, phone_pin).await else {
        return ProbeOutcome::Unreachable;
    };
    let (mut reader, mut writer) = transport.split();
    // A phone that removed us may already have closed its end, so a failed
    // write still leaves its `Unpaired` frame waiting to be read.
    let _ = writer.send(&ping_envelope()).await;
    let outcome = match tokio::time::timeout(PROBE_REPLY_TIMEOUT, reader.recv()).await {
        Ok(Ok(LocalEnvelope {
            payload: Some(local_v1::local_envelope::Payload::Unpaired(_)),
        })) => ProbeOutcome::Unpaired,
        Ok(Ok(_)) => ProbeOutcome::Paired,
        Ok(Err(_)) | Err(_) => ProbeOutcome::Unreachable,
    };
    close(reader, writer).await;
    outcome
}

fn ping_envelope() -> LocalEnvelope {
    LocalEnvelope {
        payload: Some(local_v1::local_envelope::Payload::Ping(
            crate::services::connect::grpc::connect_v1::Ping {
                ts_unix_ms: (pairing_store::now_unix() * 1000) as i64,
            },
        )),
    }
}

async fn close(mut reader: PairedReader, mut writer: PairedWriter) {
    let _ = writer.shutdown().await;
    let _ = tokio::time::timeout(CLOSE_DRAIN_TIMEOUT, async {
        while reader.recv().await.is_ok() {}
    })
    .await;
}

/// Deliver every queued removal whose phone can be reached, and forget the
/// ones that no longer apply: delivered, past their TTL, or superseded by
/// pairing the same phone again. Notices for unreachable phones stay queued.
pub async fn deliver_pending(
    dir: &CoincubeDirectory,
    identity: &DesktopIdentity,
    store: &PairingStoreFile,
    discovered: &[DiscoveredPhone],
) {
    if store.pending_unpairs.is_empty() {
        return;
    }
    let now = pairing_store::now_unix();
    let mut done = Vec::new();
    let mut sends = Vec::new();
    for pending in &store.pending_unpairs {
        let key = (pending.cert_pin, pending.removed_at_unix);
        if pending.is_expired(now) || store.phones.iter().any(|p| p.cert_pin == pending.cert_pin) {
            done.push(key);
            continue;
        }
        let fp8 = identity::pin_hex8(&pending.cert_pin);
        let Some(target) = resolve_target(&fp8, pending.fallback_addr.as_deref(), discovered)
        else {
            continue;
        };
        sends.push(async move {
            let res = notify(target, identity, pending.cert_pin).await;
            (key, &pending.name, res)
        });
    }
    for (key, name, res) in iced::futures::future::join_all(sends).await {
        match res {
            Ok(()) => {
                tracing::debug!("told {} it was removed from this desktop", name);
                done.push(key);
            }
            Err(e) => tracing::debug!("removal notice to {} not delivered yet: {}", name, e),
        }
    }
    if let Err(e) = pairing_store::clear_pending_unpairs(dir, &done) {
        tracing::warn!("could not update queued phone removals: {}", e);
    }
}

/// Ask each phone paired for `vault` whether it still has this desktop, and
/// drop the pairing of every one that says it doesn't. Returns their names.
pub async fn remove_phones_that_unpaired(
    dir: &CoincubeDirectory,
    identity: &DesktopIdentity,
    store: &PairingStoreFile,
    vault: Fingerprint,
    discovered: &[DiscoveredPhone],
) -> Vec<String> {
    let probes = store
        .phones
        .iter()
        .filter(|p| p.vault_fingerprint == vault)
        .filter_map(|phone| {
            let fp8 = identity::pin_hex8(&phone.cert_pin);
            let target = resolve_target(&fp8, phone.fallback_addr.as_deref(), discovered)?;
            Some(async move { (phone, probe(target, identity, phone.cert_pin).await) })
        });
    let mut removed = Vec::new();
    for (phone, outcome) in iced::futures::future::join_all(probes).await {
        if outcome != ProbeOutcome::Unpaired {
            continue;
        }
        match pairing_store::remove_unpaired_by_peer(dir, &phone.cert_pin, phone.paired_at_unix) {
            Ok(true) => removed.push(phone.name.clone()),
            Ok(false) => {}
            Err(e) => tracing::warn!("could not remove {} after it unpaired: {}", phone.name, e),
        }
    }
    removed
}

/// One Pair panel pass: deliver queued removals, then, when a vault is
/// loaded, ask its paired phones whether they still have this desktop.
pub async fn sync(
    dir: CoincubeDirectory,
    vault: Option<Fingerprint>,
) -> Result<SyncReport, String> {
    let store = pairing_store::load(&dir).map_err(|e| e.to_string())?;
    let probing = vault.is_some_and(|v| store.phones.iter().any(|p| p.vault_fingerprint == v));
    if store.pending_unpairs.is_empty() && !probing {
        return Ok(SyncReport::default());
    }
    let identity = identity::load_or_create(&dir).map_err(|e| e.to_string())?;
    let discovered = crate::phone_signer::mdns::browse();
    deliver_pending(&dir, &identity, &store, &discovered).await;
    let removed_by_phone = match vault {
        Some(vault) => {
            remove_phones_that_unpaired(&dir, &identity, &store, vault, &discovered).await
        }
        None => Vec::new(),
    };
    Ok(SyncReport { removed_by_phone })
}

#[cfg(test)]
mod tests {
    //! Both directions over a real loopback TLS session, with this module's
    //! pinned dial against a minimal "phone" that answers one connection.
    use super::*;
    use crate::phone_signer::pairing_store::{PairedPhone, PendingUnpair};
    use crate::phone_signer::tls;
    use prost::Message as _;
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::net::Ipv4Addr;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    fn fresh_dir() -> CoincubeDirectory {
        let mut path = std::env::temp_dir();
        path.push(format!("coincube-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("mkdir tempdir");
        CoincubeDirectory::new(path)
    }

    fn mint() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let kp = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("keygen");
        let cert = CertificateParams::new(vec!["coincube-phone.local".to_string()])
            .expect("params")
            .self_signed(&kp)
            .expect("self-sign");
        (
            cert.der().clone(),
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(kp.serialize_der())),
        )
    }

    /// How the fake phone treats the one connection it accepts.
    #[derive(Clone, Copy)]
    enum Phone {
        /// Still paired: answer a `Ping` with a `Pong`.
        Paired,
        /// Removed this desktop: send `Unpaired` first, then close.
        Unpaired,
    }

    /// A one-connection fake phone. Returns its address, its cert pin, and a
    /// handle resolving to every frame the desktop sent it.
    async fn phone(
        mode: Phone,
    ) -> (
        SocketAddr,
        [u8; 32],
        tokio::task::JoinHandle<Vec<LocalEnvelope>>,
    ) {
        let (cert, key) = mint();
        let pin = tls::fingerprint_of(&cert);
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cfg = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .expect("server cert");
        let acceptor = TlsAcceptor::from(Arc::new(cfg));
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("accept");
            // A desktop that refuses this phone's certificate aborts here.
            let Ok(mut tls) = acceptor.accept(tcp).await else {
                return Vec::new();
            };
            if let Phone::Unpaired = mode {
                write_frame(&mut tls, &unpaired_envelope("removed on phone")).await;
            }
            let mut received = Vec::new();
            loop {
                let mut len = [0u8; 4];
                if tls.read_exact(&mut len).await.is_err() {
                    break;
                }
                let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
                if tls.read_exact(&mut body).await.is_err() {
                    break;
                }
                let env = LocalEnvelope::decode(body.as_slice()).expect("decode");
                if let (Phone::Paired, Some(local_v1::local_envelope::Payload::Ping(_))) =
                    (mode, &env.payload)
                {
                    let pong = LocalEnvelope {
                        payload: Some(local_v1::local_envelope::Payload::Pong(
                            crate::services::connect::grpc::connect_v1::Pong { ts_unix_ms: 0 },
                        )),
                    };
                    write_frame(&mut tls, &pong).await;
                }
                received.push(env);
            }
            let _ = tls.shutdown().await;
            received
        });
        (addr, pin, handle)
    }

    async fn write_frame<S: AsyncWriteExt + Unpin>(tls: &mut S, env: &LocalEnvelope) {
        let body = env.encode_to_vec();
        tls.write_all(&(body.len() as u32).to_be_bytes())
            .await
            .expect("len");
        tls.write_all(&body).await.expect("body");
        tls.flush().await.expect("flush");
    }

    fn discovered(pin: &[u8; 32], addr: SocketAddr) -> DiscoveredPhone {
        DiscoveredPhone {
            cert_fp8: identity::pin_hex8(pin),
            addr,
            instance_name: "keychain-test".into(),
        }
    }

    fn paired(pin: [u8; 32], vault: Fingerprint) -> PairedPhone {
        PairedPhone {
            signer_binding: None,
            cert_pin: pin,
            name: "iPhone".into(),
            paired_at_unix: 1_700_000_000,
            wallet_fingerprints: Vec::new(),
            vault_fingerprint: vault,
            transport_pubkey: Vec::new(),
            fallback_addr: None,
        }
    }

    /// A port nothing listens on: bound, then released.
    async fn dead_addr() -> SocketAddr {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind");
        listener.local_addr().expect("addr")
    }

    #[test]
    fn target_prefers_mdns_and_falls_back_to_the_manual_address() {
        let pin = [1u8; 32];
        let mdns: SocketAddr = "192.0.2.1:1000".parse().unwrap();
        let manual = "192.0.2.2:2000";
        let fp8 = identity::pin_hex8(&pin);
        assert_eq!(
            resolve_target(&fp8, Some(manual), &[discovered(&pin, mdns)]),
            Some(mdns)
        );
        assert_eq!(
            resolve_target(&fp8, Some(manual), &[]),
            Some(manual.parse().unwrap())
        );
        assert_eq!(resolve_target(&fp8, Some("not an address"), &[]), None);
        assert_eq!(resolve_target(&fp8, None, &[]), None);
    }

    #[tokio::test]
    async fn notify_delivers_an_unpaired_frame() {
        let dir = fresh_dir();
        let desktop = identity::load_or_create(&dir).expect("identity");
        let (addr, pin, phone) = phone(Phone::Paired).await;
        notify(addr, &desktop, pin).await.expect("delivered");
        let received = phone.await.expect("phone");
        assert!(
            matches!(
                received.as_slice(),
                [LocalEnvelope {
                    payload: Some(local_v1::local_envelope::Payload::Unpaired(_))
                }]
            ),
            "{:?}",
            received
        );
    }

    #[tokio::test]
    async fn notify_refuses_a_phone_presenting_another_certificate() {
        let dir = fresh_dir();
        let desktop = identity::load_or_create(&dir).expect("identity");
        let (addr, _pin, _phone) = phone(Phone::Paired).await;
        assert!(notify(addr, &desktop, [9u8; 32]).await.is_err());
    }

    #[tokio::test]
    async fn probe_tells_paired_unpaired_and_unreachable_apart() {
        let dir = fresh_dir();
        let desktop = identity::load_or_create(&dir).expect("identity");

        let (addr, pin, phone_task) = phone(Phone::Paired).await;
        assert_eq!(probe(addr, &desktop, pin).await, ProbeOutcome::Paired);
        phone_task.await.expect("phone");

        let (addr, pin, phone_task) = phone(Phone::Unpaired).await;
        assert_eq!(probe(addr, &desktop, pin).await, ProbeOutcome::Unpaired);
        phone_task.await.expect("phone");

        assert_eq!(
            probe(dead_addr().await, &desktop, [3u8; 32]).await,
            ProbeOutcome::Unreachable
        );
    }

    #[tokio::test]
    async fn queued_removal_is_delivered_once_the_phone_is_reachable() {
        let dir = fresh_dir();
        let desktop = identity::load_or_create(&dir).expect("identity");
        let vault = Fingerprint::from([1, 2, 3, 4]);
        let (addr, pin, phone_task) = phone(Phone::Paired).await;
        pairing_store::upsert(&dir, paired(pin, vault)).expect("pair");
        pairing_store::unpair(&dir, &pin).expect("unpair");

        // Not reachable yet: the notice stays queued.
        let store = pairing_store::load(&dir).expect("load");
        deliver_pending(&dir, &desktop, &store, &[]).await;
        assert_eq!(pairing_store::load(&dir).unwrap().pending_unpairs.len(), 1);

        // Reachable: delivered and forgotten.
        deliver_pending(&dir, &desktop, &store, &[discovered(&pin, addr)]).await;
        assert!(pairing_store::load(&dir)
            .unwrap()
            .pending_unpairs
            .is_empty());
        let received = phone_task.await.expect("phone");
        assert!(matches!(
            received.first().and_then(|e| e.payload.as_ref()),
            Some(local_v1::local_envelope::Payload::Unpaired(_))
        ));
    }

    #[tokio::test]
    async fn stale_and_superseded_removals_are_dropped_without_dialling() {
        let dir = fresh_dir();
        let desktop = identity::load_or_create(&dir).expect("identity");
        let vault = Fingerprint::from([1, 2, 3, 4]);
        let expired = PendingUnpair {
            cert_pin: [5u8; 32],
            name: "old".into(),
            fallback_addr: None,
            removed_at_unix: 0,
        };
        let repaired = PendingUnpair {
            cert_pin: [6u8; 32],
            name: "again".into(),
            fallback_addr: None,
            removed_at_unix: pairing_store::now_unix(),
        };
        let store = PairingStoreFile {
            phones: vec![paired([6u8; 32], vault)],
            pending_unpairs: vec![expired, repaired],
        };
        pairing_store::save(&dir, &store).expect("save");
        deliver_pending(&dir, &desktop, &store, &[]).await;
        assert!(pairing_store::load(&dir)
            .unwrap()
            .pending_unpairs
            .is_empty());
    }

    #[tokio::test]
    async fn phone_that_unpaired_is_removed_and_others_are_kept() {
        let dir = fresh_dir();
        let desktop = identity::load_or_create(&dir).expect("identity");
        let vault = Fingerprint::from([1, 2, 3, 4]);
        let (gone_addr, gone_pin, gone) = phone(Phone::Unpaired).await;
        let (kept_addr, kept_pin, kept) = phone(Phone::Paired).await;
        let offline_pin = [7u8; 32];
        for pin in [gone_pin, kept_pin, offline_pin] {
            pairing_store::upsert(&dir, paired(pin, vault)).expect("pair");
        }
        let store = pairing_store::load(&dir).expect("load");
        let removed = remove_phones_that_unpaired(
            &dir,
            &desktop,
            &store,
            vault,
            &[
                discovered(&gone_pin, gone_addr),
                discovered(&kept_pin, kept_addr),
            ],
        )
        .await;
        assert_eq!(removed, vec!["iPhone".to_string()]);
        let pins: Vec<_> = pairing_store::load(&dir)
            .unwrap()
            .phones
            .iter()
            .map(|p| p.cert_pin)
            .collect();
        assert_eq!(pins, vec![kept_pin, offline_pin]);
        gone.await.expect("phone");
        kept.await.expect("phone");
    }
}
