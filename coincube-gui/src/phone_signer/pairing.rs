//! Pairing-offer generation, encoding/decoding, and expiry helpers.
//!
//! Authoritative QR payload: base64url(JSON) of [`PairingOffer`]. The
//! Flutter side decodes the same shape; any field rename here must be
//! mirrored there. See
//! `plans/PLAN-local-signer-lan-interop-fixes-desktop.md` §1.2 for
//! the locked wire contract.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;

use coincube_core::miniscript::bitcoin::bip32::Fingerprint;

use crate::phone_signer::identity::DesktopIdentity;

/// Version tag for the pairing-offer payload. Bumped whenever the
/// JSON fields below change in a non-backwards-compatible way.
///
/// v2 added the per-offer `psk` and made the phone's
/// `pairing_proof` (a channel-bound HMAC of that psk) mandatory, so
/// the desktop can authenticate the phone it dialed instead of
/// trusting whatever cert answered on the LAN. See
/// `plans/PLAN-local-signer-pairing-phone-auth.md`.
///
/// v3 (signing identity contract §4.1) adds `net`, the Vault's chain, and
/// binds it into the proof. This desktop emits v3 only, for every chain;
/// a phone that predates v3 refuses the code at scan.
pub const PAIRING_PROTOCOL_VERSION: u32 = 3;
/// Resource ceiling shared with Keychain: 283 bytes below QR version 40,
/// medium ECC byte capacity. The production budget test reserves more margin.
/// This is not a physical-camera scanning guarantee.
pub const MAX_PAIRING_QR_BYTES: usize = 2048;

/// Length of the per-offer pairing secret, in bytes. 128-bit: the
/// secret is online, single-use, and TTL-bounded, so this is well
/// past any brute-force concern (22 base64url chars in the QR).
///
/// **Locked at 16 to match the keychain-app**, whose v2 QR parser
/// rejects any psk that doesn't decode to exactly this many bytes.
/// Changing it is a wire-breaking change — update both sides together.
pub const PAIRING_PSK_LEN: usize = 16;

/// Domain-separation prefix for the proof-of-QR-scan HMAC. The `-v3`
/// suffix is part of the signed message: bump it (in lockstep with
/// the phone) if the construction in [`pairing_proof`] ever changes.
/// It is the only domain either app computes or verifies (contract §4.2):
/// no v2 offer is read, so the v2 domain is not kept.
const PAIRING_PROOF_DOMAIN: &[u8] = b"coincube-pair-v3";

/// JSON key of the QR offer's optional pairing-code marker. See
/// [`encode_offer_with_pairing_code`].
pub const PAIRING_CODE_KEY: &str = "pc";

/// The pairing-code format this desktop shows on its pairing screen and
/// announces in the offer's optional `pc` key.
///
/// Format 1: the first 8 characters of the desktop certificate fingerprint
/// (`certFp`, the SHA-256 of the certificate DER as 64 lowercase hex
/// characters), in upper case, as two groups of four separated by one space:
/// `A1B2 C3D4`. See [`pairing_code`].
///
/// The code is a display of a value both sides already hold. It is not part
/// of the pairing proof, and nothing in pairing depends on the user comparing
/// it: the certificate pin and the proof decide whether a pairing completes.
///
/// What a match is worth: it rules out the phone having read some other
/// computer's QR by accident. It does not rule out a deliberately substituted
/// QR. Eight hex characters are 32 bits, so a QR can carry a certificate
/// chosen to share them (about 2^32 tries, offline). The comparison is
/// therefore a convenience for the user and must never be treated as a
/// control.
pub const PAIRING_CODE_FORMAT: u32 = 1;

/// Default lifetime of a pairing offer. Long enough to scan from
/// across the room; short enough that an abandoned QR can't be
/// reused hours later.
pub const PAIRING_OFFER_TTL_SECONDS: u64 = 120;

/// Payload encoded inside the QR code shown by the desktop. The
/// phone decodes (base64url → JSON), trusts the embedded `cert`
/// after verifying it hashes to `certFp`, uses `svc` to confirm the
/// QR was generated for it, and uses `wfp` + `exp` to gate the
/// pairing flow.
///
/// Shared field meanings; Rust and Dart JSON field ordering may differ.
/// The HMAC covers the specified fields, not the serialized JSON. See
/// `plans/PLAN-local-signer-lan-cert-in-qr-desktop.md` §1.1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingOffer {
    /// Explicitly selected spendable vault xpub, never an ownership claim.
    #[serde(default, rename = "key")]
    pub signer_xpub: String,
    /// Full hash over the canonical descriptor held by this desktop.
    #[serde(default, rename = "dh")]
    pub descriptor_sha256: String,
    /// The Vault's chain as its Connect network id
    /// (`Wallet.chain.api_str()`, canonical spelling). Added in v3 and
    /// covered by the pairing proof, so a phone that answers for another
    /// chain fails verification. Empty only on an offer this desktop did
    /// not generate, which the listener refuses.
    #[serde(default, rename = "net")]
    pub network: String,
    /// Protocol version: [`PAIRING_PROTOCOL_VERSION`] (3) on every offer
    /// this desktop generates.
    #[serde(rename = "v")]
    pub version: u32,

    /// Base64url(no padding) of the desktop's self-signed cert DER.
    /// The phone verifies `sha256(decoded) == cert_fp` before
    /// trusting; the desktop sources both fields from the same
    /// [`DesktopIdentity::cert_der`] so they're guaranteed to agree.
    #[serde(rename = "cert")]
    pub cert_der_b64: String,

    /// SHA-256 of the desktop's self-signed cert DER, lowercase hex
    /// (64 chars). Redundant with `cert` but kept for two reasons:
    /// (1) the phone can short-circuit if it doesn't recognise the
    /// hash from a previous pairing, (2) the QR is shorter to log.
    #[serde(rename = "certFp")]
    pub cert_fp: String,

    /// Phone's mDNS instance name (the one it advertises under
    /// `_coincube-signer._tcp.local.`). The desktop resolved this
    /// from mDNS just before generating the offer and embeds it as a
    /// UX hint so the phone can confirm "this QR was meant for me".
    #[serde(rename = "svc")]
    pub service_name: String,

    /// Master fingerprint of the wallet we want this phone to sign
    /// for. Persisted on the resulting `PairedPhone` so reconnects
    /// can match by wallet.
    #[serde(rename = "wfp")]
    pub wallet_fingerprint: Fingerprint,

    /// Pairing-offer expiry, in unix seconds. Past this point the
    /// desktop refuses to act on the offer and the phone is expected
    /// to surface "offer expired".
    #[serde(rename = "exp")]
    pub expires_at_unix: u64,

    /// Per-offer pairing secret: [`PAIRING_PSK_LEN`] CSPRNG bytes,
    /// base64url(no pad). Generated fresh per offer, never persisted,
    /// never logged. The phone learns it only by scanning the QR and
    /// returns proof of knowledge via `PairingComplete.pairing_proof`
    /// (see [`pairing_proof`]). This is the desktop's sole evidence
    /// that the peer it dialed is the device that scanned the QR —
    /// without it the unpinned dial trusts whatever answered on the
    /// LAN. Added in protocol v2.
    #[serde(rename = "psk")]
    pub psk_b64: String,
}

/// This computer's pairing code as both apps show it, in
/// [`PAIRING_CODE_FORMAT`] 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCode {
    /// `A1B2 C3D4`: the first 8 characters of the fingerprint.
    pub short: String,
    /// The whole fingerprint in the same style: 16 groups of four upper-case
    /// characters separated by single spaces. `short` is its first two
    /// groups.
    pub full: String,
}

/// Format a desktop certificate fingerprint (`certFp`: 64 lowercase hex
/// characters) as the pairing code. `None` for anything else, so a malformed
/// fingerprint is never shown as a code.
pub fn pairing_code(cert_fp: &str) -> Option<PairingCode> {
    let well_formed = cert_fp.len() == 64
        && cert_fp
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !well_formed {
        return None;
    }
    let upper = cert_fp.to_ascii_uppercase();
    let groups: Vec<&str> = (0..upper.len())
        .step_by(4)
        .map(|start| &upper[start..start + 4])
        .collect();
    Some(PairingCode {
        short: groups[..2].join(" "),
        full: groups.join(" "),
    })
}

/// A freshly minted pairing offer. The offer carries a per-offer
/// `psk` (protocol v2) that the phone proves knowledge of, so the
/// generated artifact is just the offer itself — the secret travels
/// inside it.
#[derive(Debug)]
pub struct GeneratedOffer {
    pub offer: PairingOffer,
}

/// The vault key an offer commits to: the spendable xpub the user picked
/// and the hash of the descriptor it belongs to.
///
/// Bundled so [`generate_offer`] can require both without growing a third
/// bare `String` parameter. `PairingOffer`'s own fields stay public and
/// `#[serde(default)]` — a decoded v1 QR legitimately has them empty, which
/// is what `run_pairing` rejects — so this constrains offers this desktop
/// *generates*, not every offer that can exist.
#[derive(Debug, Clone, Default)]
pub struct OfferedKey {
    pub signer_xpub: String,
    pub descriptor_sha256: String,
}

/// Generate a fresh pairing offer aimed at a specific phone.
///
/// Takes `&DesktopIdentity` directly so `cert` and `certFp` are
/// guaranteed to derive from the same `cert_der` — the phone's
/// agreement check can't fail because the desktop split the two
/// fields across separate sources.
///
/// `phone_service_name` is the mDNS instance name resolved from a
/// `_coincube-signer._tcp.local.` browse just before this call —
/// embedded so the phone can confirm receipt of the right QR.
///
/// `key` is required rather than filled in by the caller afterwards:
/// [`crate::phone_signer::pairing_listener::run_pairing`] refuses an offer
/// carrying an empty `signer_xpub`, so an offer built without one is dead on
/// arrival. Passing it here means that can't be forgotten. (It stays a
/// *struct* rather than two more `String` arguments so it can't be
/// transposed with `phone_service_name` — all three are strings, and a swap
/// would compile.)
/// `chain` is the loaded Vault's chain; its Connect network id becomes the
/// offer's `net` (v3).
pub fn generate_offer(
    wallet_fingerprint: Fingerprint,
    identity: &DesktopIdentity,
    phone_service_name: String,
    key: OfferedKey,
    chain: crate::chain::ChainId,
) -> GeneratedOffer {
    let expires_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        + PAIRING_OFFER_TTL_SECONDS;

    // Fresh CSPRNG secret per offer. `OsRng` pulls straight from the
    // platform entropy source — never seed this from anything
    // predictable, the whole proof rests on the phone being the only
    // other party that learns it.
    let mut psk = [0u8; PAIRING_PSK_LEN];
    rand::rngs::OsRng.fill_bytes(&mut psk);

    GeneratedOffer {
        offer: PairingOffer {
            signer_xpub: key.signer_xpub,
            descriptor_sha256: key.descriptor_sha256,
            network: chain.api_str().to_owned(),
            version: PAIRING_PROTOCOL_VERSION,
            cert_der_b64: identity.cert_der_b64(),
            cert_fp: identity.cert_fp(),
            service_name: phone_service_name,
            wallet_fingerprint,
            expires_at_unix,
            psk_b64: URL_SAFE_NO_PAD.encode(psk),
        },
    }
}

/// Compute the proof-of-QR-scan: lowercase-hex `HMAC-SHA256` over the
/// domain tag, BOTH cert fingerprints and the offer's chain, keyed by the
/// offer's per-offer secret. The phone computes this after scanning the QR
/// and returns it in `PairingComplete.pairing_proof`; the desktop
/// recomputes it to authenticate the phone before pinning.
///
/// Construction (MUST stay byte-for-byte in lockstep with the phone —
/// see `grpc/local_envelope.proto` `PairingComplete.pairing_proof`):
///
/// ```text
/// key = base64url_no_pad_decode(psk_b64)
/// msg = "coincube-pair-v3" || desktop_cert_fp || phone_cert_fp || net   (raw bytes, no separators)
/// proof = lowercase_hex(HMAC-SHA256(key, msg))
/// ```
///
/// Both fingerprints are the 64-char lowercase-hex SHA-256 of the
/// respective self-signed cert DER. `desktop_cert_fp` is the value
/// embedded in the QR (`PairingOffer.cert_fp`); `phone_cert_fp` is
/// the phone's own cert fp. Binding the MAC to both pins is what
/// defeats a relay/substitution attacker: a peer that terminates TLS
/// with a different cert produces a proof over a different
/// `phone_cert_fp` than the desktop expects. `net` is the offer's chain
/// (UTF-8, the canonical Connect network id), so a phone that computed the
/// proof over another chain string fails verification (contract §4.2).
///
/// Returns `Err` only when `psk_b64` isn't valid base64url.
pub fn pairing_proof(
    psk_b64: &str,
    desktop_cert_fp: &str,
    phone_cert_fp: &str,
    net: &str,
) -> Result<String, String> {
    let psk = URL_SAFE_NO_PAD
        .decode(psk_b64)
        .map_err(|e| format!("decode psk: {}", e))?;
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &psk);
    let mut msg = Vec::with_capacity(
        PAIRING_PROOF_DOMAIN.len() + desktop_cert_fp.len() + phone_cert_fp.len() + net.len(),
    );
    msg.extend_from_slice(PAIRING_PROOF_DOMAIN);
    msg.extend_from_slice(desktop_cert_fp.as_bytes());
    msg.extend_from_slice(phone_cert_fp.as_bytes());
    msg.extend_from_slice(net.as_bytes());
    let tag = ring::hmac::sign(&key, &msg);
    Ok(tag.as_ref().iter().map(|b| format!("{:02x}", b)).collect())
}

/// Verify a phone-reported `pairing_proof` in constant time. Returns
/// `true` only when the recomputed proof matches `reported_proof`
/// exactly; `false` on any mismatch, an empty/short report (e.g. a v1
/// phone that doesn't send the field), or a malformed psk.
///
/// `phone_cert_fp` MUST be the fingerprint the desktop captured from
/// the live TLS handshake (the cert it is about to pin), not the
/// phone-reported `phone_cert_fp` string — otherwise the binding is
/// to a value the peer controls and the check proves nothing. Likewise
/// `net` is the desktop's own chain, never a phone-reported string.
pub fn verify_pairing_proof(
    psk_b64: &str,
    desktop_cert_fp: &str,
    phone_cert_fp: &str,
    net: &str,
    reported_proof: &str,
) -> bool {
    let Ok(expected) = pairing_proof(psk_b64, desktop_cert_fp, phone_cert_fp, net) else {
        return false;
    };
    // Constant-time over the hex strings. A length difference (e.g. an
    // empty report) compares unequal without leaking timing on the
    // matching prefix — and length alone is not secret.
    expected.as_bytes().ct_eq(reported_proof.as_bytes()).into()
}

/// Encode a pairing offer to the base64url(JSON) form the phone
/// expects inside the QR code.
pub fn encode_offer(offer: &PairingOffer) -> Result<String, String> {
    let json = serde_json::to_vec(offer).map_err(|e| format!("encode offer json: {}", e))?;
    let encoded = URL_SAFE_NO_PAD.encode(json);
    if encoded.len() > MAX_PAIRING_QR_BYTES {
        return Err("Pairing QR is too large".into());
    }
    Ok(encoded)
}

/// Encode a pairing offer for a screen that shows this computer's pairing
/// code beside the QR: the same JSON as [`encode_offer`], plus one optional
/// key, `"pc": 1` ([`PAIRING_CODE_KEY`], [`PAIRING_CODE_FORMAT`]).
///
/// The key tells the phone there is a code on this screen to compare, and in
/// which format. The phone asks "do these match?" only when it reads a format
/// it knows; without the key, or with another value, it pairs as it did
/// before. A phone that predates the key ignores it, because its parser
/// reads the keys it names and no others.
///
/// The key is not covered by the pairing proof and does not need to be.
/// Whoever can alter the QR can already replace every field in it; removing
/// the key only removes the question on the phone, and keeping it beside a
/// certificate chosen to share the code's 32 bits makes the question pass.
/// Pairing is decided by the certificate pin and the proof, with or without
/// the comparison.
///
/// Refuses an offer whose `certFp` is not a well-formed fingerprint: a QR
/// must never announce a code this screen cannot show.
pub fn encode_offer_with_pairing_code(offer: &PairingOffer) -> Result<String, String> {
    if pairing_code(&offer.cert_fp).is_none() {
        return Err("Pairing offer has no well-formed certificate fingerprint".into());
    }
    let mut json = serde_json::to_value(offer).map_err(|e| format!("encode offer json: {}", e))?;
    let Some(fields) = json.as_object_mut() else {
        return Err("encode offer json: not an object".into());
    };
    fields.insert(PAIRING_CODE_KEY.to_owned(), PAIRING_CODE_FORMAT.into());
    let bytes = serde_json::to_vec(&json).map_err(|e| format!("encode offer json: {}", e))?;
    let encoded = URL_SAFE_NO_PAD.encode(bytes);
    if encoded.len() > MAX_PAIRING_QR_BYTES {
        return Err("Pairing QR is too large".into());
    }
    Ok(encoded)
}

/// Decode a pairing offer from the base64url(JSON) form. Useful for
/// tests and for the Phase-3 "Connect by IP" fallback.
pub fn decode_offer(payload: &str) -> Result<PairingOffer, String> {
    if payload.len() > MAX_PAIRING_QR_BYTES {
        return Err("Pairing QR is too large".into());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|e| format!("decode base64url: {}", e))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("decode offer json: {}", e))
}

/// Whether this offer is still within its validity window.
pub fn is_expired(offer: &PairingOffer) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now >= offer.expires_at_unix
}

/// Seconds remaining until the offer expires (`0` if already past).
pub fn seconds_remaining(offer: &PairingOffer) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    offer.expires_at_unix.saturating_sub(now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dir::CoincubeDirectory;
    use sha2::Digest;

    fn fresh_identity() -> DesktopIdentity {
        let mut path = std::env::temp_dir();
        path.push(format!("coincube-pairing-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("mkdir");
        let dir = CoincubeDirectory::new(path);
        crate::phone_signer::identity::load_or_create(&dir).expect("identity")
    }

    #[test]
    fn qr_capacity_and_resource_ceiling() {
        use iced::widget::qr_code::{Data, ErrorCorrection, Version};
        Data::with_version(
            "a".repeat(2331),
            Version::Normal(40),
            ErrorCorrection::Medium,
        )
        .unwrap();
        assert!(Data::with_version(
            "a".repeat(2332),
            Version::Normal(40),
            ErrorCorrection::Medium
        )
        .is_err());
        assert!(decode_offer(&"a".repeat(MAX_PAIRING_QR_BYTES + 1)).is_err());
        let offer = generate_offer(
            Fingerprint::default(),
            &fresh_identity(),
            "s".repeat(MAX_PAIRING_QR_BYTES),
            OfferedKey::default(),
            crate::chain::ChainId::Bitcoin,
        )
        .offer;
        assert!(encode_offer(&offer).is_err());
    }

    #[test]
    fn offer_roundtrips_through_base64url_json() {
        let identity = fresh_identity();
        let g = generate_offer(
            Fingerprint::default(),
            &identity,
            "keychain-12345678".into(),
            OfferedKey::default(),
            crate::chain::ChainId::Bitcoin,
        );
        let encoded = encode_offer(&g.offer).expect("encode");
        let decoded = decode_offer(&encoded).expect("decode");
        assert_eq!(g.offer.version, decoded.version);
        assert_eq!(g.offer.cert_der_b64, decoded.cert_der_b64);
        assert_eq!(g.offer.cert_fp, decoded.cert_fp);
        assert_eq!(g.offer.service_name, decoded.service_name);
        assert_eq!(g.offer.wallet_fingerprint, decoded.wallet_fingerprint);
        assert_eq!(g.offer.expires_at_unix, decoded.expires_at_unix);
        assert_eq!(g.offer.psk_b64, decoded.psk_b64);
    }

    #[test]
    fn generated_offer_psk_is_fresh_and_correct_length() {
        let identity = fresh_identity();
        let a = generate_offer(
            Fingerprint::default(),
            &identity,
            "x".into(),
            OfferedKey::default(),
            crate::chain::ChainId::Bitcoin,
        )
        .offer;
        let b = generate_offer(
            Fingerprint::default(),
            &identity,
            "x".into(),
            OfferedKey::default(),
            crate::chain::ChainId::Bitcoin,
        )
        .offer;
        // Distinct per offer (a reused psk would let a captured proof
        // replay across pairings).
        assert_ne!(a.psk_b64, b.psk_b64, "psk must be fresh per offer");
        let raw = URL_SAFE_NO_PAD
            .decode(&a.psk_b64)
            .expect("psk is base64url");
        assert_eq!(
            raw.len(),
            PAIRING_PSK_LEN,
            "psk must be {PAIRING_PSK_LEN} bytes"
        );
    }

    /// The whole encoded QR (not just the cert field) must stay inside the
    /// QR budget, measured at the widest payload the protocol allows: a
    /// 111-char xpub, a 64-char descriptor hash, a 63-char mDNS service
    /// label (the DNS maximum) and the longest v3 `net`,
    /// `bitcoin-blake2b-testnet4`. The phone's cap is 2048 encoded
    /// characters (`MAX_PAIRING_QR_BYTES`, contract §4.1), and the longest
    /// payload this desktop can emit is pinned under it here.
    ///
    /// `QR_BUDGET` is derived from a real, measured ceiling rather than a
    /// guess: `iced::widget::qr_code::Data::new` encodes at error-correction
    /// Medium and tops out at **2331 bytes** (QR v40, byte mode), past which
    /// it returns `Err` and the pairing wizard shows "Couldn't render the
    /// pairing QR code." The budget sits between the two so it catches
    /// meaningful bloat — an RSA-2048 cert chain would push this to ~2.4 KB
    /// and break rendering outright — without tripping on ordinary drift.
    ///
    /// The `Data::new` call below is the assertion that actually matters;
    /// the byte budget is the early warning.
    #[test]
    fn production_offer_qr_budget() {
        const QR_BUDGET: usize = 1600;
        let secp = coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::new();
        let root = coincube_core::miniscript::bitcoin::bip32::Xpriv::new_master(
            coincube_core::miniscript::bitcoin::Network::Bitcoin,
            &[1; 32],
        )
        .unwrap();
        let xpub =
            coincube_core::miniscript::bitcoin::bip32::Xpub::from_priv(&secp, &root).to_string();
        assert_eq!(xpub.len(), 111);
        let offer = generate_offer(
            Fingerprint::default(),
            &fresh_identity(),
            "s".repeat(63),
            OfferedKey {
                signer_xpub: xpub,
                descriptor_sha256: "ab".repeat(32),
            },
            crate::chain::ChainId::BitcoinBlake2bTestnet4,
        )
        .offer;
        assert_eq!(
            crate::chain::ChainId::ALL
                .iter()
                .map(|c| c.api_str().len())
                .max(),
            Some(offer.network.len()),
            "measured with the longest network id"
        );
        let encoded = encode_offer(&offer).unwrap();
        println!(
            "production offer with maximum DNS service label: {} bytes",
            encoded.len()
        );
        iced::widget::qr_code::Data::new(&encoded).unwrap();
        assert!(
            encoded.len() < QR_BUDGET,
            "production payload is {} bytes, budget {}",
            encoded.len(),
            QR_BUDGET,
        );
        assert!(encoded.len() <= MAX_PAIRING_QR_BYTES);
    }

    #[test]
    fn pairing_proof_verifies_for_matching_inputs() {
        let g = generate_offer(
            Fingerprint::default(),
            &fresh_identity(),
            "x".into(),
            OfferedKey::default(),
            crate::chain::ChainId::Bitcoin,
        )
        .offer;
        let desktop_fp = &g.cert_fp;
        let phone_fp = "ab".repeat(32); // any 64-hex string
        let proof = pairing_proof(&g.psk_b64, desktop_fp, &phone_fp, &g.network).expect("compute");
        assert_eq!(proof.len(), 64, "proof is 64 lowercase-hex chars");
        assert!(verify_pairing_proof(
            &g.psk_b64, desktop_fp, &phone_fp, &g.network, &proof
        ));
    }

    #[test]
    fn pairing_proof_is_bound_to_both_fingerprints() {
        let g = generate_offer(
            Fingerprint::default(),
            &fresh_identity(),
            "x".into(),
            OfferedKey::default(),
            crate::chain::ChainId::Bitcoin,
        )
        .offer;
        let desktop_fp = &g.cert_fp;
        let phone_fp = "ab".repeat(32);
        let proof = pairing_proof(&g.psk_b64, desktop_fp, &phone_fp, &g.network).expect("compute");

        // Wrong phone fp (the relay/substitution case): a peer that
        // terminates TLS with a different cert produces a proof over a
        // different phone fp than the desktop expects → rejected.
        let other_phone_fp = "cd".repeat(32);
        assert!(
            !verify_pairing_proof(&g.psk_b64, desktop_fp, &other_phone_fp, &g.network, &proof),
            "proof must not verify against a different phone cert fp",
        );
        // Wrong desktop fp.
        let other_desktop_fp = "ef".repeat(32);
        assert!(
            !verify_pairing_proof(&g.psk_b64, &other_desktop_fp, &phone_fp, &g.network, &proof),
            "proof must not verify against a different desktop cert fp",
        );
    }

    /// Shared cross-implementation known-answer vector for the v3 proof
    /// (signing identity contract §4.2). COINCUBE Mobile asserts the SAME
    /// tuples in `test/services/local_signer/pairing_qr_test.dart`
    /// ("matches the locked Rust ↔ Dart vector"), so a drift in either
    /// codebase's HMAC construction trips both tests. Do not change without
    /// changing the phone side and bumping the domain tag: it is the wire
    /// protocol.
    ///
    /// psk = the 16 bytes 0x00..0x0f (`AAECAwQFBgcICQoLDA0ODw`),
    /// desktop fp = "aa" x 32, phone fp = "bb" x 32.
    #[test]
    fn pairing_proof_known_answer_vector() {
        let psk_b64 = "AAECAwQFBgcICQoLDA0ODw";
        let desktop_fp = "aa".repeat(32);
        let phone_fp = "bb".repeat(32);
        for (net, expected) in [
            (
                "bitcoin-blake2b",
                "ccc65575005c19fbb4826f49c68db719fc1c7f2bc0de4deea2c6e4d722c5263c",
            ),
            (
                "mainnet",
                "e7f46e42e05dd6a69c1f8372353d38fbfce4ee54d1c7ddd13bee54759ef5854a",
            ),
            (
                "bitcoin-blake2b-testnet4",
                "1c81ce3dea97020338d1dc09f88e9eaafdd100187f92fb7d3895d9fd8f5a6114",
            ),
        ] {
            let proof = pairing_proof(psk_b64, &desktop_fp, &phone_fp, net).expect("compute");
            assert_eq!(proof, expected, "{}", net);
            assert!(verify_pairing_proof(
                psk_b64,
                &desktop_fp,
                &phone_fp,
                net,
                &proof
            ));
        }
        // The retired v2 vector no longer verifies under any chain: the
        // domain and the message both changed.
        let v2 = "87f118c74fd540810db273e2ff4c00ee805831dce43aa649553d636e795239b9";
        for net in ["", "mainnet", "bitcoin-blake2b"] {
            assert!(!verify_pairing_proof(
                psk_b64,
                &desktop_fp,
                &phone_fp,
                net,
                v2
            ));
        }
    }

    /// v3 on every chain: the version is 3 and `net` is the chain's
    /// canonical Connect network id, never the `bitcoin` alias.
    #[test]
    fn generated_offers_are_v3_with_the_chains_network() {
        use crate::chain::ChainId;
        for chain in ChainId::ALL {
            let offer = generate_offer(
                Fingerprint::default(),
                &fresh_identity(),
                "x".into(),
                OfferedKey::default(),
                chain,
            )
            .offer;
            assert_eq!(offer.version, 3);
            assert_eq!(offer.network, chain.api_str());
            let json: serde_json::Value = serde_json::from_slice(
                &URL_SAFE_NO_PAD
                    .decode(encode_offer(&offer).unwrap())
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(json["v"], 3);
            assert_eq!(json["net"], chain.api_str());
        }
    }

    #[test]
    fn pairing_proof_rejects_wrong_psk_tamper_and_empty() {
        let g = generate_offer(
            Fingerprint::default(),
            &fresh_identity(),
            "x".into(),
            OfferedKey::default(),
            crate::chain::ChainId::Bitcoin,
        )
        .offer;
        let other = generate_offer(
            Fingerprint::default(),
            &fresh_identity(),
            "x".into(),
            OfferedKey::default(),
            crate::chain::ChainId::Bitcoin,
        )
        .offer;
        let desktop_fp = &g.cert_fp;
        let phone_fp = "ab".repeat(32);
        let proof = pairing_proof(&g.psk_b64, desktop_fp, &phone_fp, &g.network).expect("compute");

        // A different psk (didn't scan THIS QR).
        assert!(!verify_pairing_proof(
            &other.psk_b64,
            desktop_fp,
            &phone_fp,
            &g.network,
            &proof
        ));
        // Tampered proof.
        let mut tampered = proof.clone();
        tampered.replace_range(0..1, if proof.starts_with('0') { "1" } else { "0" });
        assert!(!verify_pairing_proof(
            &g.psk_b64, desktop_fp, &phone_fp, &g.network, &tampered
        ));
        // Empty / missing (e.g. a pre-v2 phone).
        assert!(!verify_pairing_proof(
            &g.psk_b64, desktop_fp, &phone_fp, &g.network, ""
        ));
        // A proof computed over another chain (contract §4.2): the desktop
        // verifies with its own `net`, so this fails.
        let other_chain =
            pairing_proof(&g.psk_b64, desktop_fp, &phone_fp, "bitcoin-blake2b").expect("compute");
        assert_eq!(g.network, "mainnet");
        assert!(!verify_pairing_proof(
            &g.psk_b64,
            desktop_fp,
            &phone_fp,
            &g.network,
            &other_chain
        ));
        // Swapped fingerprints.
        let swapped =
            pairing_proof(&g.psk_b64, &phone_fp, desktop_fp, &g.network).expect("compute");
        assert!(!verify_pairing_proof(
            &g.psk_b64, desktop_fp, &phone_fp, &g.network, &swapped
        ));
    }

    #[test]
    fn generated_offer_records_phone_service_name() {
        let identity = fresh_identity();
        let g = generate_offer(
            Fingerprint::default(),
            &identity,
            "keychain-deadbeef".into(),
            OfferedKey::default(),
            crate::chain::ChainId::Bitcoin,
        );
        assert_eq!(g.offer.service_name, "keychain-deadbeef");
    }

    #[test]
    fn expires_at_is_in_the_future() {
        let identity = fresh_identity();
        let g = generate_offer(
            Fingerprint::default(),
            &identity,
            "keychain-00000000".into(),
            OfferedKey::default(),
            crate::chain::ChainId::Bitcoin,
        );
        assert!(!is_expired(&g.offer));
        assert!(seconds_remaining(&g.offer) > 0);
    }

    /// Critical invariant: `cert` and `certFp` are derived from the
    /// same `cert_der` and must agree byte-for-byte. The phone's
    /// matching check will refuse any QR where they don't.
    #[test]
    fn cert_and_cert_fp_agree() {
        let identity = fresh_identity();
        let g = generate_offer(
            Fingerprint::default(),
            &identity,
            "keychain-12345678".into(),
            OfferedKey::default(),
            crate::chain::ChainId::Bitcoin,
        );
        let raw = URL_SAFE_NO_PAD
            .decode(&g.offer.cert_der_b64)
            .expect("decode cert b64");
        let computed = sha2::Sha256::digest(&raw);
        let computed_hex: String = computed.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(computed_hex, g.offer.cert_fp);
    }

    /// Sanity check on QR capacity. Ed25519 cert DER is ~250-300
    /// bytes raw, base64url-encoded ~400 bytes. The phone accepts up
    /// to ~1 KB before falling out of medium-EC QR capacity, so this
    /// catches a regression that would silently bloat the QR
    /// (e.g. switching to an RSA-2048 cert chain).
    #[test]
    fn cert_is_under_size_limit() {
        let identity = fresh_identity();
        let g = generate_offer(
            Fingerprint::default(),
            &identity,
            "keychain-12345678".into(),
            OfferedKey::default(),
            crate::chain::ChainId::Bitcoin,
        );
        assert!(
            g.offer.cert_der_b64.len() < 1024,
            "cert b64 len {} >= 1024",
            g.offer.cert_der_b64.len(),
        );
    }
}

/// The pairing code and the offer's optional `pc` marker (Mobile V3 Phase 1,
/// Desktop PR T1). Kept apart from the tests above, which are unchanged.
#[cfg(test)]
mod pairing_code_tests {
    use super::*;
    use crate::dir::CoincubeDirectory;
    use serde_json::Value;
    use sha2::Digest;

    /// The fixture both apps test against. COINCUBE Mobile copies this file.
    const FIXTURE: &str = include_str!("fixtures/pairing_code_v1.json");

    fn fixture() -> Value {
        serde_json::from_str(FIXTURE).expect("fixture is JSON")
    }

    fn text<'a>(value: &'a Value, path: &[&str]) -> &'a str {
        path.iter()
            .fold(value, |v, key| &v[*key])
            .as_str()
            .unwrap_or_else(|| panic!("fixture has no string at {:?}", path))
    }

    fn decode_json(payload: &str) -> Value {
        let bytes = URL_SAFE_NO_PAD.decode(payload).expect("base64url");
        serde_json::from_slice(&bytes).expect("offer json")
    }

    fn fixture_offer() -> PairingOffer {
        let fields = fixture()["offer"]["fields"].clone();
        serde_json::from_value(fields).expect("fixture offer fields")
    }

    fn generated_offer() -> PairingOffer {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "coincube-pairing-code-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).expect("mkdir");
        let identity =
            crate::phone_signer::identity::load_or_create(&CoincubeDirectory::new(path.clone()))
                .expect("identity");
        let offer = generate_offer(
            "f108451f".parse().unwrap(),
            &identity,
            "COINCUBE-test-phone".into(),
            OfferedKey {
                signer_xpub: "tpubD6NzVbkrYhZ4XfixtureOnlyNotARealKey".into(),
                descriptor_sha256: "ab".repeat(32),
            },
            crate::chain::ChainId::Bitcoin,
        )
        .offer;
        // The code is the short form of the identity's own fingerprint.
        assert_eq!(offer.cert_fp, identity.cert_fp());
        assert_eq!(
            pairing_code(&offer.cert_fp).unwrap().short.replace(' ', ""),
            identity.cert_fp8().to_ascii_uppercase()
        );
        let _ = std::fs::remove_dir_all(path);
        offer
    }

    #[test]
    fn the_code_is_the_first_eight_characters_upper_case_in_two_groups() {
        let fp = "a1b2c3d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeeff00";
        let code = pairing_code(fp).unwrap();
        assert_eq!(code.short, "A1B2 C3D4");
        assert_eq!(
            code.full,
            "A1B2 C3D4 E5F6 0718 293A 4B5C 6D7E 8F90 1122 3344 5566 7788 99AA BBCC DDEE FF00"
        );
        assert!(code.full.starts_with(&code.short));
        assert_eq!(code.full.replace(' ', "").to_ascii_lowercase(), fp);
    }

    #[test]
    fn a_malformed_fingerprint_has_no_code() {
        let good = "a1b2c3d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeeff00";
        assert!(pairing_code(good).is_some());
        for bad in [
            String::new(),
            good[..63].to_string(),
            format!("{good}0"),
            good.to_ascii_uppercase(),
            good.replace('a', "g"),
            good.replace("a1", "é"),
            format!(" {}", &good[1..]),
        ] {
            assert_eq!(pairing_code(&bad), None, "{bad:?}");
        }
    }

    /// One certificate digest and the string both apps must show for it; and
    /// a phone fingerprint that formats to something else, so a user (or a
    /// test) comparing the phone's fingerprint with the code sees a mismatch.
    #[test]
    fn the_shared_fixture_formats_the_computers_fingerprint_and_not_the_phones() {
        let fx = fixture();
        assert_eq!(fx["format"], PAIRING_CODE_FORMAT);
        assert_eq!(text(&fx, &["marker", "key"]), PAIRING_CODE_KEY);
        assert_eq!(fx["marker"]["value"], PAIRING_CODE_FORMAT);

        let desktop_fp = text(&fx, &["desktop", "cert_fp"]);
        let cert = URL_SAFE_NO_PAD
            .decode(text(&fx, &["desktop", "cert_der_b64"]))
            .unwrap();
        assert_eq!(hex::encode(sha2::Sha256::digest(&cert)), desktop_fp);

        let code = pairing_code(desktop_fp).unwrap();
        assert_eq!(code.short, text(&fx, &["desktop", "pairing_code"]));
        assert_eq!(code.full, text(&fx, &["desktop", "pairing_code_full"]));

        let phone_fp = text(&fx, &["phone", "cert_fp"]);
        assert_eq!(&phone_fp[..8], text(&fx, &["phone", "fp8"]));
        let wrong = pairing_code(phone_fp).unwrap().short;
        assert_eq!(wrong, text(&fx, &["phone", "not_the_pairing_code"]));
        assert_ne!(wrong, code.short, "comparing the wrong pair must fail");
    }

    #[test]
    fn the_marker_is_one_extra_key_and_changes_nothing_else() {
        for offer in [fixture_offer(), generated_offer()] {
            let plain = decode_json(&encode_offer(&offer).unwrap());
            let mut marked = decode_json(&encode_offer_with_pairing_code(&offer).unwrap());
            let marker = marked
                .as_object_mut()
                .unwrap()
                .remove(PAIRING_CODE_KEY)
                .expect("the marker is present");
            assert_eq!(marker, Value::from(PAIRING_CODE_FORMAT));
            assert_eq!(marked, plain, "every other key and value is identical");
            assert!(plain.get(PAIRING_CODE_KEY).is_none());
        }
    }

    #[test]
    fn the_fixture_payloads_are_what_the_encoders_produce() {
        let fx = fixture();
        let offer = fixture_offer();
        assert_eq!(
            encode_offer(&offer).unwrap(),
            text(&fx, &["offer", "payload_without_marker"])
        );
        // Key order is not part of the contract, so compare as JSON.
        assert_eq!(
            decode_json(&encode_offer_with_pairing_code(&offer).unwrap()),
            decode_json(text(&fx, &["offer", "payload_with_marker"]))
        );
    }

    #[test]
    fn this_desktops_own_decoder_reads_a_marked_offer_as_the_same_offer() {
        let offer = generated_offer();
        let decoded = decode_offer(&encode_offer_with_pairing_code(&offer).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&decoded).unwrap(),
            serde_json::to_value(&offer).unwrap()
        );
    }

    #[test]
    fn a_marked_offer_stays_inside_the_qr_budget() {
        let offer = generated_offer();
        let plain = encode_offer(&offer).unwrap();
        let marked = encode_offer_with_pairing_code(&offer).unwrap();
        // `"pc":1,` is 7 bytes of JSON: at most 12 base64url characters.
        assert!(marked.len() > plain.len());
        assert!(
            marked.len() <= plain.len() + 12,
            "{} vs {}",
            marked.len(),
            plain.len()
        );
        assert!(marked.len() <= MAX_PAIRING_QR_BYTES);
        iced::widget::qr_code::Data::new(&marked).expect("fits a QR code");
    }

    /// The largest offer this desktop can make: a full-length xpub, the
    /// longest mDNS instance name, the longest network id.
    #[test]
    fn the_largest_marked_offer_stays_inside_the_qr_budget() {
        let mut offer = generated_offer();
        offer.signer_xpub = format!("tpub{}", "D".repeat(107));
        assert_eq!(offer.signer_xpub.len(), 111);
        offer.service_name = "s".repeat(63);
        offer.network = crate::chain::ChainId::BitcoinBlake2bTestnet4
            .api_str()
            .to_owned();
        assert_eq!(offer.network, "bitcoin-blake2b-testnet4");
        let marked = encode_offer_with_pairing_code(&offer).unwrap();
        assert!(marked.len() <= MAX_PAIRING_QR_BYTES, "{}", marked.len());
        iced::widget::qr_code::Data::new(&marked).expect("fits a QR code");
    }

    #[test]
    fn a_qr_never_announces_a_code_the_screen_cannot_show() {
        let mut offer = generated_offer();
        offer.cert_fp = String::new();
        assert!(pairing_code(&offer.cert_fp).is_none());
        assert!(encode_offer_with_pairing_code(&offer).is_err());
        // The plain encoder is unchanged and still encodes it.
        assert!(encode_offer(&offer).is_ok());
    }

    /// The checks COINCUBE Mobile's offer parser makes today
    /// (`PairingOffer.fromQrPayload` and `_validate` in
    /// `lib/services/local_signer/pairing_qr.dart`, read at coincube-mobile
    /// `origin/main` 799b83a52f4f8e8f1b56d6c6028ad7860a91ea4c and unchanged at
    /// 86050800eba86b09102c4feb4be6d2105f2882aa), in the same order. It decodes the JSON into a map and reads only the keys below:
    /// there is no check for unknown keys, which is why `pc` is ignored.
    fn todays_phone_parser(payload: &str, now_unix: u64) -> Result<(), String> {
        const MAX_QR_PAYLOAD_BYTES: usize = 2048;
        if payload.len() > MAX_QR_PAYLOAD_BYTES {
            return Err("payload is too large".into());
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|e| format!("not valid base64url: {e}"))?;
        let json: Value =
            serde_json::from_slice(&bytes).map_err(|e| format!("payload is not JSON: {e}"))?;
        let json = json.as_object().ok_or("payload is not a JSON object")?;
        let require_int = |key: &str| {
            json.get(key)
                .and_then(Value::as_i64)
                .ok_or(format!("{key} must be an integer"))
        };
        let require_string = |key: &str| {
            json.get(key)
                .and_then(Value::as_str)
                .ok_or(format!("{key} must be a string"))
        };
        // Lenient reads: absent is allowed here and judged below; a present
        // value of another type is a cast failure in Dart.
        let optional_string = |key: &str| match json.get(key) {
            None | Some(Value::Null) => Ok(""),
            Some(Value::String(s)) => Ok(s.as_str()),
            Some(_) => Err(format!("{key} must be a string")),
        };
        let is_lower_hex = |s: &str, len: usize| {
            s.len() == len
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };

        let v = require_int("v")?;
        if v == 1 || v == 2 {
            return Err("Update COINCUBE on the desktop and pair again.".into());
        }
        if v != 3 {
            return Err(format!("unsupported version {v}"));
        }
        optional_string("key")?;
        optional_string("dh")?;
        let net = optional_string("net")?;
        let cert = require_string("cert")?;
        let cert_fp = require_string("certFp")?;
        let svc = require_string("svc")?;
        let wfp = require_string("wfp")?;
        let exp = require_int("exp")?;
        let psk = optional_string("psk")?;

        // `canonicalChainId`: the phone's closed set of canonical Connect
        // network ids (`SignerNetwork.tryFromEnvString`, minus the `bitcoin`
        // alias of `mainnet`).
        const CANONICAL_NETWORKS: [&str; 7] = [
            "mainnet",
            "testnet",
            "testnet4",
            "signet",
            "regtest",
            "bitcoin-blake2b",
            "bitcoin-blake2b-testnet4",
        ];
        if !CANONICAL_NETWORKS.contains(&net) {
            return Err("unsupported network".into());
        }
        if psk.is_empty() {
            return Err("missing psk".into());
        }
        let psk_bytes = URL_SAFE_NO_PAD
            .decode(psk.trim_end_matches('='))
            .map_err(|e| format!("psk is not valid base64url: {e}"))?;
        if psk_bytes.len() != 16 {
            return Err(format!("psk decodes to {} bytes", psk_bytes.len()));
        }
        if !is_lower_hex(cert_fp, 64) {
            return Err("certFp must be 64 lowercase hex chars".into());
        }
        if svc.is_empty() || svc.len() > 64 {
            return Err("svc must be a non-empty mDNS instance name".into());
        }
        if !is_lower_hex(wfp, 8) {
            return Err("wfp must be 8 lowercase hex chars".into());
        }
        let cert_bytes = URL_SAFE_NO_PAD
            .decode(cert.trim_end_matches('='))
            .map_err(|e| format!("cert is not valid base64url: {e}"))?;
        if !(80..=4096).contains(&cert_bytes.len()) {
            return Err(format!(
                "cert byte length {} out of bounds",
                cert_bytes.len()
            ));
        }
        if hex::encode(sha2::Sha256::digest(&cert_bytes)) != cert_fp {
            return Err("cert/certFp mismatch".into());
        }
        let window = exp - now_unix as i64;
        if window <= 0 {
            return Err("offer already expired".into());
        }
        if window < 30 {
            return Err("expiry too soon".into());
        }
        if window > 3600 {
            return Err("expiry too far in the future".into());
        }
        Ok(())
    }

    #[test]
    fn todays_phone_parser_accepts_an_offer_with_the_marker() {
        let fx = fixture();
        let now = fx["offer"]["now_unix"].as_u64().unwrap();
        for key in ["payload_without_marker", "payload_with_marker"] {
            todays_phone_parser(text(&fx, &["offer", key]), now)
                .unwrap_or_else(|e| panic!("{}: {}", key, e));
        }

        // And a live offer from this build, as the pairing screen encodes it.
        let offer = generated_offer();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        todays_phone_parser(&encode_offer(&offer).unwrap(), now).unwrap();
        todays_phone_parser(&encode_offer_with_pairing_code(&offer).unwrap(), now).unwrap();
    }

    /// The mirror above is only worth something if it refuses what the phone
    /// refuses. Spot-check that it is not vacuous.
    #[test]
    fn the_parser_mirror_refuses_what_the_phone_refuses() {
        let fx = fixture();
        let now = fx["offer"]["now_unix"].as_u64().unwrap();
        let fields = fx["offer"]["fields"].clone();
        let with = |edit: &dyn Fn(&mut serde_json::Map<String, Value>)| {
            let mut fields = fields.as_object().unwrap().clone();
            fields.insert(PAIRING_CODE_KEY.into(), 1.into());
            edit(&mut fields);
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&fields).unwrap())
        };
        assert!(todays_phone_parser(&with(&|_| {}), now).is_ok());
        assert!(todays_phone_parser(
            &with(&|f| {
                f.insert("v".into(), 2.into());
            }),
            now
        )
        .is_err());
        assert!(todays_phone_parser(
            &with(&|f| {
                f.remove("psk");
            }),
            now
        )
        .is_err());
        assert!(todays_phone_parser(
            &with(&|f| {
                f.insert("net".into(), "bitcoin".into());
            }),
            now
        )
        .is_err());
        assert!(todays_phone_parser(
            &with(&|f| {
                f.insert("certFp".into(), "00".repeat(32).into());
            }),
            now
        )
        .is_err());
        assert!(todays_phone_parser(&with(&|_| {}), now + 7200).is_err());
    }
}
