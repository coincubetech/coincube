//! `merge_verified_for` on a Bitcoin Blake2b Vault: unified signatures are
//! detected, verified and bound to the paired key; everything the Bitcoin
//! tests in `signature_tests.rs` pin still holds, and a unified record is
//! refused on Bitcoin.
use super::*;
use crate::app::state::vault::test_support::unified::{self, Fixture};
use coincube_core::{
    miniscript::bitcoin::{
        bip32::{ChildNumber, DerivationPath},
        secp256k1::Secp256k1,
    },
    signer::MasterSigner,
};

/// The binding a v2/v3 pairing records for `signer`'s primary-path key in
/// the fixture Vault.
fn binding(f: &Fixture, signer: &MasterSigner) -> pairing_store::SignerBinding {
    use sha2::{Digest, Sha256};
    let secp = Secp256k1::new();
    let origin = DerivationPath::from(vec![
        ChildNumber::from_hardened_idx(48).unwrap(),
        ChildNumber::from_hardened_idx(0).unwrap(),
    ]);
    pairing_store::SignerBinding {
        key_id: "10".into(),
        xpub: signer.xpub_at(&origin, &secp).to_string(),
        fingerprint: signer.fingerprint(&secp),
        descriptor_sha256: Sha256::digest(f.descriptor.to_string().as_bytes()).to_vec(),
        imported_scope: None,
    }
}

fn merge(unified: bool, f: &Fixture, target: &mut Psbt, returned: &Psbt, signer: usize) -> bool {
    signatures::merge_verified_for(
        unified,
        target,
        returned,
        &f.descriptor.to_string(),
        &binding(f, &f.signers[signer]),
    )
    .is_ok()
}

#[test]
fn a_unified_signature_from_the_bound_key_merges_on_btcb2() {
    let f = unified::fixture();
    let returned = unified::unified(&f.psbt, &f.signers[0]);
    let mut target = f.psbt.clone();
    assert!(merge(true, &f, &mut target, &returned, 0));
    // Only the unified record crossed over; the request's own fields stayed.
    assert_eq!(target.inputs[0].proprietary, returned.inputs[0].proprietary);
    assert_eq!(target.inputs[0].sighash_type, f.psbt.inputs[0].sighash_type);
    assert!(target.inputs[0].partial_sigs.is_empty());
    assert_eq!(target.unsigned_tx, f.psbt.unsigned_tx);
}

#[test]
fn a_unified_signature_is_refused_on_a_bitcoin_chain() {
    let f = unified::fixture();
    let returned = unified::unified(&f.psbt, &f.signers[0]);
    let mut target = f.psbt.clone();
    assert!(!merge(false, &f, &mut target, &returned, 0));
    assert_eq!(target, f.psbt, "nothing merged");
    // Even beside a valid legacy signature, which Bitcoin otherwise accepts.
    let mut both = unified::legacy(&f.psbt, &f.signers[0]);
    both.inputs[0].proprietary = returned.inputs[0].proprietary.clone();
    assert!(!merge(false, &f, &mut target, &both, 0));
    assert_eq!(target, f.psbt);
    // The same legacy signature alone still merges on Bitcoin.
    let legacy = unified::legacy(&f.psbt, &f.signers[0]);
    assert!(merge(false, &f, &mut target, &legacy, 0));
}

#[test]
fn a_plain_sighash_all_signature_is_refused_on_btcb2() {
    let f = unified::fixture();
    let legacy = unified::legacy(&f.psbt, &f.signers[0]);
    assert!(!legacy.inputs[0].partial_sigs.is_empty());
    let mut target = f.psbt.clone();
    assert!(!merge(true, &f, &mut target, &legacy, 0));
    assert_eq!(target, f.psbt, "nothing merged");
}

#[test]
fn a_unified_signature_for_any_key_but_the_bound_one_is_refused() {
    let f = unified::fixture();
    // Signer 1 signed, but the pairing is bound to signer 0.
    let other = unified::unified(&f.psbt, &f.signers[1]);
    let mut target = f.psbt.clone();
    assert!(!merge(true, &f, &mut target, &other, 0));
    // The bound key plus a second Vault key: one extra is enough to refuse.
    let both = unified::unified(&other, &f.signers[0]);
    assert!(!merge(true, &f, &mut target, &both, 0));
    assert_eq!(target, f.psbt);
}

#[test]
fn a_retained_unified_signature_altered_or_missing_is_refused() {
    let f = unified::fixture();
    // The request already carries signer 1's unified signature.
    let request = unified::unified(&f.psbt, &f.signers[1]);
    let returned = unified::unified(&request, &f.signers[0]);
    let mut ok = request.clone();
    assert!(merge(true, &f, &mut ok, &returned, 0));

    let retained = request.inputs[0].proprietary.keys().next().unwrap().clone();
    // Missing.
    let mut missing = returned.clone();
    missing.inputs[0].proprietary.remove(&retained);
    let mut target = request.clone();
    assert!(!merge(true, &f, &mut target, &missing, 0));
    assert_eq!(target, request);
    // Altered: another transaction's valid signature for the same key.
    let mut other_tx = f.psbt.clone();
    other_tx.unsigned_tx.lock_time =
        coincube_core::miniscript::bitcoin::absolute::LockTime::from_consensus(1);
    let foreign = unified::unified(&other_tx, &f.signers[1]);
    let mut altered = returned.clone();
    altered.inputs[0].proprietary.insert(
        retained.clone(),
        foreign.inputs[0].proprietary[&retained].clone(),
    );
    assert!(!merge(true, &f, &mut target, &altered, 0));
    assert_eq!(target, request);
}

#[test]
fn an_invalid_unified_signature_for_the_bound_key_is_refused() {
    let f = unified::fixture();
    // The bound key's valid signature, but over another transaction.
    let mut other_tx = f.psbt.clone();
    other_tx.unsigned_tx.lock_time =
        coincube_core::miniscript::bitcoin::absolute::LockTime::from_consensus(1);
    let foreign = unified::unified(&other_tx, &f.signers[0]);
    let mut forged = f.psbt.clone();
    forged.inputs[0].proprietary = foreign.inputs[0].proprietary.clone();
    let mut target = f.psbt.clone();
    assert!(!merge(true, &f, &mut target, &forged, 0));
    assert_eq!(target, f.psbt);
}

#[test]
fn a_returned_transaction_or_final_field_is_never_imported_on_btcb2() {
    let f = unified::fixture();
    let mut returned = unified::unified(&f.psbt, &f.signers[0]);
    returned.inputs[0].final_script_witness = Some(Default::default());
    let mut target = f.psbt.clone();
    // A final field the request did not have is not imported.
    assert!(merge(true, &f, &mut target, &returned, 0));
    assert!(target.inputs[0].final_script_witness.is_none());
    // A different transaction is refused outright.
    let mut other = returned.clone();
    other.unsigned_tx.lock_time =
        coincube_core::miniscript::bitcoin::absolute::LockTime::from_consensus(1);
    let mut target = f.psbt.clone();
    assert!(!merge(true, &f, &mut target, &other, 0));
}
