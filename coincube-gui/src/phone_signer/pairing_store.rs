//! Persisted list of phones this desktop has paired with.
//!
//! Stored as a single JSON file under [`CoincubeDirectory`] (alongside
//! the bitbox noise pairing config). One file holds N entries so we
//! can render a "Paired phones" table directly from disk.
//!
//! Skeleton status: types and load/save signatures are in place;
//! the file I/O paths are stubbed and marked TODO.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use coincube_core::miniscript::bitcoin::bip32::{DerivationPath, Fingerprint};

use crate::{
    app::settings::WalletId,
    chain::ChainId,
    dir::CoincubeDirectory,
    phone_signer::policy::{PhonePolicy, PhonePolicyKind},
};

/// On-disk record for a single paired phone. We persist the phone's
/// stable Ed25519 identity pubkey (captured during the pairing
/// handshake) plus enough metadata to render a "Paired phones" row
/// without doing any I/O.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedPhone {
    /// Absent on legacy pairings, which must be paired again.
    #[serde(default)]
    pub signer_binding: Option<SignerBinding>,
    /// Phone's TLS cert pin: `SHA-256(self-signed cert DER)`, 32
    /// raw bytes. Captured from the live TLS handshake via
    /// [`crate::phone_signer::transport::PairedTransport::peer_cert_fingerprint`]
    /// at pairing time and used to verify the phone's cert on every
    /// subsequent reconnect (matched by
    /// [`crate::phone_signer::tls::PinnedVerifier`]).
    ///
    /// **Not an Ed25519 pubkey** despite the on-disk JSON field name
    /// (kept as `"identity_pubkey"` for backward compat with v1.0
    /// stores via `#[serde(rename)]`). Attempting Ed25519 signature
    /// verification against these bytes would silently fail — they
    /// are a SHA-256 digest, not a curve point.
    #[serde(rename = "identity_pubkey")]
    pub cert_pin: [u8; 32],

    /// User-facing name. Defaults to the `device_name` reported by
    /// the phone in `PairingComplete`; the settings panel lets the
    /// user rename it.
    pub name: String,

    /// When this pairing was finalised, as unix seconds. Drives the
    /// "Paired on …" row in the settings table. Stored as `u64`
    /// instead of `chrono::DateTime` so the on-disk record doesn't
    /// require pulling in the chrono `serde` feature.
    pub paired_at_unix: u64,

    /// Compatibility/display metadata containing only the independently
    /// phone-reported selected key fingerprint. Exact signing authority lives
    /// in signer_binding; never infer ownership from this list.
    pub wallet_fingerprints: Vec<Fingerprint>,

    /// The first four bytes of the paired transport descriptor's SHA-256.
    /// This legacy field name also carries the imported `/<0;1>/*`
    /// descriptor's display ID. It gates device discovery, while the full
    /// descriptor hash, chain-bound wallet ID and account origin in
    /// `signer_binding` determine imported signing authority.
    ///
    /// `#[serde(default)]` so a row written before this field existed
    /// deserialises to `Fingerprint::default()`. An all-zero id never
    /// matches a real vault (`id_fingerprint` is `sha256(descriptor)[..4]`),
    /// so a legacy phone simply surfaces as offline until re-paired.
    #[serde(default)]
    pub vault_fingerprint: Fingerprint,

    /// The phone's ECIES transport public key (33-byte compressed
    /// secp256k1), reported in `PairingComplete.transport_pubkey` and
    /// validated at pairing.
    ///
    /// This is what makes the LAN rail ECIES_V1 like every other rail:
    /// [`crate::phone_signer::PhoneSigner::sign_tx`] seals the PSBT and the
    /// full descriptor to this key, so a paired phone gets the same sealed
    /// protos — and runs the same fingerprint + xpub-membership checks — as
    /// a Connect-mediated one. There is deliberately no `descriptor_id`-only
    /// path for LAN.
    ///
    /// Capturing it once at pairing is sound: a phone that reinstalls mints a
    /// fresh TLS identity too, so it fails [`Self::cert_pin`] and must re-pair
    /// before it could ever present a stale transport key.
    ///
    /// `#[serde(default)]` gives rows written before this field an empty vec.
    /// That is *not* treated as "seal nothing" — `sign_tx` refuses to build a
    /// session at all, telling the user to re-pair, because the alternative is
    /// exactly the plaintext fallback this field exists to remove.
    #[serde(default)]
    pub transport_pubkey: Vec<u8>,

    /// Optional manually-entered fallback target. Phase 3 surfaces a
    /// "Connect by IP" field for networks where mDNS is blocked.
    /// `host:port` form; `None` means rely on mDNS.
    pub fallback_addr: Option<String>,

    /// The chain this pairing is for, as its Connect network id: the v3
    /// offer's `net`, which the phone's record matched (signing identity
    /// contract §4.5). `None` is a pairing made before v3, which is never
    /// used to sign on any chain (master decision 21): it stays listed,
    /// refused with [`PRE_V3_PAIRING`], so the user can remove it and tell
    /// the phone.
    #[serde(default)]
    pub network: Option<String>,

    /// The phone's capability list as `PairingComplete` reported it, recorded
    /// at pairing and never refreshed afterwards (contract §4.5). A phone that
    /// paired with a build lacking a capability is not dialled for what needs
    /// it until it pairs again.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Signer-list copy for a Bitcoin Blake2b Vault when the phone did not report
/// both capabilities at pairing (contract R2.6, capability variant), word for
/// word. The capabilities are recorded at pairing only, so an updated phone
/// must also pair again before it is dialled.
pub const BTCB2_PHONE_OUTDATED: &str =
    "This phone's COINCUBE Mobile needs updating before it can sign on Bitcoin Blake2b.";

/// Signer-list copy for a pairing made before v3 (contract R2.6, legacy
/// variant), shown for every Vault on every chain.
pub const PRE_V3_PAIRING: &str =
    "This pairing predates network-aware signing. Pair this phone again.";

impl PairedPhone {
    /// The notice the pairing screen shows before the user scans when this
    /// phone already has a pairing for another chain, or one made before v3.
    /// There is one pairing per phone and desktop (master decision 19), so
    /// the new one replaces it. `None` when the stored pairing is for `chain`.
    pub fn replaced_by_pairing_for(&self, chain: ChainId) -> Option<String> {
        use crate::chain::ChainIdExt;
        match self.network.as_deref() {
            Some(net) if net == chain.api_str() => None,
            Some(net) => Some(match ChainId::from_api_str(net) {
                Some(other) => coincube_ui::localized_format!(
                    "Paired for {}. Pairing it here replaces that pairing.",
                    "Emparejado para {}. Emparejarlo aquí reemplaza ese emparejamiento.",
                    other.label()
                ),
                None => coincube_ui::localized_format!(
                    "Paired for another network. Pairing it here replaces that pairing.",
                    "Emparejado para otra red. Emparejarlo aquí reemplaza ese emparejamiento."
                ),
            }),
            None => Some(coincube_ui::localized_format!(
                "Paired before network-aware signing. Pairing it here replaces that pairing.",
                "Emparejado antes de la firma por red. Emparejarlo aquí reemplaza ese emparejamiento."
            )),
        }
    }
}

/// QR-selected key independently resolved by the authenticated phone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignerBinding {
    pub key_id: String,
    pub xpub: String,
    pub fingerprint: Fingerprint,
    pub descriptor_sha256: Vec<u8>,
    /// Present only after an imported wallet's exact account and chain-bound
    /// storage identity were confirmed. Legacy and Vault rows omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_scope: Option<ImportedPairingScope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedPairingScope {
    pub chain: ChainId,
    pub wallet_id: WalletId,
    pub account_origin: DerivationPath,
    /// Fresh admission identity. The pairing transaction overwrites this with
    /// its unique ID before publication, so same-second re-pairing revokes an
    /// old pending response even when all cryptographic fields are identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_revision: Option<String>,
}

impl ImportedPairingScope {
    /// Resolve the origin from the validated local wallet, never from a phone
    /// claim or a shared master fingerprint alone. The pairing UI must still
    /// obtain a fresh user confirmation before persisting this scope.
    pub fn for_account(
        wallet: &coincube_core::wallet_descriptor::ImportedWalletDescriptor,
        wallet_id: &WalletId,
        xpub: &str,
        fingerprint: Fingerprint,
    ) -> Result<Self, String> {
        use coincube_core::miniscript::ForEachKey;
        let error = "The selected phone account is not an exact imported wallet key.";
        if *wallet_id != WalletId::for_imported(wallet, wallet.chain()).map_err(|_| error)? {
            return Err(error.into());
        }
        let mut origins = Vec::new();
        wallet.receive_descriptor().for_each_key(|key| {
            if let Some(origin) = exact_account_origin(key, xpub, fingerprint) {
                origins.push(origin);
            }
            true
        });
        if origins.len() != 1 {
            return Err(error.into());
        }
        Ok(Self {
            chain: wallet.chain(),
            wallet_id: wallet_id.clone(),
            account_origin: origins.remove(0),
            pairing_revision: Some(uuid::Uuid::new_v4().to_string()),
        })
    }
}

fn exact_account_origin(
    key: &coincube_core::miniscript::DescriptorPublicKey,
    xpub: &str,
    fingerprint: Fingerprint,
) -> Option<DerivationPath> {
    use coincube_core::miniscript::DescriptorPublicKey;
    if let DescriptorPublicKey::XPub(account) = key {
        if account.xkey.to_string() == xpub {
            if let Some((origin_fp, path)) = &account.origin {
                if *origin_fp == fingerprint {
                    return Some(path.clone());
                }
            }
        }
    }
    None
}

impl PairedPhone {
    pub fn imported_pairing_revision(&self) -> Option<&str> {
        self.signer_binding
            .as_ref()
            .and_then(|binding| binding.imported_scope.as_ref())
            .and_then(|scope| scope.pairing_revision.as_deref())
    }

    pub fn exact_signer_for(&self, policy: &PhonePolicy) -> Result<&SignerBinding, String> {
        match policy.kind() {
            PhonePolicyKind::Vault(_) => self.exact_signer(policy.descriptor(), policy.chain()),
            PhonePolicyKind::Imported { wallet, wallet_id } => {
                self.exact_imported_signer(policy.descriptor(), wallet, wallet_id)
            }
        }
    }

    fn exact_imported_signer(
        &self,
        descriptor: &str,
        wallet: &coincube_core::wallet_descriptor::ImportedWalletDescriptor,
        wallet_id: &WalletId,
    ) -> Result<&SignerBinding, String> {
        use coincube_core::miniscript::{DescriptorPublicKey, ForEachKey};
        use sha2::{Digest, Sha256};
        let error = "Pair this phone again and select its exact imported account key.";
        self.check_paired_chain(wallet.chain())?;
        let binding = self.signer_binding.as_ref().ok_or(error)?;
        let scope = binding.imported_scope.as_ref().ok_or(error)?;
        let digest = Sha256::digest(descriptor.as_bytes());
        if scope.chain != wallet.chain()
            || &scope.wallet_id != wallet_id
            || *wallet_id != WalletId::for_imported(wallet, scope.chain).map_err(|_| error)?
            || binding
                .key_id
                .parse::<u64>()
                .ok()
                .filter(|id| *id > 0)
                .is_none()
            || binding.descriptor_sha256.as_slice() != digest.as_slice()
            || self.vault_fingerprint.to_string() != hex::encode(&digest[..4])
        {
            return Err(error.into());
        }
        let mut matches = 0usize;
        wallet.receive_descriptor().for_each_key(|key| {
            if let DescriptorPublicKey::XPub(xpub) = key {
                if xpub.xkey.to_string() == binding.xpub
                    && xpub.origin.as_ref()
                        == Some(&(binding.fingerprint, scope.account_origin.clone()))
                {
                    matches += 1;
                }
            }
            true
        });
        if matches != 1 {
            return Err(error.into());
        }
        Ok(binding)
    }

    /// Full descriptor commitment and exact parsed key membership are authority;
    /// a master fingerprint or the four-byte vault display ID alone is not.
    pub fn exact_signer(&self, descriptor: &str, chain: ChainId) -> Result<&SignerBinding, String> {
        use coincube_core::{descriptors::CoincubeDescriptor, miniscript::DescriptorPublicKey};
        use sha2::{Digest, Sha256};
        use std::str::FromStr;
        let error = "Pair this phone again and select its exact vault key.";
        let parsed = CoincubeDescriptor::from_str(descriptor).map_err(|_| error)?;
        let vault_keys: Vec<(String, Fingerprint)> = parsed
            .spendable_keys()
            .iter()
            .filter_map(|key| match key {
                DescriptorPublicKey::XPub(k) => {
                    k.origin.as_ref().map(|(fp, _)| (k.xkey.to_string(), *fp))
                }
                _ => None,
            })
            .collect();
        self.exact_signer_against(
            &hex::encode(Sha256::digest(descriptor.as_bytes())),
            &vault_keys,
            chain,
        )
    }

    /// [`Self::exact_signer`] against inputs a caller has already resolved:
    /// the hex SHA-256 of the loaded descriptor, and its `(xpub, origin
    /// fingerprint)` pairs.
    ///
    /// Exists so callers that hold those already — the settings view renders
    /// per phone, per frame — get the *same* verdict without re-parsing the
    /// descriptor. Checking a subset here is what makes a row read "Exact
    /// vault key paired" while signing and the hw refresh loop reject the
    /// phone, so this is the only predicate either path should use.
    ///
    /// `chain` is the Vault's chain. A Bitcoin Vault and its Bitcoin Blake2b
    /// twin share a descriptor, so the chain rule lives here (contract
    /// §4.5): the row must have been paired for this chain (R2.7; a pre-v3
    /// row is R2.6, legacy variant), and on Bitcoin Blake2b the phone must
    /// have reported both capabilities at pairing (R2.6, capability variant;
    /// `mobile_signing::btcb2_signing_capable`, the rule the Connect rail
    /// uses). Bitcoin-family Vaults require no capability.
    pub fn exact_signer_against(
        &self,
        descriptor_sha256_hex: &str,
        vault_keys: &[(String, Fingerprint)],
        chain: ChainId,
    ) -> Result<&SignerBinding, String> {
        let error = "Pair this phone again and select its exact vault key.";
        self.check_paired_chain(chain)?;
        let binding = self.signer_binding.as_ref().ok_or(error)?;
        let member = vault_keys
            .iter()
            .any(|(xpub, fp)| *xpub == binding.xpub && *fp == binding.fingerprint);
        if binding
            .key_id
            .parse::<u64>()
            .ok()
            .filter(|id| *id > 0)
            .is_none()
            || binding.descriptor_sha256.len() != 32
            || hex::encode(&binding.descriptor_sha256) != descriptor_sha256_hex
            || self.vault_fingerprint.to_string() != hex::encode(&binding.descriptor_sha256[..4])
            || !member
        {
            return Err(error.into());
        }
        if let Some(outdated) = self.chain_refusal(chain) {
            return Err(outdated);
        }
        Ok(binding)
    }

    /// Why this row is not used for a Vault on `chain`, as far as the chain
    /// rule goes (contract R2.6 and R2.7), or `None`. The settings list shows
    /// it for a row of the loaded Vault; [`Self::exact_signer_against`]
    /// applies the same rule.
    pub fn chain_refusal(&self, chain: ChainId) -> Option<String> {
        if let Err(message) = self.check_paired_chain(chain) {
            return Some(message);
        }
        (chain.is_blake2b() && !crate::mobile_signing::btcb2_signing_capable(&self.capabilities))
            .then(|| BTCB2_PHONE_OUTDATED.to_string())
    }

    /// The pairing's chain against the Vault's: a pre-v3 row is never used
    /// (R2.6, legacy variant), and a row for another chain is not either
    /// (R2.7). Shared by the Vault and imported predicates.
    fn check_paired_chain(&self, chain: ChainId) -> Result<(), String> {
        use crate::chain::ChainIdExt;
        match self.network.as_deref() {
            None => Err(PRE_V3_PAIRING.into()),
            Some(net) if net == chain.api_str() => Ok(()),
            Some(net) => Err(match ChainId::from_api_str(net) {
                Some(other) => coincube_ui::localized_format!(
                    "Paired for {}. Pair again to use it here.",
                    "Emparejado para {}. Empareja de nuevo para usarlo aquí.",
                    other.label()
                ),
                None => coincube_ui::localized_format!(
                    "Paired for another network. Pair again to use it here.",
                    "Emparejado para otra red. Empareja de nuevo para usarlo aquí."
                ),
            }),
        }
    }
}

/// Top-level on-disk layout. Wrapped in a struct so we can grow the
/// file (e.g. add a global "auto-accept new phones" flag) without
/// breaking the JSON shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PairingStoreFile {
    #[serde(default)]
    pub phones: Vec<PairedPhone>,
    /// Phones the user removed here that haven't been told yet. Removal
    /// takes effect locally at once; the phone learns it from an
    /// `Unpaired` frame the next time it is reachable, so it stops listing
    /// this desktop too. Grants nothing: these entries are never dialled
    /// for signing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_unpairs: Vec<PendingUnpair>,
}

impl PairingStoreFile {
    /// Decides whether a live hardware-list handle may be reused for the
    /// current imported wallet. A valid newly paired row is not sufficient:
    /// the handle captured the old row and old policy when it was dialled.
    pub fn cached_imported_matches(
        &self,
        captured: &PairedPhone,
        captured_policy: &PhonePolicy,
        current_policy: &PhonePolicy,
    ) -> bool {
        captured_policy.same_imported_authority(current_policy)
            && self.unique_signer_for(captured, current_policy).is_ok()
    }

    /// A hand-edited or partially migrated store with duplicate TLS identities
    /// has no unambiguous current row. Never choose whichever entry appears
    /// first when deciding imported signing authority.
    pub fn unique_signer_for<'a>(
        &self,
        phone: &'a PairedPhone,
        policy: &PhonePolicy,
    ) -> Result<&'a SignerBinding, String> {
        if matches!(policy.kind(), PhonePolicyKind::Vault(_)) {
            return phone.exact_signer_for(policy);
        }
        let mut matching = self
            .phones
            .iter()
            .filter(|row| row.cert_pin == phone.cert_pin);
        let current = matching
            .next()
            .ok_or("This phone is no longer paired; pair it again.")?;
        let revision = phone
            .signer_binding
            .as_ref()
            .and_then(|binding| binding.imported_scope.as_ref())
            .and_then(|scope| scope.pairing_revision.as_deref())
            .filter(|revision| !revision.is_empty())
            .ok_or("This imported phone pairing predates exact admission; pair it again.")?;
        if matching.next().is_some()
            || current
                .signer_binding
                .as_ref()
                .and_then(|binding| binding.imported_scope.as_ref())
                .and_then(|scope| scope.pairing_revision.as_deref())
                != Some(revision)
            || current.vault_fingerprint != phone.vault_fingerprint
            || current.transport_pubkey != phone.transport_pubkey
            || current.signer_binding != phone.signer_binding
        {
            return Err("This phone pairing is ambiguous; pair it again.".into());
        }
        let binding = phone.exact_signer_for(policy)?;
        if self
            .phones
            .iter()
            .filter_map(|row| row.exact_signer_for(policy).ok())
            .filter(|candidate| candidate.key_id == binding.key_id)
            .count()
            != 1
        {
            return Err(
                "This imported Mobile key ID has more than one pairing; pair again.".into(),
            );
        }
        Ok(binding)
    }
}

/// A removal still to be delivered to the phone it concerns. Carries only
/// what reaching that phone again needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingUnpair {
    pub cert_pin: [u8; 32],
    pub name: String,
    pub fallback_addr: Option<String>,
    pub removed_at_unix: u64,
}

/// How long a removal keeps waiting for its phone. A phone that stays away
/// longer still holds a pairing this desktop no longer dials, which the user
/// can remove on the phone.
pub const PENDING_UNPAIR_TTL_SECS: u64 = 14 * 24 * 60 * 60;

impl PendingUnpair {
    pub fn is_expired(&self, now_unix: u64) -> bool {
        now_unix.saturating_sub(self.removed_at_unix) > PENDING_UNPAIR_TTL_SECS
    }
}

pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// JSON file name under [`CoincubeDirectory`]. Top-level (not under
/// a per-network subdir) so the same paired phone can sign for any
/// network that wallet supports.
const STORE_FILENAME: &str = "paired-phones.json";

fn store_path(dir: &CoincubeDirectory) -> PathBuf {
    dir.path().join(STORE_FILENAME)
}

/// Load the paired-phones list. Returns an empty list if the file
/// doesn't exist yet (i.e. nothing has ever been paired).
pub fn load(dir: &CoincubeDirectory) -> std::io::Result<PairingStoreFile> {
    let _guard = super::pairing_transaction::WRITER.lock().unwrap();
    load_visible(dir)
}
pub(super) fn load_visible(dir: &CoincubeDirectory) -> std::io::Result<PairingStoreFile> {
    super::pairing_transaction::visible(dir, load_raw(dir)?)
}
pub(super) fn load_raw(dir: &CoincubeDirectory) -> std::io::Result<PairingStoreFile> {
    match std::fs::read(store_path(dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PairingStoreFile::default()),
        Err(e) => Err(e),
    }
}

/// Atomically replace the paired-phones list on disk.
pub fn save(dir: &CoincubeDirectory, file: &PairingStoreFile) -> std::io::Result<()> {
    write_durable(
        &store_path(dir),
        &serde_json::to_vec_pretty(file).map_err(std::io::Error::other)?,
    )
}

pub(super) fn write_durable(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(tmp, path)?;
    #[cfg(unix)]
    std::fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

/// Append (or replace by cert pin) a paired-phone record and persist.
pub fn upsert(dir: &CoincubeDirectory, phone: PairedPhone) -> std::io::Result<PairingStoreFile> {
    let _guard = super::pairing_transaction::WRITER.lock().unwrap();
    super::pairing_transaction::revoke(dir, &phone.cert_pin)?;
    let mut file = load_visible(dir)?;
    // Pairing again supersedes an undelivered removal of the same phone.
    file.pending_unpairs
        .retain(|p| p.cert_pin != phone.cert_pin);
    if let Some(existing) = file
        .phones
        .iter_mut()
        .find(|p| p.cert_pin == phone.cert_pin)
    {
        *existing = phone;
    } else {
        file.phones.push(phone);
    }
    save(dir, &file)?;
    Ok(file)
}

/// Persist a freshly completed pairing, preserving the user-customised
/// `name` and `fallback_addr` from any prior row with the same
/// `cert_pin`. Returns the merged row that was written.
///
/// Re-pairing the same phone must not clobber a manual fallback
/// `host:port` (set in the settings panel for mDNS-blocked networks)
/// or a user-applied rename: the fresh `run_pairing` result only
/// carries phone-reported defaults, so it can't be the source of
/// truth for fields the desktop user has since edited. A load error
/// while looking up the prior row is harmless — `upsert` below would
/// surface the same I/O failure, and on a first-time pairing there's
/// no prior row to preserve anyway.
///
/// This runs inside the spawned pairing task (not the UI handler) so a
/// completed pairing is recorded even if the user navigated away from
/// the panel before the handshake finished — the phone is committed
/// once `run_pairing` returns `Ok`, so the desktop must record it too
/// or the two sides drift into a half-paired state.
pub fn upsert_preserving_user_fields(
    dir: &CoincubeDirectory,
    fresh: PairedPhone,
) -> std::io::Result<PairedPhone> {
    let merged = match load(dir).ok().and_then(|file| {
        file.phones
            .into_iter()
            .find(|e| e.cert_pin == fresh.cert_pin)
    }) {
        Some(existing) => PairedPhone {
            fallback_addr: existing.fallback_addr,
            name: existing.name,
            ..fresh
        },
        None => fresh,
    };
    upsert(dir, merged.clone())?;
    Ok(merged)
}

/// User edits revoke a provisional writer and update only user-owned fields
/// on the latest durable binding, never a stale UI copy of the whole store.
pub fn update_user_fields(
    dir: &CoincubeDirectory,
    pin: &[u8; 32],
    name: String,
    fallback: Option<String>,
) -> std::io::Result<()> {
    let _guard = super::pairing_transaction::WRITER.lock().unwrap();
    super::pairing_transaction::revoke(dir, pin)?;
    let mut file = load_visible(dir)?;
    let row = file
        .phones
        .iter_mut()
        .find(|p| &p.cert_pin == pin)
        .ok_or_else(|| std::io::Error::other("Paired phone was removed"))?;
    row.name = name;
    row.fallback_addr = fallback;
    save(dir, &file)
}

/// Remove a paired phone by cert pin. No-op if not present.
pub fn remove(dir: &CoincubeDirectory, cert_pin: &[u8; 32]) -> std::io::Result<PairingStoreFile> {
    let _guard = super::pairing_transaction::WRITER.lock().unwrap();
    super::pairing_transaction::revoke(dir, cert_pin)?;
    let mut file = load_visible(dir)?;
    file.phones.retain(|p| &p.cert_pin != cert_pin);
    save(dir, &file)?;
    Ok(file)
}

/// The user removed this phone here: drop its row at once and queue an
/// `Unpaired` notice for the phone, delivered by
/// [`super::unpair_sync`] whenever the phone is next reachable. No-op if the
/// phone isn't paired.
pub fn unpair(dir: &CoincubeDirectory, cert_pin: &[u8; 32]) -> std::io::Result<PairingStoreFile> {
    let _guard = super::pairing_transaction::WRITER.lock().unwrap();
    super::pairing_transaction::revoke(dir, cert_pin)?;
    // After `revoke` this phone's raw row is its visible row. Editing the raw
    // file leaves any other phone's in-progress pairing untouched.
    let mut file = load_raw(dir)?;
    let Some(index) = file.phones.iter().position(|p| &p.cert_pin == cert_pin) else {
        return load_visible(dir);
    };
    let removed = file.phones.remove(index);
    file.pending_unpairs.retain(|p| &p.cert_pin != cert_pin);
    file.pending_unpairs.push(PendingUnpair {
        cert_pin: removed.cert_pin,
        name: removed.name,
        fallback_addr: removed.fallback_addr,
        removed_at_unix: now_unix(),
    });
    save(dir, &file)?;
    load_visible(dir)
}

/// A persisted store may contain duplicate full pins. Neither ordering can
/// authorize a Vault-scoped removal.
fn unique_phone_index_for_vault(
    file: &PairingStoreFile,
    cert_pin: &[u8; 32],
    expected_vault: Fingerprint,
    expected_descriptor: &[u8; 32],
) -> Option<usize> {
    let mut matches = file
        .phones
        .iter()
        .enumerate()
        .filter(|(_, phone)| &phone.cert_pin == cert_pin);
    let (index, phone) = matches.next()?;
    if matches.next().is_some()
        || phone.vault_fingerprint != expected_vault
        || phone.signer_binding.as_ref().is_none_or(|binding| {
            binding.descriptor_sha256.as_slice() != expected_descriptor.as_slice()
        })
    {
        return None;
    }
    Some(index)
}

/// Remove a phone only while its durable binding still belongs to the Vault
/// that requested removal. The check and removal share the writer lock with
/// `upsert`, so a re-pair cannot move this cert pin between them.
pub fn unpair_if_vault(
    dir: &CoincubeDirectory,
    cert_pin: &[u8; 32],
    expected_vault: Fingerprint,
    expected_descriptor: &[u8; 32],
) -> std::io::Result<bool> {
    let _guard = super::pairing_transaction::WRITER.lock().unwrap();
    // Refuse duplicate full pins, including one belonging to another Vault.
    // Check both the visible binding and raw disk rows before revoking any
    // provisional transaction for this pin.
    if unique_phone_index_for_vault(
        &load_visible(dir)?,
        cert_pin,
        expected_vault,
        expected_descriptor,
    )
    .is_none()
        || unique_phone_index_for_vault(
            &load_raw(dir)?,
            cert_pin,
            expected_vault,
            expected_descriptor,
        )
        .is_none()
    {
        return Ok(false);
    }

    super::pairing_transaction::revoke(dir, cert_pin)?;
    let mut file = load_raw(dir)?;
    let index = unique_phone_index_for_vault(&file, cert_pin, expected_vault, expected_descriptor)
        .ok_or_else(|| std::io::Error::other("paired phone changed during removal"))?;
    let removed = file.phones.remove(index);
    file.pending_unpairs.retain(|p| &p.cert_pin != cert_pin);
    file.pending_unpairs.push(PendingUnpair {
        cert_pin: removed.cert_pin,
        name: removed.name,
        fallback_addr: removed.fallback_addr,
        removed_at_unix: now_unix(),
    });
    save(dir, &file)?;
    Ok(true)
}

/// The phone said it removed this desktop. That carries the same authority as
/// the user removing the phone here, so like [`unpair`] it supersedes any
/// pairing journal for the phone, including one stuck after a lost final
/// acknowledgement, which would otherwise keep the row dialled forever. Only
/// the row that connection was made for is removed (the imported pairing's
/// revision disambiguates even same-second re-pairing; Vault retains its
/// existing display-time guard); for any other row nothing is touched.
/// Returns whether a row was removed.
pub fn remove_unpaired_by_peer(
    dir: &CoincubeDirectory,
    cert_pin: &[u8; 32],
    paired_at_unix: u64,
    pairing_revision: Option<&str>,
) -> std::io::Result<bool> {
    let _guard = super::pairing_transaction::WRITER.lock().unwrap();
    let dialled = |p: &PairedPhone| {
        let imported_scope = p
            .signer_binding
            .as_ref()
            .and_then(|binding| binding.imported_scope.as_ref());
        let same_admission = match (imported_scope, pairing_revision) {
            (None, None) => true,
            (Some(scope), Some(revision)) if !revision.is_empty() => {
                scope.pairing_revision.as_deref() == Some(revision)
            }
            _ => false,
        };
        &p.cert_pin == cert_pin && p.paired_at_unix == paired_at_unix && same_admission
    };
    if !load_visible(dir)?.phones.iter().any(dialled) {
        return Ok(false);
    }
    super::pairing_transaction::revoke(dir, cert_pin)?;
    // After `revoke` this phone's raw row is its visible row. Editing the raw
    // file leaves any other phone's in-progress pairing untouched.
    let mut file = load_raw(dir)?;
    file.phones.retain(|p| !dialled(p));
    // The phone already knows; nothing left to tell it.
    file.pending_unpairs.retain(|p| &p.cert_pin != cert_pin);
    save(dir, &file)?;
    Ok(true)
}

/// Forget queued removal notices: delivered ones and ones past
/// [`PENDING_UNPAIR_TTL_SECS`]. A notice is matched on its removal time too,
/// so one queued again while the first was being delivered stays queued.
pub fn clear_pending_unpairs(
    dir: &CoincubeDirectory,
    done: &[([u8; 32], u64)],
) -> std::io::Result<()> {
    if done.is_empty() {
        return Ok(());
    }
    let _guard = super::pairing_transaction::WRITER.lock().unwrap();
    let mut file = load_raw(dir)?;
    file.pending_unpairs
        .retain(|p| !done.contains(&(p.cert_pin, p.removed_at_unix)));
    save(dir, &file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::bip32::Fingerprint;

    /// Allocate a fresh temp `CoincubeDirectory` per test. We don't pull
    /// `tempfile` into deps for one helper — `std::env::temp_dir()` plus
    /// a uuid subfolder is sufficient and aligns with the test's
    /// single-process scope.
    fn fresh_dir() -> CoincubeDirectory {
        let mut path = std::env::temp_dir();
        path.push(format!("coincube-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("mkdir tempdir");
        CoincubeDirectory::new(path)
    }

    /// A binding whose descriptor commitment matches but whose other
    /// halves don't must NOT read as an exact signer.
    ///
    /// This is the settings-row status bug: matching `descriptor_sha256`
    /// alone rendered "Exact vault key paired" for phones that signing and
    /// the hw refresh loop reject, because `exact_signer` also requires a
    /// usable backend key id, a matching vault id, and the reported
    /// xpub/fingerprint to actually name a descriptor key.
    #[test]
    fn exact_signer_against_requires_more_than_the_descriptor_hash() {
        let digest = [7u8; 32];
        let hash_hex = hex::encode(digest);
        let vault_fp: Fingerprint = hex::encode(&digest[..4]).parse().expect("vault fp");
        let key_fp = Fingerprint::from([1, 2, 3, 4]);
        let vault_keys = vec![("xpub-selected".to_string(), key_fp)];

        let phone = |binding: SignerBinding| PairedPhone {
            signer_binding: Some(binding),
            vault_fingerprint: vault_fp,
            ..sample_phone(1)
        };
        let good = SignerBinding {
            key_id: "10".into(),
            xpub: "xpub-selected".into(),
            fingerprint: key_fp,
            descriptor_sha256: digest.to_vec(),
            imported_scope: None,
        };

        assert!(
            phone(good.clone())
                .exact_signer_against(&hash_hex, &vault_keys, ChainId::Bitcoin)
                .is_ok(),
            "a fully consistent binding is an exact signer",
        );

        // Each of these matches `descriptor_sha256` — the only thing the
        // settings row used to check — but must still be rejected.
        for (label, binding) in [
            (
                "unusable backend key id",
                SignerBinding {
                    key_id: "0".into(),
                    ..good.clone()
                },
            ),
            (
                "xpub not in the descriptor",
                SignerBinding {
                    xpub: "xpub-other".into(),
                    ..good.clone()
                },
            ),
            (
                "fingerprint of a different key",
                SignerBinding {
                    fingerprint: Fingerprint::from([9, 9, 9, 9]),
                    ..good.clone()
                },
            ),
        ] {
            assert!(
                phone(binding)
                    .exact_signer_against(&hash_hex, &vault_keys, ChainId::Bitcoin)
                    .is_err(),
                "{} must not read as an exact signer",
                label,
            );
        }

        // Same binding, but the row belongs to a different vault.
        assert!(
            PairedPhone {
                signer_binding: Some(good),
                vault_fingerprint: Fingerprint::from([0, 0, 0, 1]),
                ..sample_phone(1)
            }
            .exact_signer_against(&hash_hex, &vault_keys, ChainId::Bitcoin)
            .is_err(),
            "a vault id that doesn't match the descriptor commitment must not read as exact",
        );
    }

    fn sample_phone(seed: u8) -> PairedPhone {
        PairedPhone {
            signer_binding: None,
            cert_pin: [seed; 32],
            name: format!("Phone {}", seed),
            paired_at_unix: 1_700_000_000 + seed as u64,
            wallet_fingerprints: vec![Fingerprint::from([seed, seed, seed, seed])],
            vault_fingerprint: Fingerprint::from([0xaa, 0xbb, seed, seed]),
            transport_pubkey: Vec::new(),
            fallback_addr: if seed.is_multiple_of(2) {
                Some(format!("10.0.0.{}:8443", seed))
            } else {
                None
            },
            network: Some("mainnet".into()),
            capabilities: Vec::new(),
        }
    }

    fn bind_descriptor(phone: &mut PairedPhone, digest: [u8; 32]) {
        phone.signer_binding = Some(SignerBinding {
            key_id: "1".into(),
            xpub: "test-xpub".into(),
            fingerprint: Fingerprint::default(),
            descriptor_sha256: digest.to_vec(),
            imported_scope: None,
        });
    }

    #[test]
    fn pending_rollback_preserves_exact_prior_and_stale_cleanup_cannot_delete_new_run() {
        use super::super::pairing_transaction::PairingTransaction;
        let dir = fresh_dir();
        let mut prior = sample_phone(1);
        prior.name = "User name".into();
        prior.fallback_addr = Some("192.0.2.1:1234".into());
        upsert(&dir, prior.clone()).unwrap();
        let old = PairingTransaction::prepare(&dir, "old".into(), sample_phone(1)).unwrap();
        old.write_candidate().unwrap();
        assert_eq!(
            serde_json::to_value(&load(&dir).unwrap().phones[0]).unwrap(),
            serde_json::to_value(&prior).unwrap()
        );
        let new = PairingTransaction::prepare(&dir, "new".into(), sample_phone(1)).unwrap();
        old.rollback().unwrap();
        assert!(old.write_candidate().is_err());
        new.write_candidate().unwrap();
        new.rollback().unwrap();
        assert_eq!(
            serde_json::to_value(&load(&dir).unwrap().phones[0]).unwrap(),
            serde_json::to_value(&prior).unwrap()
        );
    }
    /// Master decision 21: a `paired-phones.json` written before v3 (rows
    /// without `network` or `capabilities`) loads, and every row stays
    /// listed, is refused with the pre-v3 copy, and can be removed with the
    /// phone told as today.
    #[test]
    fn a_file_with_pre_v3_rows_loads_with_every_row_refused_and_removable() {
        use sha2::{Digest, Sha256};
        let dir = fresh_dir();
        let descriptor = "wsh(or_d(multi(2,[ffd63c8d/48'/1'/0'/2']tpubDExA3EC3iAsPxPhFn4j6gMiVup6V2eH3qKyk69RcTc9TTNRfFYVPad8bJD5FCHVQxyBT4izKsvr7Btd2R4xmQ1hZkvsqGBaeE82J71uTK4N/<0;1>/*,[de6eb005/48'/1'/0'/2']tpubDFGuYfS2JwiUSEXiQuNGdT3R7WTDhbaE6jbUhgYSSdhmfQcSx7ZntMPPv7nrkvAqjpj3jX9wbhSGMeKVao4qAzhbNyBi7iQmv5xxQk6H6jz/<0;1>/*),and_v(v:pkh([ffd63c8d/48'/1'/0'/2']tpubDExA3EC3iAsPxPhFn4j6gMiVup6V2eH3qKyk69RcTc9TTNRfFYVPad8bJD5FCHVQxyBT4izKsvr7Btd2R4xmQ1hZkvsqGBaeE82J71uTK4N/<2;3>/*),older(3))))#p9ax3xxp";
        let digest: [u8; 32] = Sha256::digest(descriptor.as_bytes()).into();
        let mut v2_rows = Vec::new();
        for seed in [5u8, 6] {
            let mut phone = sample_phone(seed);
            bind_descriptor(&mut phone, digest);
            phone.vault_fingerprint = hex::encode(&digest[..4]).parse().unwrap();
            let mut json = serde_json::to_value(&phone).unwrap();
            let row = json.as_object_mut().unwrap();
            row.remove("network");
            row.remove("capabilities");
            v2_rows.push(json);
        }
        std::fs::write(
            store_path(&dir),
            serde_json::to_vec(&serde_json::json!({ "phones": v2_rows })).unwrap(),
        )
        .unwrap();
        let file = load(&dir).unwrap();
        assert_eq!(file.phones.len(), 2, "pre-v3 rows stay listed");
        let vault_keys: Vec<(String, Fingerprint)> = Vec::new();
        for phone in &file.phones {
            assert_eq!(phone.network, None);
            assert!(phone.capabilities.is_empty());
            assert_eq!(
                phone
                    .exact_signer(descriptor, ChainId::Bitcoin)
                    .unwrap_err(),
                PRE_V3_PAIRING
            );
            assert_eq!(
                phone
                    .exact_signer_against(&hex::encode(digest), &vault_keys, ChainId::Bitcoin)
                    .unwrap_err(),
                PRE_V3_PAIRING
            );
        }
        // Removable, and the phone is told, as today.
        let removed = file.phones[0].cert_pin;
        let after = unpair(&dir, &removed).unwrap();
        assert_eq!(after.phones.len(), 1);
        assert_eq!(after.pending_unpairs.len(), 1);
        assert_eq!(after.pending_unpairs[0].cert_pin, removed);
        // Nothing was written into the store's rows besides the removal.
        assert_eq!(load(&dir).unwrap().phones[0].network, None);
    }

    /// One pairing per phone and desktop (master decision 19): pairing the
    /// same phone for another chain replaces its row, keeps the user's name
    /// and fallback address, and records the new chain and capabilities.
    #[test]
    fn pairing_for_another_chain_replaces_the_row_and_keeps_the_users_name() {
        use super::super::pairing_transaction::PairingTransaction;
        let dir = fresh_dir();
        let mut bitcoin = sample_phone(8);
        bitcoin.name = "Alice's phone".into();
        bitcoin.fallback_addr = Some("192.0.2.8:1234".into());
        upsert(&dir, bitcoin.clone()).unwrap();

        let mut btcb2 = sample_phone(8);
        btcb2.name = "Pixel".into();
        btcb2.fallback_addr = None;
        btcb2.network = Some("bitcoin-blake2b".into());
        btcb2.capabilities = vec!["chain-identity-v1".into(), "btcb2-unified-v2".into()];
        let transaction = PairingTransaction::prepare(&dir, "btcb2".into(), btcb2.clone()).unwrap();
        transaction.write_candidate().unwrap();
        transaction.decide().unwrap();
        transaction.finish().unwrap();

        let file = load(&dir).unwrap();
        assert_eq!(file.phones.len(), 1, "one row per phone");
        let row = &file.phones[0];
        assert_eq!(row.network.as_deref(), Some("bitcoin-blake2b"));
        assert_eq!(row.capabilities, btcb2.capabilities);
        assert_eq!(row.name, "Alice's phone");
        assert_eq!(row.fallback_addr.as_deref(), Some("192.0.2.8:1234"));
    }

    #[test]
    fn the_pairing_screen_says_which_pairing_a_new_one_replaces() {
        let mut phone = sample_phone(9);
        phone.network = Some("mainnet".into());
        assert_eq!(phone.replaced_by_pairing_for(ChainId::Bitcoin), None);
        assert_eq!(
            phone
                .replaced_by_pairing_for(ChainId::BitcoinBlake2b)
                .unwrap(),
            "Paired for Bitcoin. Pairing it here replaces that pairing."
        );
        phone.network = Some("bitcoin-blake2b-testnet4".into());
        assert_eq!(
            phone.replaced_by_pairing_for(ChainId::Testnet4).unwrap(),
            "Paired for Bitcoin Blake2b Testnet4. Pairing it here replaces that pairing."
        );
        phone.network = None;
        assert!(phone
            .replaced_by_pairing_for(ChainId::Bitcoin)
            .unwrap()
            .contains("before network-aware signing"));
        phone.network = Some("future-chain".into());
        assert!(phone
            .replaced_by_pairing_for(ChainId::Bitcoin)
            .unwrap()
            .contains("another network"));
    }

    #[test]
    fn unpair_queues_one_notice_and_pairing_again_drops_it() {
        let dir = fresh_dir();
        let phone = sample_phone(2);
        upsert(&dir, phone.clone()).unwrap();
        upsert(&dir, sample_phone(3)).unwrap();
        unpair(&dir, &phone.cert_pin).unwrap();
        unpair(&dir, &phone.cert_pin).unwrap(); // already gone: no second notice
        let file = load(&dir).unwrap();
        assert_eq!(file.phones.len(), 1);
        assert_eq!(file.pending_unpairs.len(), 1);
        assert_eq!(file.pending_unpairs[0].cert_pin, phone.cert_pin);
        assert_eq!(file.pending_unpairs[0].fallback_addr, phone.fallback_addr);

        upsert(&dir, phone.clone()).unwrap();
        assert!(load(&dir).unwrap().pending_unpairs.is_empty());

        // A pairing started again supersedes the notice before it completes:
        // delivering it mid-handshake would undo the new pairing.
        unpair(&dir, &phone.cert_pin).unwrap();
        let _staged = super::super::pairing_transaction::PairingTransaction::prepare(
            &dir,
            "again".into(),
            phone.clone(),
        )
        .unwrap();
        assert!(load(&dir).unwrap().pending_unpairs.is_empty());
    }

    #[test]
    fn refused_vault_unpair_keeps_an_unrelated_provisional_pairing() {
        use super::super::pairing_transaction::PairingTransaction;

        let dir = fresh_dir();
        let mut original = sample_phone(44);
        let original_digest = [0u8; 32];
        bind_descriptor(&mut original, original_digest);
        upsert(&dir, original.clone()).unwrap();
        let mut candidate = original.clone();
        candidate.vault_fingerprint = Fingerprint::from([1, 2, 3, 4]);
        let candidate_digest = [1u8; 32];
        bind_descriptor(&mut candidate, candidate_digest);
        let transaction =
            PairingTransaction::prepare(&dir, "new-binding".into(), candidate.clone()).unwrap();
        transaction.write_candidate().unwrap();

        // The provisional candidate is on disk, but the visible binding is
        // still the prior Vault. A refusal must leave its journal intact.
        assert!(!unpair_if_vault(
            &dir,
            &original.cert_pin,
            candidate.vault_fingerprint,
            &candidate_digest,
        )
        .unwrap());
        transaction.decide().unwrap();
        transaction.finish().unwrap();
        let stored = load(&dir).unwrap();
        assert_eq!(stored.phones.len(), 1);
        assert_eq!(
            stored.phones[0].vault_fingerprint,
            candidate.vault_fingerprint
        );
        assert!(stored.pending_unpairs.is_empty());
    }

    #[test]
    fn vault_unpair_refuses_duplicate_full_pins_in_either_order() {
        let dir = fresh_dir();
        let mut vault_a = sample_phone(45);
        let digest_a = [0u8; 32];
        bind_descriptor(&mut vault_a, digest_a);
        let mut vault_b = vault_a.clone();
        vault_b.vault_fingerprint = Fingerprint::from([1, 2, 3, 4]);
        bind_descriptor(&mut vault_b, [1u8; 32]);
        for phones in [
            vec![vault_b.clone(), vault_a.clone()],
            vec![vault_a.clone(), vault_b.clone()],
        ] {
            save(
                &dir,
                &PairingStoreFile {
                    phones: phones.clone(),
                    ..PairingStoreFile::default()
                },
            )
            .unwrap();
            assert!(!unpair_if_vault(
                &dir,
                &vault_a.cert_pin,
                vault_a.vault_fingerprint,
                &digest_a,
            )
            .unwrap());
            let stored = load(&dir).unwrap();
            assert_eq!(stored.phones.len(), 2);
            assert_eq!(
                stored.phones[0].vault_fingerprint,
                phones[0].vault_fingerprint
            );
            assert_eq!(
                stored.phones[1].vault_fingerprint,
                phones[1].vault_fingerprint
            );
            assert!(stored.pending_unpairs.is_empty());
        }
    }

    #[test]
    fn peer_unpair_removes_only_the_row_it_was_made_for() {
        let dir = fresh_dir();
        let phone = sample_phone(4);
        upsert(&dir, phone.clone()).unwrap();
        // A connection from an earlier pairing of the same phone.
        assert!(
            !remove_unpaired_by_peer(&dir, &phone.cert_pin, phone.paired_at_unix - 1, None)
                .unwrap()
        );
        assert_eq!(load(&dir).unwrap().phones.len(), 1);

        assert!(
            remove_unpaired_by_peer(&dir, &phone.cert_pin, phone.paired_at_unix, None).unwrap()
        );
        assert!(load(&dir).unwrap().phones.is_empty());
    }

    /// A re-pair whose FINISHED was lost leaves a decided journal for good,
    /// with the earlier pairing still visible and dialled. The phone's removal
    /// must still apply, or that row is redialled every refresh tick forever.
    #[test]
    fn peer_unpair_supersedes_a_pairing_stuck_after_finish() {
        use super::super::pairing_transaction::PairingTransaction;
        let dir = fresh_dir();
        let phone = sample_phone(4);
        upsert(&dir, phone.clone()).unwrap();
        let repaired = PairedPhone {
            paired_at_unix: phone.paired_at_unix + 100,
            ..phone.clone()
        };
        let stuck = PairingTransaction::prepare(&dir, "lost-finished".into(), repaired).unwrap();
        stuck.write_candidate().unwrap();
        stuck.decide().unwrap();
        drop(stuck); // Decided: drop leaves it hidden, it never rolls back.
        assert_eq!(
            load(&dir).unwrap().phones[0].paired_at_unix,
            phone.paired_at_unix,
            "the earlier pairing is what stays visible and dialled",
        );

        assert!(
            remove_unpaired_by_peer(&dir, &phone.cert_pin, phone.paired_at_unix, None).unwrap()
        );
        assert!(load(&dir).unwrap().phones.is_empty());
        assert!(
            load_raw(&dir).unwrap().phones.is_empty(),
            "the hidden candidate goes too"
        );
    }

    #[test]
    fn stores_without_queued_removals_keep_their_shape() {
        let dir = fresh_dir();
        // A store written before `pending_unpairs` existed still loads.
        std::fs::write(store_path(&dir), br#"{"phones":[]}"#).unwrap();
        assert!(load(&dir).unwrap().pending_unpairs.is_empty());
        // And an empty queue isn't written, so older builds read it as before.
        upsert(&dir, sample_phone(5)).unwrap();
        let raw = std::fs::read_to_string(store_path(&dir)).unwrap();
        assert!(!raw.contains("pending_unpairs"), "{}", raw);
    }

    #[test]
    fn queued_removal_expires() {
        let pending = PendingUnpair {
            cert_pin: [1; 32],
            name: "x".into(),
            fallback_addr: None,
            removed_at_unix: 1_000,
        };
        assert!(!pending.is_expired(1_000 + PENDING_UNPAIR_TTL_SECS));
        assert!(pending.is_expired(1_001 + PENDING_UNPAIR_TTL_SECS));
    }

    #[test]
    fn unpair_and_rename_revoke_pending_writers() {
        use super::super::pairing_transaction::PairingTransaction;
        let dir = fresh_dir();
        upsert(&dir, sample_phone(1)).unwrap();
        let pending = PairingTransaction::prepare(&dir, "pending".into(), sample_phone(1)).unwrap();
        pending.write_candidate().unwrap();
        remove(&dir, &[1; 32]).unwrap();
        assert!(pending.write_candidate().is_err());
        pending.rollback().unwrap();
        assert!(load(&dir).unwrap().phones.is_empty());
        upsert(&dir, sample_phone(1)).unwrap();
        let pending = PairingTransaction::prepare(&dir, "next".into(), sample_phone(1)).unwrap();
        let mut renamed = sample_phone(1);
        renamed.name = "New user name".into();
        upsert(&dir, renamed).unwrap();
        assert!(pending.finish().is_err());
        pending.rollback().unwrap();
        assert_eq!(load(&dir).unwrap().phones[0].name, "New user name");
    }
    #[test]
    fn storage_failure_leaves_no_trusted_candidate() {
        use super::super::pairing_transaction::PairingTransaction;
        let dir = fresh_dir();
        std::fs::create_dir(dir.path().join("pairing-transactions.json.tmp")).unwrap();
        assert!(PairingTransaction::prepare(&dir, "failed".into(), sample_phone(1)).is_err());
        assert!(load(&dir).unwrap().phones.is_empty());
    }

    #[test]
    fn irreversible_decision_stays_hidden_and_survives_drop() {
        use super::super::{pairing_run::PairingRun, pairing_transaction::PairingTransaction};
        let dir = fresh_dir();
        let prior = sample_phone(1);
        upsert(&dir, prior.clone()).unwrap();
        let mut candidate = prior.clone();
        candidate.paired_at_unix += 100;
        let transaction = PairingTransaction::prepare(&dir, "decision".into(), candidate).unwrap();
        transaction.write_candidate().unwrap();
        let run = PairingRun::default();
        run.decide(|| {
            transaction
                .decide()
                .map_err(|e| super::super::errors::PairingError::InternalError(e.to_string()))
        })
        .unwrap();
        assert_eq!(
            load(&dir).unwrap().phones[0].paired_at_unix,
            prior.paired_at_unix,
            "decision must remain hidden until FINISHED"
        );
        run.cancel();
        run.check().unwrap();
        drop(transaction);
        assert!(
            std::fs::read_to_string(dir.path().join("pairing-transactions.json"))
                .unwrap()
                .contains("decision")
        );
        assert_eq!(
            load(&dir).unwrap().phones[0].paired_at_unix,
            prior.paired_at_unix
        );
    }

    #[test]
    fn decided_storage_failure_and_reload_never_expose_or_rollback() {
        use super::super::pairing_transaction::PairingTransaction;
        let dir = fresh_dir();
        let transaction =
            PairingTransaction::prepare(&dir, "hidden".into(), sample_phone(1)).unwrap();
        transaction.write_candidate().unwrap();
        transaction.decide().unwrap();
        let decided = std::fs::read(dir.path().join("pairing-transactions.json")).unwrap();
        std::fs::create_dir(dir.path().join("pairing-transactions.json.tmp")).unwrap();
        assert!(transaction.finish().is_err());
        transaction.rollback().unwrap();
        drop(transaction);
        let reloaded = CoincubeDirectory::new(dir.path().to_path_buf());
        assert!(load(&reloaded).unwrap().phones.is_empty());
        assert_eq!(
            std::fs::read(dir.path().join("pairing-transactions.json")).unwrap(),
            decided
        );
        assert_eq!(load_raw(&dir).unwrap().phones.len(), 1);
    }

    #[tokio::test]
    async fn task_abort_before_and_after_decision_respects_durable_state() {
        use super::super::pairing_transaction::PairingTransaction;
        for decided in [false, true] {
            let dir = fresh_dir();
            let task_dir = dir.clone();
            let (ready, reached) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                let transaction =
                    PairingTransaction::prepare(&task_dir, "abort".into(), sample_phone(1))
                        .unwrap();
                transaction.write_candidate().unwrap();
                if decided {
                    transaction.decide().unwrap();
                }
                ready.send(()).unwrap();
                std::future::pending::<()>().await;
                drop(transaction);
            });
            reached.await.unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            assert!(load(&dir).unwrap().phones.is_empty());
            assert_eq!(load_raw(&dir).unwrap().phones.len(), usize::from(decided));
            assert_eq!(
                std::fs::read_to_string(dir.path().join("pairing-transactions.json"))
                    .unwrap()
                    .contains("abort"),
                decided
            );
        }
    }

    #[test]
    fn only_finished_acknowledgement_exposes_new_binding() {
        use super::super::{pairing_run::PairingRun, pairing_transaction::PairingTransaction};
        let dir = fresh_dir();
        let transaction =
            PairingTransaction::prepare(&dir, "complete".into(), sample_phone(1)).unwrap();
        transaction.write_candidate().unwrap();
        assert!(transaction.finish().is_err());
        let run = PairingRun::default();
        run.decide(|| {
            transaction
                .decide()
                .map_err(|e| super::super::errors::PairingError::InternalError(e.to_string()))
        })
        .unwrap();
        run.cancel();
        run.check().unwrap();
        assert!(load(&dir).unwrap().phones.is_empty());
        transaction.finish().unwrap();
        transaction.rollback().unwrap();
        drop(transaction);
        assert_eq!(load(&dir).unwrap().phones.len(), 1);
    }

    #[test]
    fn cancellation_before_decision_restores_exact_prior() {
        use super::super::{pairing_run::PairingRun, pairing_transaction::PairingTransaction};
        let dir = fresh_dir();
        let prior = sample_phone(1);
        upsert(&dir, prior.clone()).unwrap();
        let transaction =
            PairingTransaction::prepare(&dir, "cancel".into(), sample_phone(1)).unwrap();
        transaction.write_candidate().unwrap();
        let run = PairingRun::default();
        run.cancel();
        assert!(run
            .decide(|| transaction
                .decide()
                .map_err(|e| super::super::errors::PairingError::InternalError(e.to_string())))
            .is_err());
        drop(transaction);
        assert_eq!(
            serde_json::to_value(&load(&dir).unwrap().phones[0]).unwrap(),
            serde_json::to_value(prior).unwrap()
        );
    }

    #[test]
    fn legacy_finished_journal_stays_irreversible_when_another_record_changes() {
        use super::super::pairing_transaction::PairingTransaction;
        let dir = fresh_dir();
        let transaction =
            PairingTransaction::prepare(&dir, "legacy".into(), sample_phone(1)).unwrap();
        transaction.write_candidate().unwrap();
        let path = dir.path().join("pairing-transactions.json");
        let mut entries: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let entry = entries[hex::encode([1; 32])].as_object_mut().unwrap();
        entry.remove("state");
        entry.insert("finished".into(), true.into());
        std::fs::write(&path, serde_json::to_vec(&entries).unwrap()).unwrap();
        assert!(load(&dir).unwrap().phones.is_empty());
        let other = PairingTransaction::prepare(&dir, "other".into(), sample_phone(2)).unwrap();
        drop(other);
        drop(transaction);
        assert!(std::fs::read_to_string(path).unwrap().contains("legacy"));
        assert!(load(&dir).unwrap().phones.is_empty());
        assert_eq!(load_raw(&dir).unwrap().phones.len(), 1);
    }

    #[test]
    fn load_on_missing_file_returns_empty() {
        let dir = fresh_dir();
        let file = load(&dir).expect("load");
        assert!(file.phones.is_empty());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = fresh_dir();
        let mut file = PairingStoreFile::default();
        file.phones.push(sample_phone(7));
        file.phones.push(sample_phone(8));
        save(&dir, &file).expect("save");
        let read = load(&dir).expect("load");
        assert_eq!(read.phones.len(), 2);
        assert_eq!(read.phones[0].cert_pin, file.phones[0].cert_pin);
        assert_eq!(read.phones[0].name, file.phones[0].name);
        assert_eq!(read.phones[1].fallback_addr, file.phones[1].fallback_addr);
        assert_eq!(
            read.phones[1].wallet_fingerprints,
            file.phones[1].wallet_fingerprints
        );
        assert_eq!(
            read.phones[1].vault_fingerprint,
            file.phones[1].vault_fingerprint
        );
    }

    #[test]
    fn upsert_adds_new_entry() {
        let dir = fresh_dir();
        upsert(&dir, sample_phone(1)).expect("upsert add");
        upsert(&dir, sample_phone(2)).expect("upsert add 2");
        let read = load(&dir).expect("load");
        assert_eq!(read.phones.len(), 2);
    }

    #[test]
    fn upsert_replaces_existing_by_cert_pin() {
        let dir = fresh_dir();
        upsert(&dir, sample_phone(1)).expect("first");
        let mut updated = sample_phone(1);
        updated.name = "Renamed".into();
        upsert(&dir, updated).expect("replace");
        let read = load(&dir).expect("load");
        assert_eq!(read.phones.len(), 1);
        assert_eq!(read.phones[0].name, "Renamed");
    }

    #[test]
    fn remove_deletes_present_entry() {
        let dir = fresh_dir();
        upsert(&dir, sample_phone(1)).expect("upsert");
        upsert(&dir, sample_phone(2)).expect("upsert 2");
        remove(&dir, &[1u8; 32]).expect("remove");
        let read = load(&dir).expect("load");
        assert_eq!(read.phones.len(), 1);
        assert_eq!(read.phones[0].cert_pin, [2u8; 32]);
    }

    /// Re-pairing the same phone (matched by cert pin) must preserve a
    /// user-applied rename and a manually-entered fallback addr, while
    /// still taking the fresh run's `paired_at_unix` and
    /// `wallet_fingerprints`. The fresh row only carries phone-reported
    /// defaults, so it can't be the source of truth for fields the
    /// desktop user has since edited.
    #[test]
    fn upsert_preserving_user_fields_keeps_rename_and_fallback() {
        let dir = fresh_dir();
        let prior = PairedPhone {
            signer_binding: None,
            cert_pin: [42u8; 32],
            name: "My Phone".into(),
            paired_at_unix: 1_700_000_000,
            wallet_fingerprints: Vec::new(),
            vault_fingerprint: Fingerprint::from([0xaa, 0xaa, 0xaa, 0xaa]),
            transport_pubkey: Vec::new(),
            fallback_addr: Some("10.0.0.5:8443".into()),
            network: Some("mainnet".into()),
            capabilities: Vec::new(),
        };
        save(
            &dir,
            &PairingStoreFile {
                phones: vec![prior],
                ..Default::default()
            },
        )
        .expect("seed store");

        let fresh = PairedPhone {
            signer_binding: None,
            cert_pin: [42u8; 32],
            name: "Pixel 8".into(),
            paired_at_unix: 1_700_999_999,
            wallet_fingerprints: vec![Fingerprint::from([1, 2, 3, 4])],
            vault_fingerprint: Fingerprint::from([0xbb, 0xbb, 0xbb, 0xbb]),
            transport_pubkey: Vec::new(),
            fallback_addr: None,
            network: Some("mainnet".into()),
            capabilities: Vec::new(),
        };
        let written = upsert_preserving_user_fields(&dir, fresh).expect("upsert");

        // Returned row reflects the merge.
        assert_eq!(written.name, "My Phone");
        assert_eq!(written.fallback_addr.as_deref(), Some("10.0.0.5:8443"));

        let on_disk = load(&dir).expect("load");
        assert_eq!(on_disk.phones.len(), 1);
        // User-customised fields preserved from the prior row.
        assert_eq!(on_disk.phones[0].name, "My Phone");
        assert_eq!(
            on_disk.phones[0].fallback_addr.as_deref(),
            Some("10.0.0.5:8443"),
        );
        // Fields that legitimately come from the fresh run survive —
        // including the vault id: re-pairing against a different vault
        // re-scopes the phone to the freshly-paired vault.
        assert_eq!(on_disk.phones[0].paired_at_unix, 1_700_999_999);
        assert_eq!(
            on_disk.phones[0].wallet_fingerprints,
            vec![Fingerprint::from([1, 2, 3, 4])],
        );
        assert_eq!(
            on_disk.phones[0].vault_fingerprint,
            Fingerprint::from([0xbb, 0xbb, 0xbb, 0xbb]),
        );
    }

    /// First-time pairing (no prior row) writes the fresh row verbatim.
    #[test]
    fn upsert_preserving_user_fields_first_pair_writes_fresh() {
        let dir = fresh_dir();
        let fresh = sample_phone(5);
        let written = upsert_preserving_user_fields(&dir, fresh.clone()).expect("upsert");
        assert_eq!(written.name, fresh.name);

        let on_disk = load(&dir).expect("load");
        assert_eq!(on_disk.phones.len(), 1);
        assert_eq!(on_disk.phones[0].cert_pin, fresh.cert_pin);
        assert_eq!(on_disk.phones[0].name, fresh.name);
    }

    #[test]
    fn remove_is_noop_when_absent() {
        let dir = fresh_dir();
        upsert(&dir, sample_phone(1)).expect("upsert");
        remove(&dir, &[42u8; 32]).expect("remove missing");
        let read = load(&dir).expect("load");
        assert_eq!(read.phones.len(), 1);
    }

    #[test]
    fn save_writes_pretty_json_no_tmp_left_behind() {
        // Atomic rename should leave only the final file on disk, no
        // .json.tmp sibling.
        let dir = fresh_dir();
        let mut file = PairingStoreFile::default();
        file.phones.push(sample_phone(3));
        save(&dir, &file).expect("save");
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("readdir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(entries.contains(&STORE_FILENAME.to_string()));
        assert!(
            !entries.iter().any(|n| n.ends_with(".tmp")),
            "tmp file leaked: {:?}",
            entries
        );
    }

    /// Regression: the in-memory field is `cert_pin` (it's a cert
    /// SHA-256, not an Ed25519 pubkey), but for backward compat
    /// with v1.0 stores the on-disk JSON key MUST stay
    /// `"identity_pubkey"`. A future serde refactor that dropped
    /// the `#[serde(rename)]` would silently invalidate every
    /// installed user's pairing store.
    #[test]
    fn on_disk_json_field_name_stays_identity_pubkey() {
        let phone = sample_phone(0xab);
        let json = serde_json::to_string(&phone).expect("serialize");
        assert!(
            json.contains("\"identity_pubkey\""),
            "on-disk JSON must keep the v1.0 field name; got {}",
            json,
        );
        assert!(
            !json.contains("\"cert_pin\""),
            "on-disk JSON must NOT leak the renamed Rust identifier; got {}",
            json,
        );
        // And round-trips: a v1.0 file using `identity_pubkey` must
        // still deserialize.
        let legacy = r#"{
            "identity_pubkey": [1,2,3,4,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
            "name": "Legacy",
            "paired_at_unix": 1700000000,
            "wallet_fingerprints": [],
            "fallback_addr": null
        }"#;
        let decoded: PairedPhone = serde_json::from_str(legacy).expect("decode legacy");
        assert_eq!(decoded.cert_pin[..4], [1, 2, 3, 4]);
        assert_eq!(decoded.name, "Legacy");
    }
}
