//! PSBT files for Split step 1 (#568 owner decision D6: files and text only).
//!
//! - Save the unsigned construction as BIP 174 binary or base64 text.
//! - Load a returned file with a size cap, detecting binary or base64 text.
//! - Verify each returned PSBT against the exact opaque construction before
//!   it is used, and combine the signatures of N returned files (one per
//!   cosigner) into one PSBT that is again verified.
//!
//! Verification is `coincube_core::foreign_split::finalize_split_step1`'s own
//! check, run per file: everything except `partial_sigs` and an absent or
//! explicit `SIGHASH_ALL` request must equal the construction, and every
//! supplied signature must verify for its input. A file that satisfies every
//! input is also finalizable; one that holds only some signatures (one
//! cosigner of a multisig) is accepted as a partial, but a returned file must
//! carry at least one signature. A file the wallet already finalized is
//! refused with its own message (Split finalizes and checks the witness
//! itself). Anything else, including another transaction, changed metadata,
//! a non-`ALL` sighash or a bad signature, refuses the file by position. Nothing here finalizes for
//! broadcast, persists state or reads a chain; a PSBT holds only public data.

use std::{fmt, io::Read, path::Path};

use base64::Engine;
use coincube_core::{
    foreign_split::{finalize_split_step1, FinalizeError, SplitStep1},
    miniscript::bitcoin::{psbt::Psbt, secp256k1},
};

/// Largest PSBT file accepted, in bytes (binary or text).
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Most returned files one combine accepts.
pub const MAX_COMBINED_FILES: usize = 16;
const MAGIC: &[u8] = b"psbt\xff";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Binary,
    Base64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    Io(String),
    TooLarge,
    /// Neither a binary PSBT nor base64 text of one.
    NotPsbt,
    /// The file is not the construction plus valid `SIGHASH_ALL` signatures.
    Refused(FinalizeError),
    /// A returned file with no signature (for example the exported one).
    Unsigned,
    /// The wallet finalized the PSBT instead of returning its signatures.
    Finalized,
    NoFiles,
    TooManyFiles,
    /// Returned file at this position (0-based) refused.
    File(usize, Box<FileError>),
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "The PSBT file could not be read or written: {error}"),
            Self::TooLarge => write!(
                f,
                "The file is larger than {} MB, too large to be a Split PSBT.",
                MAX_FILE_BYTES / (1024 * 1024)
            ),
            Self::NotPsbt => f.write_str("The file is not a PSBT (binary or base64 text)."),
            Self::Refused(FinalizeError::ConstructionChanged) => f.write_str(
                "The PSBT is not the Split transaction that was exported. Sign the exact exported file.",
            ),
            Self::Refused(FinalizeError::UnsupportedSighash) => f.write_str(
                "The PSBT was signed with a signature type other than SIGHASH_ALL, which Split refuses.",
            ),
            Self::Refused(FinalizeError::InvalidSignature { input }) => {
                write!(f, "Input {input} has a signature that does not verify.")
            }
            Self::Refused(other) => write!(f, "The PSBT was refused: {other:?}."),
            Self::Unsigned => f.write_str(
                "This PSBT has no signatures. Choose the file your wallet saved after signing.",
            ),
            Self::Finalized => f.write_str(
                "Your wallet finalized this PSBT. Sign it again without finalizing (some wallets call this \"sign only\" or turn off \"finalize\"), then load that file.",
            ),
            Self::NoFiles => f.write_str("Choose at least one signed PSBT file."),
            Self::TooManyFiles => write!(f, "Combine at most {MAX_COMBINED_FILES} files."),
            Self::File(index, error) => write!(f, "File {}: {error}", index + 1),
        }
    }
}

impl std::error::Error for FileError {}

pub fn encode(psbt: &Psbt, encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::Binary => psbt.serialize(),
        Encoding::Base64 => base64::engine::general_purpose::STANDARD
            .encode(psbt.serialize())
            .into_bytes(),
    }
}

/// Binary when it starts with the BIP 174 magic, otherwise base64 text (all
/// ASCII whitespace ignored, so line-wrapped text loads). Returns the
/// detected encoding.
pub fn decode(bytes: &[u8]) -> Result<(Psbt, Encoding), FileError> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(FileError::TooLarge);
    }
    if bytes.starts_with(MAGIC) {
        return Psbt::deserialize(bytes)
            .map(|psbt| (psbt, Encoding::Binary))
            .map_err(|_| FileError::NotPsbt);
    }
    let text: String = std::str::from_utf8(bytes)
        .map_err(|_| FileError::NotPsbt)?
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let raw = base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| FileError::NotPsbt)?;
    if !raw.starts_with(MAGIC) {
        return Err(FileError::NotPsbt);
    }
    Psbt::deserialize(&raw)
        .map(|psbt| (psbt, Encoding::Base64))
        .map_err(|_| FileError::NotPsbt)
}

/// Write the unsigned construction for an external signer.
pub fn save(path: &Path, construction: &SplitStep1, encoding: Encoding) -> Result<(), FileError> {
    std::fs::write(path, encode(construction.psbt(), encoding))
        .map_err(|error| FileError::Io(error.to_string()))
}

/// Read one returned file, refusing anything over [`MAX_FILE_BYTES`] without
/// reading past the cap.
pub fn load(path: &Path) -> Result<Psbt, FileError> {
    let file = std::fs::File::open(path).map_err(|error| FileError::Io(error.to_string()))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| FileError::Io(error.to_string()))?;
    decode(&bytes).map(|(psbt, _)| psbt)
}

/// Whether `psbt` is the exact construction plus at least one valid
/// `SIGHASH_ALL` signature. A partial (not every input satisfied) is
/// accepted; an unsigned or finalized file is not.
pub fn verify(construction: &SplitStep1, psbt: &Psbt) -> Result<(), FileError> {
    if psbt
        .inputs
        .iter()
        .any(|input| input.final_script_sig.is_some() || input.final_script_witness.is_some())
    {
        return Err(FileError::Finalized);
    }
    if psbt
        .inputs
        .iter()
        .all(|input| input.partial_sigs.is_empty())
    {
        return Err(FileError::Unsigned);
    }
    let secp = secp256k1::Secp256k1::verification_only();
    match finalize_split_step1(construction, psbt, &secp) {
        // `Unsatisfied` is returned only after the construction, sighash,
        // signature and economics checks all passed (core's current order,
        // pinned by `split_psbt_partial_with_a_bad_signature_is_refused`).
        Ok(_) | Err(FinalizeError::Unsatisfied) => Ok(()),
        Err(error) => Err(FileError::Refused(error)),
    }
}

/// Merge the signatures of each returned PSBT into the construction. Every
/// file is verified first; the first refused file refuses the combine. The
/// result is verified again and carries the construction's own metadata.
pub fn combine(construction: &SplitStep1, files: &[Psbt]) -> Result<Psbt, FileError> {
    if files.is_empty() {
        return Err(FileError::NoFiles);
    }
    if files.len() > MAX_COMBINED_FILES {
        return Err(FileError::TooManyFiles);
    }
    for (index, file) in files.iter().enumerate() {
        verify(construction, file).map_err(|error| FileError::File(index, Box::new(error)))?;
    }
    let mut combined = construction.psbt().clone();
    for file in files {
        for (input, signed) in combined.inputs.iter_mut().zip(&file.inputs) {
            for (key, signature) in &signed.partial_sigs {
                input.partial_sigs.entry(*key).or_insert(*signature);
            }
        }
    }
    verify(construction, &combined)?;
    Ok(combined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{
        foreign_split_inventory::FreshIndex,
        split_source::split_source,
        split_test_wallets::{self as fixture, Shape},
    };
    use coincube_core::{
        chain::ChainId,
        foreign_split::{create_split_step1, SplitInputs},
        miniscript::bitcoin::{
            absolute::LockTime, bip32::Xpriv, hashes::Hash, psbt::PsbtSighashType,
            sighash::EcdsaSighashType, Amount, BlockHash,
        },
    };

    fn construction(shape: Shape) -> (SplitStep1, Vec<Xpriv>) {
        let wallet = fixture::wallet(shape);
        let inventory = fixture::inventory(&wallet);
        let coins = inventory.splittable_coins();
        let source = split_source(&wallet.external, Some(&wallet.internal)).unwrap();
        let FreshIndex::Proven(destination) = inventory.fresh_receive() else {
            panic!("fixture has a fresh index");
        };
        let tip = inventory.bitcoin_tip_height();
        let step1 = create_split_step1(
            &SplitInputs {
                chain: ChainId::Bitcoin,
                source: &source,
                coins: &coins,
                fork_height: inventory.fork_height(),
                destination,
            },
            2,
            LockTime::from_height(tip).unwrap(),
            tip,
            BlockHash::from_byte_array([7; 32]),
        )
        .unwrap();
        (step1, wallet.signers)
    }

    fn sign(psbt: &Psbt, signer: &Xpriv) -> Psbt {
        let mut signed = psbt.clone();
        signed.sign(signer, &secp256k1::Secp256k1::new()).unwrap();
        signed
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("split-psbt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn split_psbt_file_round_trips_in_both_encodings() {
        let (step1, signers) = construction(Shape::Wpkh);
        for encoding in [Encoding::Binary, Encoding::Base64] {
            let path = scratch("step1.psbt");
            save(&path, &step1, encoding).unwrap();
            assert_eq!(load(&path).unwrap(), *step1.psbt());
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(decode(&bytes).unwrap().1, encoding);
            // A signed file written back by a wallet loads and verifies.
            let signed = sign(step1.psbt(), &signers[0]);
            std::fs::write(&path, encode(&signed, encoding)).unwrap();
            let loaded = load(&path).unwrap();
            assert_eq!(loaded, signed);
            verify(&step1, &loaded).unwrap();
            let secp = secp256k1::Secp256k1::verification_only();
            finalize_split_step1(&step1, &loaded, &secp).unwrap();
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
        // Base64 text with surrounding whitespace, as pasted.
        let base64 = String::from_utf8(encode(step1.psbt(), Encoding::Base64)).unwrap();
        let text = format!("\n  {}\r\n", base64);
        assert_eq!(decode(text.as_bytes()).unwrap().0, *step1.psbt());
        // Line-wrapped at 64 columns, as some wallets save it.
        let wrapped: Vec<String> = base64
            .as_bytes()
            .chunks(64)
            .map(|line| String::from_utf8(line.to_vec()).unwrap())
            .collect();
        assert_eq!(
            decode(wrapped.join("\r\n").as_bytes()).unwrap().0,
            *step1.psbt()
        );
    }

    /// F3/F7 (#615 review): a returned file must carry a signature, and a
    /// finalized file is refused with its own message.
    #[test]
    fn split_psbt_file_refuses_unsigned_and_finalized_files() {
        let (step1, signers) = construction(Shape::Wpkh);
        assert_eq!(verify(&step1, step1.psbt()), Err(FileError::Unsigned));
        assert_eq!(
            combine(&step1, &[step1.psbt().clone()]),
            Err(FileError::File(0, Box::new(FileError::Unsigned)))
        );
        let signed = sign(step1.psbt(), &signers[0]);
        assert_eq!(
            combine(&step1, &[signed.clone(), step1.psbt().clone()]),
            Err(FileError::File(1, Box::new(FileError::Unsigned)))
        );
        let mut finalized = signed;
        coincube_core::miniscript::psbt::PsbtExt::finalize_mut(
            &mut finalized,
            &secp256k1::Secp256k1::verification_only(),
        )
        .unwrap();
        assert_eq!(verify(&step1, &finalized), Err(FileError::Finalized));
        assert!(FileError::Finalized
            .to_string()
            .contains("without finalizing"));
        assert!(FileError::Unsigned.to_string().contains("no signatures"));
    }

    /// F4 (#615 review): one cosigner's partial file with an invalid
    /// signature under that cosigner's own key is refused. Partial files are
    /// accepted through `FinalizeError::Unsatisfied`, so this pins that core
    /// checks every signature before it tries to finalize.
    #[test]
    fn split_psbt_partial_with_a_bad_signature_is_refused() {
        for shape in [Shape::WshSortedMulti, Shape::WshMulti] {
            let (step1, signers) = construction(shape);
            let partial = sign(step1.psbt(), &signers[0]);
            verify(&step1, &partial).unwrap();
            let mut bad = partial.clone();
            let foreign = *partial.inputs[0].partial_sigs.values().next().unwrap();
            let own = bad.inputs[1].partial_sigs.values_mut().next().unwrap();
            *own = foreign;
            assert_eq!(
                verify(&step1, &bad),
                Err(FileError::Refused(FinalizeError::InvalidSignature {
                    input: 1
                }))
            );
            assert!(matches!(
                combine(&step1, &[bad, sign(step1.psbt(), &signers[1])]),
                Err(FileError::File(0, _))
            ));
        }
    }

    #[test]
    fn split_psbt_file_refuses_non_psbt_and_oversized_input() {
        for bytes in [
            b"".as_slice(),
            b"not a psbt",
            b"psbt\xff\x00garbage",
            b"cHNidP8=",
            "aGVsbG8gd29ybGQ=".as_bytes(),
        ] {
            assert_eq!(decode(bytes).err(), Some(FileError::NotPsbt), "{bytes:?}");
        }
        let path = scratch("large.psbt");
        std::fs::write(&path, vec![b'A'; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert_eq!(load(&path).err(), Some(FileError::TooLarge));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        assert!(matches!(
            load(Path::new("/nonexistent/split.psbt")),
            Err(FileError::Io(_))
        ));
    }

    /// A returned file is refused unless it is the exact construction plus
    /// valid SIGHASH_ALL signatures.
    #[test]
    fn split_psbt_file_refuses_substitutions() {
        let (step1, signers) = construction(Shape::ShWpkh);
        let signed = sign(step1.psbt(), &signers[0]);
        verify(&step1, &signed).unwrap();

        // Another construction: a different destination amount.
        let mut other = signed.clone();
        other.unsigned_tx.output[1].value = Amount::from_sat(1_000);
        // Changed signer metadata.
        let mut metadata = signed.clone();
        metadata.inputs[0].bip32_derivation.clear();
        // Dropped previous transaction.
        let mut previous = signed.clone();
        previous.inputs[0].non_witness_utxo = None;
        // A different construction entirely: another wallet's step 1.
        let (foreign, foreign_signers) = construction(Shape::Wpkh);
        let foreign = sign(foreign.psbt(), &foreign_signers[0]);
        for substituted in [other, metadata, previous, foreign] {
            assert_eq!(
                verify(&step1, &substituted),
                Err(FileError::Refused(FinalizeError::ConstructionChanged))
            );
        }

        // Any sighash but ALL.
        let mut single = signed.clone();
        single.inputs[0].sighash_type = Some(PsbtSighashType::from(
            EcdsaSighashType::SinglePlusAnyoneCanPay,
        ));
        assert_eq!(
            verify(&step1, &single),
            Err(FileError::Refused(FinalizeError::UnsupportedSighash))
        );
        // A signature moved to the other input does not verify there.
        let mut moved = signed.clone();
        let (key, signature) = signed.inputs[0].partial_sigs.iter().next().unwrap();
        moved.inputs[1].partial_sigs.clear();
        moved.inputs[1].partial_sigs.insert(*key, *signature);
        assert!(matches!(
            verify(&step1, &moved),
            Err(FileError::Refused(FinalizeError::InvalidSignature { .. }))
        ));
        assert!(FileError::Refused(FinalizeError::ConstructionChanged)
            .to_string()
            .contains("exact exported file"));
    }

    /// Each cosigner signs its own copy; the files combine into one
    /// finalizable PSBT. A substituted file refuses the whole combine.
    #[test]
    fn split_psbt_files_combine_per_cosigner_and_refuse_a_substitute() {
        for shape in [Shape::WshSortedMulti, Shape::WshMulti] {
            let (step1, signers) = construction(shape);
            let files: Vec<_> = signers.iter().map(|s| sign(step1.psbt(), s)).collect();
            // One cosigner alone is a valid partial, not finalizable.
            verify(&step1, &files[0]).unwrap();
            let secp = secp256k1::Secp256k1::verification_only();
            assert_eq!(
                finalize_split_step1(&step1, &files[0], &secp).err(),
                Some(FinalizeError::Unsatisfied)
            );
            let combined = combine(&step1, &files).unwrap();
            let verified = finalize_split_step1(&step1, &combined, &secp).unwrap();
            assert!(verified.signatures_per_input().iter().all(|n| *n == 2));

            let (other, other_signers) = construction(Shape::Wpkh);
            let substitute = sign(other.psbt(), &other_signers[0]);
            assert_eq!(
                combine(&step1, &[files[0].clone(), substitute]),
                Err(FileError::File(
                    1,
                    Box::new(FileError::Refused(FinalizeError::ConstructionChanged))
                ))
            );
            let mut tampered = files[1].clone();
            tampered.outputs[1].bip32_derivation.clear();
            assert!(matches!(
                combine(&step1, &[files[0].clone(), tampered]),
                Err(FileError::File(1, _))
            ));
        }
        let (step1, _) = construction(Shape::Wpkh);
        assert_eq!(combine(&step1, &[]), Err(FileError::NoFiles));
        let many = vec![step1.psbt().clone(); MAX_COMBINED_FILES + 1];
        assert_eq!(combine(&step1, &many), Err(FileError::TooManyFiles));
    }
}
