//! Raw blocks of either chain, including Bitcoin Blake2b's extended header.
//!
//! Bitcoin Knots' BLAKE2b hardfork (<https://github.com/bitcoinknots/bitcoin/pull/359>)
//! marks an extended header by setting the top bit of the version field. That
//! header is 164 bytes and hashed with BLAKE2b; `bitcoin::block::Header` can
//! only read the legacy 80-byte form, so a raw Bitcoin Blake2b block from
//! 961,640 on does not deserialize with rust-bitcoin. The header code here is
//! a port of `src/knots.rs` in the indexer Connect runs (`retropex/electrs`,
//! commit `4453cac`, MIT), pinned to Knots' `block_header_v2.json` vectors and
//! to blocks of the live chain.
//!
//! [`confirmed_transaction`] is the only entry point: it authenticates a raw
//! block against the hash a caller already trusts and returns one of its
//! transactions *with the witness the block commits to*. A txid commits to
//! no witness, so this is the way to learn which signatures were confirmed.

use std::{error, fmt};

use miniscript::bitcoin::{
    block::{Header, Version},
    consensus::deserialize_partial,
    hashes::{sha256, sha256d, Hash, HashEngine},
    Block, BlockHash, CompactTarget, Transaction, TxMerkleNode, Txid,
};

use blake2::{digest::consts::U32, Blake2b, Digest};

/// Set in the header's version field to mark the extended (BLAKE2b) header.
const VERSION_HEADER_V2_FLAG: u32 = 0x8000_0000;

const HEADER_V1_SIZE: usize = 80;
const HEADER_V2_SIZE: usize = 164;

/// The largest raw block [`confirmed_transaction`] reads: a 4 MWU block's
/// serialization can't exceed 4 MB, plus the extended header.
pub const MAX_BLOCK_BYTES: usize = 4_000_000 + HEADER_V2_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    TooLarge,
    /// The bytes are not a block: a short header, a bad transaction list or
    /// trailing data.
    Malformed,
    /// The header does not hash to the block the caller asked for.
    WrongHash,
    /// The transactions do not match the header's merkle root.
    MerkleRoot,
    /// The coinbase's witness commitment does not match the witnesses.
    WitnessCommitment,
    /// The block does not contain the transaction.
    NotInBlock,
}

impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "block exceeds {} bytes", MAX_BLOCK_BYTES),
            Self::Malformed => write!(f, "malformed block"),
            Self::WrongHash => write!(f, "block does not hash to the requested block"),
            Self::MerkleRoot => write!(f, "transactions do not match the merkle root"),
            Self::WitnessCommitment => write!(f, "witnesses do not match the commitment"),
            Self::NotInBlock => write!(f, "transaction is not in the block"),
        }
    }
}

impl error::Error for BlockError {}

/// The header fields the hashes need, legacy or extended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockHeader {
    /// The version as on the wire, flag included.
    version: u32,
    prev_blockhash: [u8; 32],
    merkle_root: [u8; 32],
    /// The time as on the wire.
    time: u32,
    bits: u32,
    nonce: u32,
    extended: Option<Extended>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Extended {
    nonce2: u32,
    nonce3: u32,
    extranonce: [u8; 16],
    time_offset: u32,
    txcount: u16,
    flags: u8,
    xor_key_mask_clear_bits: u8,
    xor_key: [u8; 16],
    height: i32,
    mm_rhs: [u8; 32],
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }
}

impl BlockHeader {
    /// Parse the header at the start of `raw`, returning it and its size.
    fn parse(raw: &[u8]) -> Option<(Self, usize)> {
        let mut r = Reader(raw);
        let version = r.u32()?;
        let mut header = BlockHeader {
            version,
            prev_blockhash: r.take()?,
            merkle_root: r.take()?,
            time: r.u32()?,
            bits: r.u32()?,
            nonce: r.u32()?,
            extended: None,
        };
        if version & VERSION_HEADER_V2_FLAG == 0 {
            return Some((header, HEADER_V1_SIZE));
        }
        header.extended = Some(Extended {
            nonce2: r.u32()?,
            nonce3: r.u32()?,
            extranonce: r.take()?,
            time_offset: r.u32()?,
            txcount: r.take().map(u16::from_le_bytes)?,
            flags: r.take::<1>()?[0],
            xor_key_mask_clear_bits: r.take::<1>()?[0],
            xor_key: r.take()?,
            height: r.take().map(i32::from_le_bytes)?,
            mm_rhs: r.take()?,
        });
        Some((header, HEADER_V2_SIZE))
    }

    fn block_hash(&self, raw: &[u8]) -> BlockHash {
        match &self.extended {
            // SHA256d over the 80 bytes.
            None => BlockHash::from_raw_hash(sha256d::Hash::hash(&raw[..HEADER_V1_SIZE])),
            Some(extended) => self.blake2b_block_hash(extended),
        }
    }

    /// Knots' `CBlockHeader::GetHash()` for an extended header.
    fn blake2b_block_hash(&self, e: &Extended) -> BlockHash {
        // A pooling miner only learns `xor_key` once it finds a block, so the
        // header commits to the key's hash rather than to the key itself.
        let xor_key_hash = tagged_hash("Bitcoin block hash PoW XOR key", &[&e.xor_key]);

        let mut xor_key_mask = [0u8; 32];
        if e.xor_key != [0u8; 16] {
            xor_key_mask = tagged_hash("Bitcoin block hash PoW XOR mask", &[&e.xor_key]);
            // `clear_bits` is a u8, so this stays in bounds.
            let clear_bytes = usize::from(e.xor_key_mask_clear_bits / 8);
            xor_key_mask[..clear_bytes].fill(0);
            xor_key_mask[clear_bytes] &= 0xffu8 >> (e.xor_key_mask_clear_bits % 8);
        }

        // Display order: the wire bytes reversed.
        let mut prevblock_ordered = self.prev_blockhash;
        prevblock_ordered.reverse();
        let mut prevblock_hidden =
            tagged_hash("Bitcoin prevblock header, hashed", &[&prevblock_ordered]);

        let h1 = tagged_hash(
            "Bitcoin block header 1",
            &[
                &self.version.to_le_bytes(),
                &prevblock_ordered,
                &e.height.to_le_bytes(),
                &self.merkle_root,
                &self.time.to_le_bytes(),
                &[0], // reserved for extended 40-bit time
                &self.bits.to_le_bytes(),
                &u32::from(e.txcount).to_le_bytes(),
                &[e.flags, e.xor_key_mask_clear_bits],
                &xor_key_hash,
            ],
        );
        let h2 = tagged_hash("Merge-mining hook", &[&h1, &[0u8; 32], &e.mm_rhs]);
        let hash = blake2b_256(&[&[0u8; 4], &h2, &e.extranonce]);

        let nonce = self.nonce.to_le_bytes();
        let nonce2 = e.nonce2.to_le_bytes();
        let time_offset = e.time_offset.to_le_bytes();
        let nonce3 = e.nonce3.to_le_bytes();
        let tail: [&[u8]; 5] = [&nonce, &nonce2, &time_offset, &nonce3, &hash];
        let hash = match e.flags & 3 {
            0 => {
                prevblock_hidden[..6].fill(0);
                blake2b_256(&[
                    &prevblock_hidden,
                    tail[0],
                    tail[1],
                    tail[2],
                    tail[3],
                    tail[4],
                ])
            }
            1 => blake2b_256(&[&nonce, &nonce2, &nonce3, &time_offset, &hash, &h2]),
            2 => blake2b_256(&[&[0u8; 48], &h2, tail[0], tail[1], tail[2], tail[3], tail[4]]),
            _ => blake2b_256(&[&[0u8; 80], &h2, tail[0], tail[1], tail[2], tail[3], tail[4]]),
        };

        let mut inner = [0u8; 32];
        for (i, byte) in inner.iter_mut().rev().enumerate() {
            *byte = hash[i] ^ xor_key_mask[i];
        }
        BlockHash::from_byte_array(inner)
    }
}

/// BIP340-style tagged SHA256, as Bitcoin Core's `TaggedHash`.
fn tagged_hash(tag: &str, parts: &[&[u8]]) -> [u8; 32] {
    let tag_hash = sha256::Hash::hash(tag.as_bytes());
    let mut engine = sha256::Hash::engine();
    engine.input(tag_hash.as_byte_array());
    engine.input(tag_hash.as_byte_array());
    for part in parts {
        engine.input(part);
    }
    sha256::Hash::from_engine(engine).to_byte_array()
}

fn blake2b_256(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Blake2b::<U32>::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// Return `txid` from the raw block `raw`, with its witness, after checking
/// that the block's header hashes to `expected` and that its transactions and
/// their witnesses are the ones the header commits to (merkle root, then the
/// coinbase's BIP 141 witness commitment).
///
/// What `expected` means is the caller's business: this proves only that the
/// returned bytes are what that block holds.
pub fn confirmed_transaction(
    raw: &[u8],
    expected: BlockHash,
    txid: Txid,
) -> Result<Transaction, BlockError> {
    if raw.len() > MAX_BLOCK_BYTES {
        return Err(BlockError::TooLarge);
    }
    let (header, size) = BlockHeader::parse(raw).ok_or(BlockError::Malformed)?;
    if header.block_hash(raw) != expected {
        return Err(BlockError::WrongHash);
    }
    let (txdata, used): (Vec<Transaction>, usize) =
        deserialize_partial(&raw[size..]).map_err(|_| BlockError::Malformed)?;
    if used != raw.len() - size || txdata.is_empty() {
        return Err(BlockError::Malformed);
    }
    // rust-bitcoin's commitment checks read only the merkle root and the
    // transactions, so a legacy header carrying the parsed root reuses them.
    let block = Block {
        header: Header {
            version: Version::ONE,
            prev_blockhash: BlockHash::all_zeros(),
            merkle_root: TxMerkleNode::from_byte_array(header.merkle_root),
            time: 0,
            bits: CompactTarget::from_consensus(0),
            nonce: 0,
        },
        txdata,
    };
    if !block.check_merkle_root() {
        return Err(BlockError::MerkleRoot);
    }
    if !block.check_witness_commitment() {
        return Err(BlockError::WitnessCommitment);
    }
    block
        .txdata
        .into_iter()
        .find(|tx| tx.compute_txid() == txid)
        .ok_or(BlockError::NotInBlock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniscript::bitcoin::consensus::{deserialize, serialize, Encodable};
    use miniscript::bitcoin::hex::{DisplayHex, FromHex};
    use std::str::FromStr;

    /// (serialized header, block hash) vectors from Knots'
    /// `src/test/data/block_header_v2.json`, covering all four ASIC profiles,
    /// as the indexer's `src/knots.rs` carries them.
    const V2_HEADER_VECTORS: &[(&str, &str)] = &[
        // profile_0_time_offset
        (
            "000000a01f1e1d1c1b1a191817161514131211100f0e0d0c0b0a0908070605040302010000112233445566778899aabbccddeeff00102030405060708090a0b0c0d0e0f0a8913577ffff001d0df0ad0b44332211efcdab89ffeeddccbbaa998877665544332211005802000003001c000000000000000000000000000000000040d10c008967452301efcdab8967452301efcdab8967452301efcdab8967452301efcdab",
            "4b495dcf05d70a49785b799b22284fbcd9dd1209237c53c87e4674b15587d704",
        ),
        // profile_1_time_offset_nonzero_key
        (
            "000000a01f1e1d1c1b1a191817161514131211100f0e0d0c0b0a0908070605040302010000112233445566778899aabbccddeeff00102030405060708090a0b0c0d0e0f0a8913577ffff001d0df0ad0b44332211efcdab89ffeeddccbbaa998877665544332211005802000001001d00efcdab8967452301efcdab896745230141d10c008967452301efcdab8967452301efcdab8967452301efcdab8967452301efcdab",
            "44b383821dea9af8d7d81ba7741c34ac8c07ab81ab081d8b6bf0575a787a1eef",
        ),
        // profile_2_time_offset_selector_7
        (
            "000000a01f1e1d1c1b1a191817161514131211100f0e0d0c0b0a0908070605040302010000112233445566778899aabbccddeeff00102030405060708090a0b0c0d0e0f0a8913577ffff001d0df0ad0bddccbbaaefcdab89ffeeddccbbaa998877665544332211005802000003001e071032547698badcfe1032547698badcfe40d10c008967452301efcdab8967452301efcdab8967452301efcdab8967452301efcdab",
            "06fddae4eaca10b85c87a3c7ed71717fd83998a32fe13f4780722b1f5d882e76",
        ),
        // profile_3_time_offset_selector_8
        (
            "000000a01f1e1d1c1b1a191817161514131211100f0e0d0c0b0a0908070605040302010000112233445566778899aabbccddeeff00102030405060708090a0b0c0d0e0f0a8913577ffff001d0df0ad0b44332211040302010000000000000000ffffffffffffffff5802000003001f081032547698badcfe1032547698badcfe40d10c008967452301efcdab8967452301efcdab8967452301efcdab8967452301efcdab",
            "e6304527536f619d3ad71b1c21a22fdef9068498acc561b4100b034373a87058",
        ),
        // profile_0_time_offset_disabled_selector_255
        (
            "000000a01f1e1d1c1b1a191817161514131211100f0e0d0c0b0a0908070605040302010000112233445566778899aabbccddeeff00102030405060708090a0b0c0d0e0f000943577ffff001dffffffff44332211efcdab89ffeeddccbbaa9988776655443322110088776655030018ff2222222222222222111111111111111140d10c000000000000000000000000000000000000000000000000000000000000000000",
            "c31b24420d67f86e524f980a24a18e88f36c821046d5288251b5d88998c69f86",
        ),
    ];

    /// Bitcoin Blake2b mainnet's first post-fork block, 961,640, whose hash
    /// Connect also pins as its fallback checkpoint.
    const FORK_HEADER: &str = "000000a0657e02138733654183a2c7320d85ca9d743fe139c4bb01000000000000000000c137a8515a0f6b3aaf6049cc7611787c022ad523d51094be0a0363d0dc0bc7684dca936a4f8d001a5671798c84daeb494dca936a00000000b1ccf00d0300000000000000000000001e0300000000000000000000000000000000000068ac0e000000000000000000000000000000000000000000000000000000000000000000";
    const FORK_HASH: &str = "0000000000000050c1e5f69672f459293be14f46e5a494e7a8c8541396f18eeb";

    /// Bitcoin Blake2b mainnet block 979,904 as Connect serves it (49
    /// transactions, segwit ones among them).
    const LIVE_BLOCK: &[u8] = include_bytes!("../tests/data/btcb2_block_979904.raw");
    const LIVE_HASH: &str = "000000000000000065508ea018e19120795fb5af170a89853ad3b477343fd252";

    fn hash(s: &str) -> BlockHash {
        BlockHash::from_str(s).unwrap()
    }

    #[test]
    fn blake2b_256_test_vectors() {
        assert_eq!(
            blake2b_256(&[b""]).to_lower_hex_string(),
            "0e5751c026e543b2e8ab2eb06099daa1d1e5df47778f7787faab45cdf12fe3a8"
        );
        assert_eq!(
            blake2b_256(&[b"a", b"bc"]).to_lower_hex_string(),
            "bddd813c634239723171ef3fee98579b94964e3bb1cb3e427262c8c068d52319"
        );
    }

    #[test]
    fn extended_headers_hash_like_knots() {
        for (serialized, expected) in V2_HEADER_VECTORS.iter().chain([&(FORK_HEADER, FORK_HASH)]) {
            let bytes = Vec::from_hex(serialized).unwrap();
            let (header, size) = BlockHeader::parse(&bytes).unwrap();
            assert_eq!(size, bytes.len());
            assert!(header.extended.is_some());
            assert_eq!(header.block_hash(&bytes), hash(expected), "{}", expected);
        }
    }

    #[test]
    fn legacy_headers_hash_like_rust_bitcoin() {
        let genesis = miniscript::bitcoin::blockdata::constants::genesis_block(
            miniscript::bitcoin::Network::Bitcoin,
        );
        let mut bytes = Vec::new();
        genesis.header.consensus_encode(&mut bytes).unwrap();
        let (header, size) = BlockHeader::parse(&bytes).unwrap();
        assert_eq!((size, header.extended), (HEADER_V1_SIZE, None));
        assert_eq!(header.block_hash(&bytes), genesis.block_hash());

        let raw = serialize(&genesis);
        let coinbase = genesis.txdata[0].compute_txid();
        let tx = confirmed_transaction(&raw, genesis.block_hash(), coinbase).unwrap();
        assert_eq!(tx, genesis.txdata[0]);
    }

    #[test]
    fn a_live_block_yields_its_witnessed_transactions() {
        let (header, size) = BlockHeader::parse(LIVE_BLOCK).unwrap();
        assert!(header.extended.is_some());
        let txdata: Vec<Transaction> = deserialize(&LIVE_BLOCK[size..]).unwrap();
        assert_eq!(txdata.len(), 49);
        let witnessed = txdata
            .iter()
            .skip(1)
            .find(|tx| tx.input.iter().all(|input| !input.witness.is_empty()))
            .expect("the fixture has a segwit transaction");
        let found =
            confirmed_transaction(LIVE_BLOCK, hash(LIVE_HASH), witnessed.compute_txid()).unwrap();
        assert_eq!(serialize(&found), serialize(witnessed));
        assert_eq!(found.compute_wtxid(), witnessed.compute_wtxid());
    }

    #[test]
    fn a_block_that_lies_is_refused() {
        let txdata: Vec<Transaction> = deserialize(&LIVE_BLOCK[HEADER_V2_SIZE..]).unwrap();
        let target = txdata
            .iter()
            .skip(1)
            .find(|tx| tx.input.iter().all(|input| !input.witness.is_empty()))
            .unwrap()
            .clone();
        let txid = target.compute_txid();
        let at = |raw: &[u8]| confirmed_transaction(raw, hash(LIVE_HASH), txid);

        // Another block's hash.
        assert_eq!(
            confirmed_transaction(LIVE_BLOCK, hash(FORK_HASH), txid),
            Err(BlockError::WrongHash)
        );
        // A header field flipped.
        let mut raw = LIVE_BLOCK.to_vec();
        raw[100] ^= 1;
        assert_eq!(at(&raw), Err(BlockError::WrongHash));

        // The target's witness swapped: same txid, so the merkle root holds,
        // but the witness commitment does not.
        let mut forged = target.clone();
        let mut element = forged.input[0].witness.to_vec();
        let last = element[0].len() - 1;
        element[0][last] ^= 1;
        forged.input[0].witness = element.into();
        assert_eq!(forged.compute_txid(), txid);
        let mut txs = txdata.clone();
        let position = txs.iter().position(|tx| tx.compute_txid() == txid).unwrap();
        txs[position] = forged;
        let mut raw = LIVE_BLOCK[..HEADER_V2_SIZE].to_vec();
        raw.extend(serialize(&txs));
        assert_eq!(at(&raw), Err(BlockError::WitnessCommitment));

        // A transaction dropped: the merkle root no longer matches.
        let mut txs = txdata.clone();
        txs.remove(position);
        let mut raw = LIVE_BLOCK[..HEADER_V2_SIZE].to_vec();
        raw.extend(serialize(&txs));
        assert_eq!(at(&raw), Err(BlockError::MerkleRoot));

        // Trailing bytes, a truncated block, an absent transaction.
        let mut raw = LIVE_BLOCK.to_vec();
        raw.push(0);
        assert_eq!(at(&raw), Err(BlockError::Malformed));
        assert_eq!(
            at(&LIVE_BLOCK[..LIVE_BLOCK.len() - 1]),
            Err(BlockError::Malformed)
        );
        assert_eq!(at(&LIVE_BLOCK[..100]), Err(BlockError::Malformed));
        assert_eq!(
            confirmed_transaction(LIVE_BLOCK, hash(LIVE_HASH), Txid::all_zeros()),
            Err(BlockError::NotInBlock)
        );
        assert_eq!(
            at(&vec![0u8; MAX_BLOCK_BYTES + 1]),
            Err(BlockError::TooLarge)
        );
    }
}
