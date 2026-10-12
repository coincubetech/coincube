//! The OP_RETURN poison output shared by Claim step 1 and Split step 1.
//!
//! The script is a deterministic 90-byte OP_RETURN: larger than RDTS's 83-byte
//! output-script limit, so a Bitcoin transaction carrying it is invalid on a
//! fork enforcing RDTS while that deployment is active. Its validity is
//! time-dependent; the caller must check the deployment and expiry margin
//! separately. Building or parsing this script is labeling only: it proves no
//! chain state, confers no signing or broadcast authority and is not poison
//! evidence until the transaction carrying it is confirmed.

use std::{collections::BTreeSet, convert::TryFrom};

use miniscript::bitcoin::{
    self,
    hashes::{sha256, Hash},
    BlockHash, OutPoint, ScriptBuf,
};

use crate::chain::ChainId;

const TAG: &[u8; 14] = b"REPLAY-PROTECT";
const PAYLOAD_VERSION: u8 = 1;
const PAYLOAD_LEN: usize = 87;
/// Total scriptPubKey length: OP_RETURN, OP_PUSHDATA1, length byte, payload.
pub const SCRIPT_LEN: usize = 90;

/// The payload's chain byte. Only the Bitcoin chains a step 1 can run on have
/// one; every other chain (including both BTCB2 chains) has none.
fn chain_byte(chain: ChainId) -> Option<u8> {
    match chain {
        ChainId::Bitcoin => Some(0),
        ChainId::Testnet4 => Some(1),
        _ => None,
    }
}

/// Build the poison script for a Bitcoin step 1 spending exactly `outpoints`.
///
/// `outpoints` is a set, so the commitment is independent of input order.
/// `fork_marker` identifies the intended observed fork anchor; it is caller
/// labeling, NOT authenticated chain evidence. Returns `None` for a chain with
/// no payload chain byte (anything but Bitcoin mainnet or testnet4).
pub fn split_poison_script(
    chain: ChainId,
    fork_marker: BlockHash,
    outpoints: &BTreeSet<OutPoint>,
) -> Option<ScriptBuf> {
    let chain_byte = chain_byte(chain)?;
    // Sorted outpoints make the labeling independent of input presentation order.
    let bytes: Vec<_> = outpoints
        .iter()
        .flat_map(bitcoin::consensus::serialize)
        .collect();
    let commitment = sha256::Hash::hash(&bytes);
    let mut payload = [0u8; PAYLOAD_LEN];
    payload[..14].copy_from_slice(TAG);
    payload[14] = PAYLOAD_VERSION;
    payload[15] = chain_byte;
    payload[16..48].copy_from_slice(fork_marker.as_byte_array());
    payload[48..80].copy_from_slice(commitment.as_byte_array());
    let poison = ScriptBuf::new_op_return(
        bitcoin::script::PushBytesBuf::try_from(payload.to_vec())
            .expect("fixed 87-byte payload fits script push limits"),
    );
    debug_assert_eq!(poison.len(), SCRIPT_LEN);
    Some(poison)
}

/// Decoded labels from the canonical neutral marker. These are not proof of
/// chain state or input ownership; callers still authenticate/rebuild the spend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitPoisonDetails {
    pub version: u8,
    pub chain: ChainId,
    pub fork_marker: BlockHash,
    pub input_commitment: sha256::Hash,
}

/// Decode only the canonical format: OP_RETURN, PUSHDATA1, 87-byte payload,
/// neutral tag, supported version/network, and zero padding. The input
/// commitment is verified against the actual inputs by rebuilding the spend.
pub fn split_poison_details(script: &bitcoin::Script) -> Option<SplitPoisonDetails> {
    let bytes = script.as_bytes();
    if bytes.len() != SCRIPT_LEN || bytes[..3] != [0x6a, 0x4c, PAYLOAD_LEN as u8] {
        return None;
    }
    let payload = &bytes[3..];
    if &payload[..14] != TAG || payload[14] != PAYLOAD_VERSION || payload[80..] != [0; 7] {
        return None;
    }
    let chain = match payload[15] {
        0 => ChainId::Bitcoin,
        1 => ChainId::Testnet4,
        _ => return None,
    };
    Some(SplitPoisonDetails {
        version: payload[14],
        chain,
        fork_marker: BlockHash::from_slice(&payload[16..48]).ok()?,
        input_commitment: sha256::Hash::from_slice(&payload[48..80]).ok()?,
    })
}

/// Read the fork label only from a canonical neutral marker.
pub fn split_poison_fork_marker(script: &bitcoin::Script) -> Option<BlockHash> {
    split_poison_details(script).map(|details| details.fork_marker)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outpoints() -> BTreeSet<OutPoint> {
        vec![
            OutPoint::new(bitcoin::Txid::from_byte_array([2; 32]), 1),
            OutPoint::new(bitcoin::Txid::from_byte_array([1; 32]), 0),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn only_bitcoin_step1_chains_have_a_poison() {
        let marker = BlockHash::from_byte_array([7; 32]);
        for chain in ChainId::ALL {
            let script = split_poison_script(chain, marker, &outpoints());
            assert_eq!(
                script.is_some(),
                matches!(chain, ChainId::Bitcoin | ChainId::Testnet4),
                "{chain:?}"
            );
            if let Some(script) = script {
                assert!(script.is_op_return());
                assert_eq!(script.len(), SCRIPT_LEN);
                // RDTS limits output scripts to 83 bytes.
                assert!(script.len() > 83);
                assert_eq!(split_poison_fork_marker(&script), Some(marker));
            }
        }
    }

    #[test]
    fn details_match_the_actual_payload_and_inputs() {
        let marker = BlockHash::from_byte_array([7; 32]);
        for chain in [ChainId::Bitcoin, ChainId::Testnet4] {
            let script = split_poison_script(chain, marker, &outpoints()).unwrap();
            let details = split_poison_details(&script).unwrap();
            assert_eq!(details.version, 1);
            assert_eq!(details.chain, chain);
            assert_eq!(details.fork_marker, marker);
            let serialized: Vec<_> = outpoints()
                .iter()
                .flat_map(bitcoin::consensus::serialize)
                .collect();
            assert_eq!(details.input_commitment, sha256::Hash::hash(&serialized));
            assert_eq!(&script.as_bytes()[3..17], b"REPLAY-PROTECT");
            let changed = split_poison_script(chain, marker, &BTreeSet::new()).unwrap();
            assert_ne!(
                split_poison_details(&changed).unwrap().input_commitment,
                details.input_commitment
            );
        }
    }

    #[test]
    fn parser_rejects_legacy_and_noncanonical_markers() {
        let good = split_poison_script(
            ChainId::Bitcoin,
            BlockHash::from_byte_array([7; 32]),
            &outpoints(),
        )
        .unwrap();
        for offset in [0, 1, 2, 3, 17, 18, 83, 89] {
            let mut bytes = good.to_bytes();
            bytes[offset] = 0xff;
            let bad = ScriptBuf::from_bytes(bytes);
            assert!(split_poison_details(&bad).is_none(), "offset {}", offset);
            assert!(split_poison_fork_marker(&bad).is_none());
        }
        let mut legacy = good.to_bytes();
        legacy[3..17].copy_from_slice(b"COINCUBE-SPLIT");
        assert!(split_poison_details(&ScriptBuf::from_bytes(legacy)).is_none());
        let bytes = good.as_bytes();
        let mut nonminimal = vec![0x6a, 0x4d, 87, 0];
        nonminimal.extend_from_slice(&bytes[3..]);
        assert!(split_poison_details(&ScriptBuf::from_bytes(nonminimal)).is_none());
    }

    #[test]
    fn marker_parser_refuses_other_shapes() {
        let marker = BlockHash::from_byte_array([7; 32]);
        let good = split_poison_script(ChainId::Bitcoin, marker, &outpoints()).unwrap();
        let short = ScriptBuf::new_op_return(
            bitcoin::script::PushBytesBuf::try_from(vec![0u8; 80]).unwrap(),
        );
        let mut trailing = good.to_bytes();
        trailing.push(0x51);
        for script in [
            ScriptBuf::new(),
            short,
            ScriptBuf::from_bytes(trailing),
            ScriptBuf::from_bytes(good.as_bytes()[1..].to_vec()),
        ] {
            assert_eq!(split_poison_fork_marker(&script), None);
        }
    }
}
