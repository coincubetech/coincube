//! `knots_verify <archive> <SHA256SUMS> <SHA256SUMS.asc>`
//!
//! Exit 0 iff (1) the archive's SHA-256 is listed for exactly its file name in
//! `SHA256SUMS`, and (2) `SHA256SUMS.asc` carries at least one signature that
//! verifies, over `SHA256SUMS`, against the Knots signing key vendored in
//! `coincube-gui/assets/knots_signing_key.asc` (fingerprint re-derived and
//! pinned). This is the same trust anchor and the same rule the desktop
//! installer applies (`coincube-gui/src/installer/step/node/bitcoind.rs`,
//! `verify_detached_signature` / `hash_listed_in_manifest`), so the functional
//! tests run a Knots binary the app itself would have accepted — without
//! depending on a keyserver or a system `gpg`.

use std::path::Path;
use std::process::exit;

use pgp::composed::{Deserializable, SignedPublicKey, StandaloneSignature};
use pgp::types::KeyDetails;
use sha2::{Digest, Sha256};

/// Same pin as `coincube_gui::node::bitcoind::KNOTS_SIGNING_KEY_FINGERPRINT`.
const KNOTS_SIGNING_KEY_FINGERPRINT: &str = "1A3E761F19D2CC7785C5502EA291A2C45D0C504A";
/// The vendored key itself, read from the single copy the desktop ships.
const KNOTS_SIGNING_KEY_ASC: &str =
    include_str!("../../../../coincube-gui/assets/knots_signing_key.asc");

fn hash_listed_in_manifest(bytes: &[u8], archive_filename: &str, sha256sums: &str) -> bool {
    let bytes_hash = hex::encode(Sha256::digest(bytes));
    sha256sums.lines().any(|line| {
        let mut fields = line.split_whitespace();
        matches!(
            (fields.next(), fields.next()),
            (Some(hash), Some(name))
                if name == archive_filename && hash.eq_ignore_ascii_case(&bytes_hash)
        )
    })
}

fn verify_detached_signature(data: &[u8], asc: &str) -> Result<(), String> {
    if !asc
        .trim_start()
        .starts_with("-----BEGIN PGP SIGNATURE-----")
    {
        return Err("no PGP signature block".into());
    }
    let (pubkey, _) = SignedPublicKey::from_string(KNOTS_SIGNING_KEY_ASC)
        .map_err(|e| format!("vendored key unreadable: {e}"))?;
    if !hex::encode(pubkey.fingerprint().as_bytes())
        .eq_ignore_ascii_case(KNOTS_SIGNING_KEY_FINGERPRINT)
    {
        return Err("vendored key fingerprint does not match the pin".into());
    }
    let bound_subkeys: Vec<_> = pubkey
        .public_subkeys
        .iter()
        .filter(|sub| sub.verify(&pubkey.primary_key).is_ok())
        .collect();
    let (sig_iter, _) = StandaloneSignature::from_string_many(asc)
        .map_err(|e| format!("signature block unreadable: {e}"))?;
    let signatures: Vec<StandaloneSignature> = sig_iter.flatten().collect();
    let verifies = signatures.iter().any(|sig| {
        sig.verify(&pubkey, data).is_ok()
            || bound_subkeys
                .iter()
                .any(|sub| sig.verify(*sub, data).is_ok())
    });
    if verifies {
        Ok(())
    } else {
        Err(format!(
            "none of the {} signature(s) verify against {}",
            signatures.len(),
            KNOTS_SIGNING_KEY_FINGERPRINT
        ))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: knots_verify <archive> <SHA256SUMS> <SHA256SUMS.asc>");
        exit(2);
    }
    let archive = Path::new(&args[1]);
    let archive_name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let archive_bytes = std::fs::read(archive).unwrap_or_else(|e| {
        eprintln!("cannot read {}: {e}", archive.display());
        exit(2)
    });
    let sums_bytes = std::fs::read(&args[2]).unwrap_or_else(|e| {
        eprintln!("cannot read {}: {e}", args[2]);
        exit(2)
    });
    let asc = std::fs::read_to_string(&args[3]).unwrap_or_else(|e| {
        eprintln!("cannot read {}: {e}", args[3]);
        exit(2)
    });

    if let Err(e) = verify_detached_signature(&sums_bytes, &asc) {
        eprintln!("SHA256SUMS.asc: {e}");
        exit(1);
    }
    let sums = String::from_utf8_lossy(&sums_bytes);
    if !hash_listed_in_manifest(&archive_bytes, archive_name, &sums) {
        eprintln!("{archive_name}: SHA-256 not listed in SHA256SUMS");
        exit(1);
    }
    println!(
        "ok: {archive_name} listed in SHA256SUMS; SHA256SUMS.asc verified against {}",
        KNOTS_SIGNING_KEY_FINGERPRINT
    );
}
