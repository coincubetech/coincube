//! The Bitcoin Blake2b **Claim target** — the Cube a claim is made *into*.
//!
//! A claim reuses one Bitcoin Cube's Vault descriptor on the fork chain, so
//! the target watches exactly the addresses the source Cube already controls.
//! Creating it is all this module does: no poison, no sweep, no broadcast.
//! Step two of the claim (`services::claim_workflow::Step2Authorization`)
//! stays uninhabited.

use coincube_core::miniscript::bitcoin::bip32::Fingerprint;
use std::sync::Arc;

use crate::signer::Signer;

/// The Bitcoin Cube a claim target is built from.
///
/// Carried in [`super::UserFlow::ClaimBlake2b`] rather than in the installer's
/// `cube_settings`: those describe the Cube the installer is *building*, and a
/// claim builds a Bitcoin Blake2b Cube while running inside a Bitcoin one.
/// Handing the source Cube's settings to a fork installer would defeat the
/// chain check in [`super::Installer::try_new_for_chain`] rather than satisfy
/// it.
#[derive(Clone)]
pub struct ClaimSource {
    /// The source Cube itself, carried whole rather than field by field: the
    /// installer needs its id (session lookup), its name (the target's alias),
    /// its backup state (inherited by the target, which shares the mnemonic)
    /// **and** the settings themselves, to rebuild the source Cube if the user
    /// backs out of the claim.
    pub cube: crate::app::settings::CubeSettings,
    /// The descriptor the target reuses, verbatim. The whole point of a claim:
    /// a different string would watch different addresses.
    pub descriptor: coincube_core::descriptors::CoincubeDescriptor,
    /// The source Cube's master signer, already unlocked in this session.
    ///
    /// The target must be able to *spend* what it watches, and the descriptor's
    /// keys are derived from this seed. See [`target_master_seed`].
    pub signer: Arc<Signer>,
}

impl std::fmt::Debug for ClaimSource {
    /// Deliberately opaque: this type owns an unlocked master signer.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimSource")
            .field("cube_id", &self.cube.id)
            .finish_non_exhaustive()
    }
}

impl ClaimSource {
    /// The source Cube's id — what the session PIN and the unlocked signer are
    /// keyed by.
    pub fn cube_id(&self) -> &str {
        &self.cube.id
    }

    /// The default alias for the target Cube.
    pub fn default_target_alias(&self) -> String {
        format!("{} · BTCB2", self.cube.name)
    }

    /// Whether the source Cube's seed is recorded as backed up. Inherited
    /// rather than reset: the target's seed *is* the source's, so a mnemonic
    /// the user has already written down does not become un-backed-up because
    /// a second Cube now shares it — and the claim flow deliberately does not
    /// show it again.
    pub fn seed_backed_up(&self) -> bool {
        self.cube.backed_up
    }

    /// The fingerprint the target's descriptor keys derive from — the source
    /// Cube's, because the descriptor is the source Cube's.
    pub fn master_signer_fingerprint(&self) -> Fingerprint {
        self.signer.fingerprint()
    }
}

/// **The claim target's seed decision, and the only place it is made.**
///
/// The target is created from the source Cube's descriptor, whose keys derive
/// from the source Cube's master seed. So the seed is not a UX choice: a target
/// holding an unrelated seed would watch the right addresses and be unable to
/// sign for any of them.
///
/// The recorded decision (`plans/bitcoin-blake2b/PLAN-bitcoin-blake2b-coincube.md`
/// PR 4, invariant I10) is therefore to reuse the source Cube's seed, which
/// also keeps PIN and duress behaviour identical between the two Cubes. On
/// disk that is a second encrypted copy of the same mnemonic, written into the
/// fork chain's own `bitcoin-blake2b/mnemonics` folder under the source Cube's
/// PIN — the chains never share a seed *file*, by
/// [`crate::services::unlock::seed_folder`].
///
/// Returning the signer rather than writing here is what keeps the decision
/// reversible: a watch-only target that delegates signing to the source Cube
/// would return `None` from this one function, and nothing else would change
/// shape.
pub fn target_master_seed(source: &ClaimSource) -> Option<Arc<Signer>> {
    Some(source.signer.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::Network;

    /// A mainnet Vault descriptor, shape-representative; the claim path never
    /// inspects its contents, only carries it verbatim.
    /// A real mainnet Vault descriptor (the fixture `loader.rs` uses). The
    /// claim path never inspects its contents, only carries it verbatim.
    const DESC: &str = "tr([abcdef01]xpub6Eze7yAT3Y1wGrnzedCNVYDXUqa9NmHVWck5emBaTbXtURbe1NWZbK9bsz1TiVE7Cz341PMTfYgFw1KdLWdzcM1UMFTcdQfCYhhXZ2HJvTW/<0;1>/*,and_v(v:pk([abcdef01]xpub688Hn4wScQAAiYJLPg9yH27hUpfZAUnmJejRQBCiwfP5PEDzjWMNW1wChcninxr5gyavFqbbDjdV1aK5USJz8NDVjUy7FRQaaqqXHh5SbXe/<0;1>/*),older(52560)))#0mt7e93c";

    fn source(name: &str) -> ClaimSource {
        use std::str::FromStr;
        let signer = Signer::generate(Network::Bitcoin).unwrap();
        ClaimSource {
            cube: crate::app::settings::CubeSettings::new_with_raw_id(
                "cube-1".into(),
                name.into(),
                coincube_core::chain::ChainId::Bitcoin,
            ),
            descriptor: coincube_core::descriptors::CoincubeDescriptor::from_str(DESC).unwrap(),
            signer: Arc::new(signer),
        }
    }

    #[test]
    fn the_target_alias_defaults_to_the_source_name_plus_the_chain() {
        assert_eq!(source("Savings").default_target_alias(), "Savings · BTCB2");
    }

    #[test]
    fn the_target_seed_is_the_source_cube_seed() {
        let s = source("Savings");
        let seed = target_master_seed(&s).expect("the recorded decision reuses the source seed");
        assert_eq!(
            seed.fingerprint(),
            s.signer.fingerprint(),
            "a target signing for the source descriptor needs the source seed"
        );
    }

    #[test]
    fn the_debug_rendering_never_carries_seed_material() {
        let s = source("Savings");
        let rendered = format!("{s:?}");
        assert!(rendered.contains("cube-1"), "{}", rendered);
        for word in s.signer.mnemonic() {
            assert!(!rendered.contains(word), "{}", rendered);
        }
    }
}
