//! What both Mobile signing rails share: the PSBT wire codec and the BTCB2
//! capability rule.
//!
//! The Connect rail (`app/state/vault/keychain_sign.rs`) and the Wi-Fi rail
//! (`phone_signer`, `PhoneSigner::sign_tx`) send the same PSBT to the same
//! COINCUBE Mobile and read back the same result, so they use one codec. The
//! wallet's signature format selects it, never the remote bytes: on a Bitcoin
//! chain it is the plain PSBT serialisation; on a Bitcoin Blake2b Vault the
//! internal unified records travel as standard `PSBT_IN_PARTIAL_SIG` entries
//! with the `ALL|UNIFIED` sighash byte (`coincube_core::psbt_unified`).

use coincube_core::miniscript::bitcoin::psbt::Psbt;

/// The capability a phone sends when it implements the signing identity
/// contract (chain stated and checked on every session).
pub const CHAIN_IDENTITY_CAPABILITY: &str = "chain-identity-v1";

/// The capability a phone sends only when it has loaded the unified signing
/// library and can produce `ALL|UNIFIED` signatures (contract §2.6, master
/// decisions 16 and 22). It replaces `btcb2-unified-v1`, which builds released
/// before the library shipped send without being able to sign: a phone that
/// stores only the old name reads as an outdated app.
pub const BTCB2_UNIFIED_CAPABILITY: &str = "btcb2-unified-v2";

/// The BTCB2 capability rule, one function for both rails: a phone may sign a
/// Bitcoin Blake2b Vault only when its capability list carries both literals,
/// compared exactly. A statement about the phone, never about a signature:
/// replay status still comes only from verified signatures.
pub fn btcb2_signing_capable(capabilities: &[String]) -> bool {
    [CHAIN_IDENTITY_CAPABILITY, BTCB2_UNIFIED_CAPABILITY]
        .iter()
        .all(|required| capabilities.iter().any(|value| value == required))
}

/// Encode `psbt` for a phone. `unified` is the wallet's signature format
/// (`uses_unified_signatures`: a Bitcoin Blake2b Vault), never a property of
/// remote bytes.
pub fn signing_wire_psbt(unified: bool, psbt: Psbt) -> Result<Vec<u8>, String> {
    if unified {
        let internal =
            coincube_core::psbt_unified::UnifiedPsbt::from_psbt(psbt).map_err(|e| e.to_string())?;
        coincube_core::psbt_unified::export_standard(&internal).map_err(|e| e.to_string())
    } else {
        Ok(psbt.serialize())
    }
}

/// Decode a phone's result in the same format [`signing_wire_psbt`] sent.
pub fn returned_wire_psbt(unified: bool, bytes: &[u8]) -> Result<Psbt, String> {
    if unified {
        // This strict importer accepts standard 0x21 and legacy internal records;
        // it rejects duplicate/conflicting namespaces. Never retry a failed parse.
        coincube_core::psbt_unified::import_standard(bytes)
            .map(|internal| internal.psbt().clone())
            .map_err(|e| e.to_string())
    } else {
        Psbt::deserialize(bytes).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_btcb2_capability_rule_needs_both_exact_literals() {
        let caps = |values: &[&str]| values.iter().map(|v| v.to_string()).collect::<Vec<_>>();
        assert!(btcb2_signing_capable(&caps(&[
            "chain-identity-v1",
            "btcb2-unified-v2"
        ])));
        for refused in [
            caps(&[]),
            caps(&["chain-identity-v1"]),
            caps(&["btcb2-unified-v2"]),
            caps(&["chain-identity-v1", "btcb2-unified-v1"]),
            caps(&[" chain-identity-v1", "BTCB2-UNIFIED-V2"]),
        ] {
            assert!(!btcb2_signing_capable(&refused), "{:?}", refused);
        }
    }
}
