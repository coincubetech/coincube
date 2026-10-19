//! Size-bounded binary and base64 PSBT file transport.
//! Verification of returned signatures belongs to the transaction's owner.

use std::{io::Read, path::Path};

use base64::Engine;
use coincube_core::miniscript::bitcoin::psbt::Psbt;

pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAGIC: &[u8] = b"psbt\xff";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Binary,
    Base64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    Io(String),
    TooLarge,
    NotPsbt,
}

pub fn encode(psbt: &Psbt, encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::Binary => psbt.serialize(),
        Encoding::Base64 => base64::engine::general_purpose::STANDARD
            .encode(psbt.serialize())
            .into_bytes(),
    }
}

/// Detect binary or whitespace-tolerant base64, without accepting other data.
pub fn decode(bytes: &[u8]) -> Result<(Psbt, Encoding), TransportError> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(TransportError::TooLarge);
    }
    if bytes.starts_with(MAGIC) {
        return Psbt::deserialize(bytes)
            .map(|psbt| (psbt, Encoding::Binary))
            .map_err(|_| TransportError::NotPsbt);
    }
    let text: String = std::str::from_utf8(bytes)
        .map_err(|_| TransportError::NotPsbt)?
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let raw = base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| TransportError::NotPsbt)?;
    if !raw.starts_with(MAGIC) {
        return Err(TransportError::NotPsbt);
    }
    Psbt::deserialize(&raw)
        .map(|psbt| (psbt, Encoding::Base64))
        .map_err(|_| TransportError::NotPsbt)
}

/// Read no more than the cap plus one byte before rejecting an oversized file.
pub fn load(path: &Path) -> Result<Psbt, TransportError> {
    let file = std::fs::File::open(path).map_err(|error| TransportError::Io(error.to_string()))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| TransportError::Io(error.to_string()))?;
    decode(&bytes).map(|(psbt, _)| psbt)
}
