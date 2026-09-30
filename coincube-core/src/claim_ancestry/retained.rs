//! Bounded raw-path storage. Decoding repeats structural verification and never
//! restores chain observations, freshness, or permission to spend.
use super::{search::OwnedLink, *};
use miniscript::bitcoin::consensus::serialize;
use std::convert::TryInto;

const MAGIC: &[u8; 8] = b"CCAP\0\0\0\x01";
const HEADER: usize = 8 + 36 + 4;
const LINK_HEADER: usize = 8;
pub const MAX_ENCODED_BYTES: usize = HEADER + MAX_PATH_BYTES + MAX_PATH_TRANSACTIONS * LINK_HEADER;

#[derive(Debug)]
pub enum DecodeError {
    Encoding,
    WrongSelection,
    Structural(Error),
}

/// Contains raw evidence only. Even a successfully restored path requires new
/// canonical root qualification and all wallet/spend checks before use.
#[derive(Debug)]
pub struct RetainedPath {
    selected: OutPoint,
    links: Vec<OwnedLink>,
}
impl RetainedPath {
    pub fn new(selected: OutPoint, links: Vec<OwnedLink>) -> Result<Self, Error> {
        verify_owned(selected, &links)?;
        Ok(Self { selected, links })
    }
    pub fn selected(&self) -> OutPoint {
        self.selected
    }
    pub fn links(&self) -> &[OwnedLink] {
        &self.links
    }
    pub fn reverify(&self) -> Result<CoinbaseDependency, Error> {
        verify_owned(self.selected, &self.links)
    }
    pub fn encode(&self) -> Vec<u8> {
        // Construction and private fields guarantee all conversions and limits.
        let mut encoded = Vec::with_capacity(
            HEADER
                + self
                    .links
                    .iter()
                    .map(|link| LINK_HEADER + link.transaction.len())
                    .sum::<usize>(),
        );
        encoded.extend_from_slice(MAGIC);
        encoded.extend_from_slice(&serialize(&self.selected));
        encoded.extend_from_slice(&(self.links.len() as u32).to_le_bytes());
        for link in &self.links {
            encoded.extend_from_slice(&(link.transaction.len() as u32).to_le_bytes());
            encoded.extend_from_slice(
                &link
                    .parent_input
                    .map_or(u32::MAX, |index| index as u32)
                    .to_le_bytes(),
            );
            encoded.extend_from_slice(&link.transaction);
        }
        encoded
    }
    /// The expected selection must come from the independently bound intent.
    /// Length/count limits are checked before allocation; borrowed raw links are
    /// structurally verified before they become owned evidence.
    pub fn decode(expected: OutPoint, encoded: &[u8]) -> Result<Self, DecodeError> {
        if encoded.len() > MAX_ENCODED_BYTES {
            return Err(DecodeError::Encoding);
        }
        let mut remaining = encoded;
        if take(&mut remaining, MAGIC.len())? != MAGIC {
            return Err(DecodeError::Encoding);
        }
        let selected: OutPoint =
            deserialize(take(&mut remaining, 36)?).map_err(|_| DecodeError::Encoding)?;
        if selected != expected {
            return Err(DecodeError::WrongSelection);
        }
        let count = number(&mut remaining)? as usize;
        if count == 0 || count > MAX_PATH_TRANSACTIONS {
            return Err(DecodeError::Encoding);
        }
        let mut links = Vec::with_capacity(count);
        let mut total = 0usize;
        for _ in 0..count {
            let length = number(&mut remaining)? as usize;
            total = total.checked_add(length).ok_or(DecodeError::Encoding)?;
            if length > MAX_TRANSACTION_BYTES || total > MAX_PATH_BYTES {
                return Err(DecodeError::Encoding);
            }
            let parent = number(&mut remaining)?;
            links.push(Link {
                transaction: take(&mut remaining, length)?,
                parent_input: (parent != u32::MAX).then_some(parent as usize),
            });
        }
        if !remaining.is_empty() {
            return Err(DecodeError::Encoding);
        }
        verify(selected, &links).map_err(DecodeError::Structural)?;
        Ok(Self {
            selected,
            links: links
                .into_iter()
                .map(|link| OwnedLink {
                    transaction: link.transaction.to_vec(),
                    parent_input: link.parent_input,
                })
                .collect(),
        })
    }
}
fn take<'a>(remaining: &mut &'a [u8], count: usize) -> Result<&'a [u8], DecodeError> {
    let result = remaining.get(..count).ok_or(DecodeError::Encoding)?;
    *remaining = &remaining[count..];
    Ok(result)
}
fn number(remaining: &mut &[u8]) -> Result<u32, DecodeError> {
    let bytes: [u8; 4] = take(remaining, 4)?
        .try_into()
        .map_err(|_| DecodeError::Encoding)?;
    Ok(u32::from_le_bytes(bytes))
}
fn verify_owned(selected: OutPoint, links: &[OwnedLink]) -> Result<CoinbaseDependency, Error> {
    // Bound even the small temporary vector before constructing it.
    if links.is_empty() || links.len() > MAX_PATH_TRANSACTIONS {
        return Err(Error::PathLimit);
    }
    verify(
        selected,
        &links
            .iter()
            .map(|link| Link {
                transaction: &link.transaction,
                parent_input: link.parent_input,
            })
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim_ancestry::tests::{out, tx};

    fn fixture() -> RetainedPath {
        let root = tx(OutPoint::null());
        let selected = tx(out(&root));
        RetainedPath::new(
            out(&selected),
            vec![
                OwnedLink {
                    transaction: serialize(&selected),
                    parent_input: Some(0),
                },
                OwnedLink {
                    transaction: serialize(&root),
                    parent_input: None,
                },
            ],
        )
        .unwrap()
    }
    #[test]
    fn restart_reverifies_exact_links_and_requires_intent_selection() {
        let path = fixture();
        let expected = path.reverify().unwrap();
        let raw = path.encode();
        let restored = RetainedPath::decode(path.selected(), &raw).unwrap();
        assert_eq!(restored.encode(), raw);
        assert_eq!(restored.reverify().unwrap().root(), expected.root());
        assert_eq!(restored.reverify().unwrap().txids(), expected.txids());
        assert!(matches!(
            RetainedPath::decode(expected.root(), &raw),
            Err(DecodeError::WrongSelection)
        ));
        for end in 0..raw.len() {
            assert!(RetainedPath::decode(path.selected(), &raw[..end]).is_err());
        }
        let mut trailing = raw.clone();
        trailing.push(0);
        assert!(matches!(
            RetainedPath::decode(path.selected(), &trailing),
            Err(DecodeError::Encoding)
        ));
        let mut substituted = raw;
        // Change the selected transaction version without changing its intent txid.
        substituted[HEADER + LINK_HEADER] ^= 1;
        assert!(matches!(
            RetainedPath::decode(path.selected(), &substituted),
            Err(DecodeError::Structural(Error::WrongTransaction))
        ));
    }
    #[test]
    fn exact_count_and_raw_byte_limits_round_trip() {
        use miniscript::bitcoin::ScriptBuf;
        for (count, size) in [
            (MAX_PATH_TRANSACTIONS, None),
            (10, Some(MAX_TRANSACTION_BYTES)),
        ] {
            let mut links = Vec::new();
            let mut parent = OutPoint::null();
            for index in 0..count {
                let mut transaction = tx(parent);
                if let Some(size) = size {
                    transaction.output[0].script_pubkey =
                        ScriptBuf::from_bytes(vec![0; size - 100]);
                    let used = serialize(&transaction).len();
                    transaction.output[0].script_pubkey =
                        ScriptBuf::from_bytes(vec![0; size - 100 + size - used]);
                    assert_eq!(serialize(&transaction).len(), size);
                }
                parent = out(&transaction);
                links.push(OwnedLink {
                    transaction: serialize(&transaction),
                    parent_input: (index != 0).then_some(0),
                });
            }
            links.reverse();
            let path = RetainedPath::new(parent, links).unwrap();
            let encoded = path.encode();
            assert_eq!(
                RetainedPath::decode(parent, &encoded).unwrap().encode(),
                encoded
            );
            if size.is_some() {
                assert_eq!(
                    path.links()
                        .iter()
                        .map(|link| link.transaction.len())
                        .sum::<usize>(),
                    MAX_PATH_BYTES
                );
            }
        }
    }
    #[test]
    fn malformed_envelopes_and_edges_cannot_restore_a_path() {
        let path = fixture();
        let raw = path.encode();
        for (offset, value) in [(7, 2), (8 + 36, 0), (8 + 36, 65)] {
            let mut changed = raw.clone();
            changed[offset] = value;
            assert!(matches!(
                RetainedPath::decode(path.selected(), &changed),
                Err(DecodeError::Encoding)
            ));
        }
        let mut huge = raw.clone();
        huge[HEADER..HEADER + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            RetainedPath::decode(path.selected(), &huge),
            Err(DecodeError::Encoding)
        ));
        let mut terminal = raw.clone();
        let terminal_parent = HEADER + LINK_HEADER + path.links()[0].transaction.len() + 4;
        terminal[terminal_parent..terminal_parent + 4].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            RetainedPath::decode(path.selected(), &terminal),
            Err(DecodeError::Structural(Error::InvalidTerminal))
        ));
        // Every announced item is individually bounded, and the envelope fits
        // its cap. The eleventh payload exceeds the shared raw-byte allowance.
        let mut cumulative = raw[..HEADER].to_vec();
        cumulative[44..48].copy_from_slice(&11u32.to_le_bytes());
        for _ in 0..10 {
            cumulative.extend_from_slice(&(MAX_TRANSACTION_BYTES as u32).to_le_bytes());
            cumulative.extend_from_slice(&0u32.to_le_bytes());
            cumulative.resize(cumulative.len() + MAX_TRANSACTION_BYTES, 0);
        }
        cumulative.extend_from_slice(&1u32.to_le_bytes());
        cumulative.extend_from_slice(&u32::MAX.to_le_bytes());
        cumulative.push(0);
        assert!(cumulative.len() < MAX_ENCODED_BYTES);
        assert!(matches!(
            RetainedPath::decode(path.selected(), &cumulative),
            Err(DecodeError::Encoding)
        ));
        let mut edge = raw;
        edge[HEADER + 4..HEADER + 8].copy_from_slice(&1u32.to_le_bytes());
        assert!(matches!(
            RetainedPath::decode(path.selected(), &edge),
            Err(DecodeError::Structural(Error::InvalidInput))
        ));
        assert!(matches!(
            RetainedPath::decode(path.selected(), &vec![0; MAX_ENCODED_BYTES + 1]),
            Err(DecodeError::Encoding)
        ));
    }
}
