//! Seeds for the Split unified fallback (#568 B4b; owner decisions P8 and
//! C3): up to `threshold` BIP39 seeds held for one session, each bound to a
//! key origin of the foreign wallet's policy, with a passphrase per seed.
//!
//! A foreign single-sig wallet needs one seed; a `k`-of-`n` multisig needs
//! `k`. A seed is admitted only when its master fingerprint is one of the
//! policy's key origins, once, and only while the seeds held cover fewer
//! than `threshold` policy keys. Signing runs every held seed through
//! [`SessionSeedSource::sign_unified`], so the result carries `ALL|UNIFIED`
//! records for exactly the keys those seeds control and nothing else; the
//! core finalizer decides whether that satisfies every input.
//!
//! The binding is by master fingerprint only (#647 O3): four bytes, which a
//! seed can match without deriving the policy's key at that origin (a
//! collision, or the right seed with a wrong passphrase whose fingerprint
//! happens to match). Admission is therefore no proof of the key. The core
//! finalizer verifies every record against the keys each input commits to,
//! so such a seed can only end in an incomplete or refused signing, never in
//! an accepted wrong signature. The user-facing copy for that case is
//! B4b-3's.
//!
//! This module keeps nothing: no file, no configuration entry, no encrypted
//! store and no session cache. Each seed lives in a zeroizing
//! [`SessionSeedSource`]; a refused seed is dropped at once, and clearing or
//! dropping the set scrubs every held one. `Debug` output names counts only.
//! Nothing in the app reaches this module yet (D1).

use std::fmt;

use coincube_core::{
    chain::ChainId,
    foreign_split::SplitSource,
    miniscript::{
        bitcoin::{
            bip32::Fingerprint,
            secp256k1::{self, Secp256k1},
        },
        descriptor::{ShInner, WshInner},
        Descriptor, DescriptorPublicKey, ForEachKey, Terminal,
    },
    psbt_unified::UnifiedPsbt,
    unified_foreign::ForeignUnifiedError,
};
use zeroize::Zeroizing;

use super::foreign_wallet_source::{SessionSeedSource, SourceError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedSetError {
    /// Not a supported Split shape, or a key without an origin fingerprint:
    /// no seed could be matched to it.
    UnsupportedPolicy,
    /// The words or passphrase do not form a BIP39 seed.
    Seed(SourceError),
    /// The seed's master fingerprint is not one of the policy's key origins
    /// (a wrong seed, or a policy seed with the wrong passphrase). A match is
    /// not proof of the key at that origin: see the module note (#647 O3).
    UnknownOrigin,
    /// A seed with this fingerprint is already held.
    Duplicate,
    /// The seeds held already cover the threshold.
    Full {
        threshold: usize,
    },
    /// The seeds held cover fewer policy keys than the threshold needs.
    Incomplete {
        have: usize,
        need: usize,
    },
    Signing(ForeignUnifiedError),
}

impl fmt::Display for SeedSetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPolicy => f.write_str("This wallet's keys cannot be matched to seeds"),
            Self::Seed(_) => f.write_str("Not a valid recovery phrase"),
            Self::UnknownOrigin => {
                f.write_str("This seed (with this passphrase) is not one of the wallet's keys")
            }
            Self::Duplicate => f.write_str("This seed is already entered"),
            Self::Full { threshold } => {
                write!(
                    f,
                    "The wallet needs {threshold} seed(s), and they are all entered"
                )
            }
            Self::Incomplete { have, need } => {
                write!(f, "{have} of {need} seeds entered")
            }
            Self::Signing(err) => write!(f, "Signing refused: {err}"),
        }
    }
}

impl std::error::Error for SeedSetError {}

/// The signing policy of a foreign source: the origin fingerprint of every
/// key and the threshold. Public material only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedPolicy {
    origins: Vec<Fingerprint>,
    threshold: usize,
}

impl SeedPolicy {
    /// Read the policy from the source's receive descriptor (the change
    /// descriptor is the same wallet, checked by `SplitSource`).
    pub fn of(source: &SplitSource) -> Result<Self, SeedSetError> {
        let descriptor = source.external();
        let threshold = match descriptor {
            Descriptor::Pkh(_) | Descriptor::Wpkh(_) => 1,
            Descriptor::Sh(sh) => match sh.as_inner() {
                ShInner::Wpkh(_) => 1,
                _ => return Err(SeedSetError::UnsupportedPolicy),
            },
            Descriptor::Wsh(wsh) => match wsh.as_inner() {
                WshInner::SortedMulti(sorted) => sorted.k(),
                WshInner::Ms(ms) => match &ms.node {
                    Terminal::Multi(thresh) => thresh.k(),
                    _ => return Err(SeedSetError::UnsupportedPolicy),
                },
            },
            _ => return Err(SeedSetError::UnsupportedPolicy),
        };
        let mut origins = Vec::new();
        let mut supported = true;
        descriptor.for_each_key(|key| {
            match origin_fingerprint(key) {
                Some(fingerprint) => origins.push(fingerprint),
                None => supported = false,
            }
            true
        });
        if !supported || origins.is_empty() || threshold == 0 || threshold > origins.len() {
            return Err(SeedSetError::UnsupportedPolicy);
        }
        Ok(Self { origins, threshold })
    }

    pub fn threshold(&self) -> usize {
        self.threshold
    }

    /// One origin fingerprint per policy key, in descriptor order.
    pub fn origins(&self) -> &[Fingerprint] {
        &self.origins
    }
}

fn origin_fingerprint(key: &DescriptorPublicKey) -> Option<Fingerprint> {
    let origin = match key {
        DescriptorPublicKey::XPub(xpub) => xpub.origin.as_ref(),
        DescriptorPublicKey::Single(single) => single.origin.as_ref(),
        DescriptorPublicKey::MultiXPub(xpub) => xpub.origin.as_ref(),
    };
    origin
        .map(|(fingerprint, _)| *fingerprint)
        .filter(|fingerprint| *fingerprint != Fingerprint::default())
}

/// The seeds entered for one unified sweep, each bound to a policy origin.
pub struct SeedSet {
    policy: SeedPolicy,
    held: Vec<(Fingerprint, SessionSeedSource)>,
}

impl fmt::Debug for SeedSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SeedSet")
            .field("threshold", &self.policy.threshold)
            .field("held", &self.held.len())
            .finish_non_exhaustive()
    }
}

impl SeedSet {
    pub fn new(source: &SplitSource) -> Result<Self, SeedSetError> {
        Ok(Self {
            policy: SeedPolicy::of(source)?,
            held: Vec::new(),
        })
    }

    pub fn policy(&self) -> &SeedPolicy {
        &self.policy
    }

    pub fn threshold(&self) -> usize {
        self.policy.threshold
    }

    /// Seeds held.
    pub fn len(&self) -> usize {
        self.held.len()
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// The master fingerprints of the seeds held: public material.
    pub fn fingerprints(&self) -> Vec<Fingerprint> {
        self.held
            .iter()
            .map(|(fingerprint, _)| *fingerprint)
            .collect()
    }

    /// How many policy keys the seeds held control.
    pub fn covered(&self) -> usize {
        self.policy
            .origins
            .iter()
            .filter(|origin| self.held.iter().any(|(held, _)| held == *origin))
            .count()
    }

    /// Whether the seeds held cover the threshold.
    pub fn is_complete(&self) -> bool {
        self.covered() >= self.policy.threshold
    }

    /// Add one seed. `words` and `passphrase` are consumed and scrubbed
    /// whether or not the seed is accepted, and a refused seed is dropped,
    /// and so scrubbed, before returning. Returns the seed's fingerprint.
    pub fn add(
        &mut self,
        words: Zeroizing<String>,
        passphrase: Zeroizing<String>,
    ) -> Result<Fingerprint, SeedSetError> {
        if self.is_complete() {
            return Err(SeedSetError::Full {
                threshold: self.policy.threshold,
            });
        }
        let seed = SessionSeedSource::new(words, passphrase).map_err(SeedSetError::Seed)?;
        let fingerprint = seed.fingerprint();
        if !self.policy.origins.contains(&fingerprint) {
            return Err(SeedSetError::UnknownOrigin);
        }
        if self.held.iter().any(|(held, _)| *held == fingerprint) {
            return Err(SeedSetError::Duplicate);
        }
        self.held.push((fingerprint, seed));
        Ok(fingerprint)
    }

    /// Sign `psbt` with every seed held, one after another, each through
    /// [`SessionSeedSource::sign_unified`] (Bitcoin Blake2b only, `0x21`
    /// only, every result verified). Refused until the set is complete. The
    /// input PSBT is not changed.
    pub fn sign_unified(
        &self,
        psbt: &UnifiedPsbt,
        chain: ChainId,
        secp: &Secp256k1<secp256k1::All>,
    ) -> Result<UnifiedPsbt, SeedSetError> {
        if !self.is_complete() {
            return Err(SeedSetError::Incomplete {
                have: self.covered(),
                need: self.policy.threshold,
            });
        }
        let mut signed = psbt.clone();
        for (_, seed) in &self.held {
            signed = seed
                .sign_unified(&signed, chain, secp)
                .map_err(SeedSetError::Signing)?;
        }
        Ok(signed)
    }

    /// Drop every seed held; each is scrubbed by its signer's `Drop`.
    pub fn clear(&mut self) {
        self.held.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::{
        bip39::Mnemonic,
        claim::BlockRef,
        foreign_split::{
            create_unified_sweep, finalize_unified_sweep, SplitBranch, SplitCoin, UnifiedInputs,
            UnifiedReplayStatus,
        },
        miniscript::bitcoin::{
            absolute::LockTime, bip32::DerivationPath, hashes::Hash, transaction, Amount,
            BlockHash, Network, OutPoint, ScriptBuf, Transaction, TxIn, TxOut, Txid,
        },
        signer::SessionSigner,
    };
    use std::{
        fs,
        path::Path,
        str::FromStr,
        time::{SystemTime, UNIX_EPOCH},
    };

    const FORK: u64 = 900;
    const BTCB2: ChainId = ChainId::BitcoinBlake2b;

    fn words(byte: u8) -> Zeroizing<String> {
        Zeroizing::new(Mnemonic::from_entropy(&[byte; 16]).unwrap().to_string())
    }

    fn passphrase(text: &str) -> Zeroizing<String> {
        Zeroizing::new(text.to_owned())
    }

    /// A policy key's public material, derived the way the seed set's own
    /// signer will derive it.
    fn account(byte: u8, passphrase: &str, path: &str) -> String {
        let secp = Secp256k1::new();
        let signer = SessionSigner::from_mnemonic(
            Network::Bitcoin,
            Mnemonic::from_entropy(&[byte; 16]).unwrap(),
            passphrase,
        )
        .unwrap();
        let origin = DerivationPath::from_str(path).unwrap();
        format!(
            "[{}/{}]{}",
            signer.fingerprint(&secp),
            path.trim_start_matches("m/"),
            signer.xpub_at(&origin, &secp)
        )
    }

    fn source(template: &str) -> SplitSource {
        let branch =
            |b: u32| Descriptor::from_str(&template.replace("{b}", &b.to_string())).unwrap();
        SplitSource::new(branch(0), Some(branch(1))).unwrap()
    }

    /// A 2-of-3 sortedmulti of seeds 1, 2 and 3; seed 2 has a passphrase.
    fn multisig_source() -> SplitSource {
        let keys: Vec<_> = [(1, ""), (2, "second passphrase"), (3, "")]
            .iter()
            .map(|(byte, passphrase)| {
                format!("{}/{{b}}/*", account(*byte, passphrase, "m/48'/0'/0'/2'"))
            })
            .collect();
        source(&format!("wsh(sortedmulti(2,{}))", keys.join(",")))
    }

    fn coin(source: &SplitSource, branch: SplitBranch, index: u32, sats: u64) -> SplitCoin {
        let descriptor = match branch {
            SplitBranch::External => source.external(),
            SplitBranch::Internal => source.internal().unwrap(),
        };
        let script = descriptor
            .at_derivation_index(index)
            .unwrap()
            .script_pubkey();
        let previous = Transaction {
            version: transaction::Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::new(Txid::from_byte_array([index as u8 + 1; 32]), 0),
                ..TxIn::default()
            }],
            output: vec![TxOut {
                value: Amount::from_sat(sats),
                script_pubkey: script,
            }],
        };
        let block = BlockRef {
            height: FORK - 10,
            hash: BlockHash::from_byte_array([0x33; 32]),
        };
        SplitCoin {
            outpoint: OutPoint::new(previous.compute_txid(), 0),
            branch,
            index,
            previous,
            bitcoin_block: Some(block),
            btcb2_block: Some(block),
        }
    }

    #[test]
    fn seed_set_binds_to_policy_caps_at_threshold_and_has_no_persistence() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let datadir = std::env::temp_dir().join(format!("coincube-split-seed-set-{unique}"));
        fs::create_dir(&datadir).unwrap();

        let wallet = multisig_source();
        let mut set = SeedSet::new(&wallet).unwrap();
        assert_eq!(set.threshold(), 2);
        assert_eq!(set.policy().origins().len(), 3);
        assert!(set.is_empty() && !set.is_complete());

        // Unknown origin: a seed outside the policy, and a policy seed with
        // the wrong passphrase (another fingerprint). Not a seed at all.
        assert_eq!(
            set.add(words(9), passphrase("")),
            Err(SeedSetError::UnknownOrigin)
        );
        assert_eq!(
            set.add(words(2), passphrase("")),
            Err(SeedSetError::UnknownOrigin)
        );
        assert_eq!(
            set.add(Zeroizing::new("not a seed".to_owned()), passphrase("")),
            Err(SeedSetError::Seed(SourceError::Mnemonic))
        );
        assert!(set.is_empty());

        // A real sweep of this wallet's coins, for the signing checks.
        let coins = [
            coin(&wallet, SplitBranch::External, 0, 100_000),
            coin(&wallet, SplitBranch::Internal, 3, 50_000),
        ];
        let target = ScriptBuf::new_p2wsh(&ScriptBuf::from_bytes(vec![0x51]).wscript_hash());
        let sweep = create_unified_sweep(
            &UnifiedInputs {
                chain: BTCB2,
                source: &wallet,
                coins: &coins,
                fork_height: FORK,
                target: &target,
            },
            5,
            LockTime::ZERO,
            960,
        )
        .unwrap();
        let unsigned = UnifiedPsbt::from_psbt(sweep.psbt().clone()).unwrap();
        let secp = Secp256k1::new();
        assert_eq!(
            set.sign_unified(&unsigned, BTCB2, &secp).err(),
            Some(SeedSetError::Incomplete { have: 0, need: 2 })
        );

        // Bound to its origin, once.
        let first = set.add(words(1), passphrase("")).unwrap();
        assert!(set.policy().origins().contains(&first));
        assert_eq!(set.fingerprints(), vec![first]);
        assert_eq!(
            set.add(words(1), passphrase("")),
            Err(SeedSetError::Duplicate)
        );
        assert_eq!(set.covered(), 1);
        assert_eq!(
            set.sign_unified(&unsigned, BTCB2, &secp).err(),
            Some(SeedSetError::Incomplete { have: 1, need: 2 })
        );
        let second = set.add(words(2), passphrase("second passphrase")).unwrap();
        assert_ne!(first, second);
        assert!(set.is_complete());
        assert_eq!(set.len(), 2);

        // Capped at the threshold: the third policy seed is refused.
        assert_eq!(
            set.add(words(3), passphrase("")),
            Err(SeedSetError::Full { threshold: 2 })
        );
        assert_eq!(set.len(), 2);

        // Debug names counts only.
        assert_eq!(format!("{set:?}"), "SeedSet { threshold: 2, held: 2, .. }");

        // A complete set signs the sweep, which finalizes Protected with the
        // two seeds' signatures on every input.
        let signed = set.sign_unified(&unsigned, BTCB2, &secp).unwrap();
        let verified = finalize_unified_sweep(&sweep, &signed, &secp).unwrap();
        assert_eq!(verified.replay_status(), UnifiedReplayStatus::Protected);
        assert!(verified.inputs().iter().all(|r| r.unified_used == 2));
        // The set refuses the Bitcoin chain through its signer.
        assert!(matches!(
            set.sign_unified(&unsigned, ChainId::Bitcoin, &secp),
            Err(SeedSetError::Signing(
                ForeignUnifiedError::NotBitcoinBlake2b(ChainId::Bitcoin)
            ))
        ));

        // Clearing scrubs: nothing is left to sign with.
        set.clear();
        assert!(set.is_empty());
        assert_eq!(
            set.sign_unified(&unsigned, BTCB2, &secp).err(),
            Some(SeedSetError::Incomplete { have: 0, need: 2 })
        );
        drop(set);

        // Single-sig: threshold 1, and the one seed completes the set.
        let single = source(&format!("wpkh({}/{{b}}/*)", account(4, "", "m/84'/0'/0'")));
        let mut set = SeedSet::new(&single).unwrap();
        assert_eq!(set.threshold(), 1);
        assert_eq!(
            set.add(words(1), passphrase("")),
            Err(SeedSetError::UnknownOrigin)
        );
        set.add(words(4), passphrase("")).unwrap();
        assert!(set.is_complete());
        assert_eq!(
            set.add(words(4), passphrase("")),
            Err(SeedSetError::Full { threshold: 1 })
        );

        // A key without an origin cannot be matched to any seed.
        let bare = account(4, "", "m/84'/0'/0'");
        let bare = &bare[bare.find(']').unwrap() + 1..];
        assert_eq!(
            SeedSet::new(&source(&format!("wpkh({bare}/{{b}}/*)"))).err(),
            Some(SeedSetError::UnsupportedPolicy)
        );
        // One origin-less key among three is refused too, not silently
        // reduced to a two-key policy (#647 O2).
        let with_origin = [
            account(1, "", "m/48'/0'/0'/2'"),
            account(2, "second passphrase", "m/48'/0'/0'/2'"),
        ];
        let third = account(3, "", "m/48'/0'/0'/2'");
        let third = &third[third.find(']').unwrap() + 1..];
        let mixed = format!(
            "wsh(sortedmulti(2,{}/{{b}}/*,{}/{{b}}/*,{}/{{b}}/*))",
            with_origin[0], with_origin[1], third
        );
        assert_eq!(
            SeedSet::new(&source(&mixed)).err(),
            Some(SeedSetError::UnsupportedPolicy)
        );

        // Nothing was written anywhere.
        assert_eq!(fs::read_dir(&datadir).unwrap().count(), 0);
        fs::remove_dir(datadir).unwrap();

        // The production part of this module names no persistence API.
        // Normalized to LF: Git for Windows checks the file out with CRLF
        // (`core.autocrlf`), which would hide the marker (#647 F1).
        let text = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/services/split_seed.rs"),
        )
        .unwrap()
        .replace("\r\n", "\n");
        let marker = "#[cfg(test)]\nmod tests {";
        assert_eq!(text.matches(marker).count(), 1);
        let production = &text[..text.find(marker).unwrap()];
        assert!(production.contains("pub fn sign_unified"));
        for forbidden in [
            "fs::",
            "serde",
            "store_encrypted",
            "store_unlocked_signer",
            "File",
            "settings",
            "keyring",
        ] {
            assert!(
                !production.contains(forbidden),
                "split_seed.rs names {}",
                forbidden
            );
        }

        // D1: nothing in the app reaches the seed set or the unified sweep
        // API. The Daemon trait and the embedded daemon only forward the
        // transport.
        fn walk(dir: &Path, files: &mut Vec<(String, String)>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, files);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push((
                        path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                        fs::read_to_string(&path).unwrap(),
                    ));
                }
            }
        }
        let mut files = Vec::new();
        walk(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let own = [
            "src/services/split_seed.rs",
            "src/services/foreign_wallet_source.rs",
        ];
        let transport = ["src/daemon/mod.rs", "src/daemon/embedded.rs"];
        let mut unexpected = Vec::new();
        for (file, text) in &files {
            if own.contains(&file.as_str()) {
                continue;
            }
            // The module declaration, and nothing else, in the services list.
            if file == "src/services/mod.rs" {
                assert_eq!(text.matches("split_seed").count(), 1, "{}", file);
                assert!(text.contains("pub mod split_seed;"));
                continue;
            }
            // The forwarded transport names are removed before the check so
            // that `UnifiedSweep` inside `VerifiedUnifiedSweep` is not a hit.
            let text = if transport.contains(&file.as_str()) {
                text.replace("submit_verified_unified_sweep", "")
                    .replace("VerifiedUnifiedSweep", "")
            } else {
                text.clone()
            };
            for ident in [
                "SeedSet",
                "SeedPolicy",
                "split_seed",
                "create_unified_sweep",
                "reconstruct_unified_sweep",
                "finalize_unified_sweep",
                "verify_unified_sweep_transaction",
                "UnifiedSweep",
                "VerifiedUnifiedSweep",
                "submit_verified_unified_sweep",
                "for_unified_sweep",
            ] {
                if text.contains(ident) {
                    unexpected.push(format!("{} names {}", file, ident));
                }
            }
        }
        assert!(unexpected.is_empty(), "{:?}", unexpected);
    }
}
