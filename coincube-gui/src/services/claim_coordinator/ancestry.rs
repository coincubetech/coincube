//! Ancestry admission retains its separate proof and signed artifact types.
//! Review eligibility requires fresh live proof, owned construction and preflight.
use super::*;
use coincube_core::{
    claim_ancestry::retained::RetainedPath, claim_finalize::VerifiedAncestryTransfer,
    claim_spend::AncestrySelfTransfer,
};
impl Coordinator {
    #[allow(clippy::too_many_arguments)]
    pub fn create_ancestry(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        construction: &AncestrySelfTransfer,
        path: &RetainedPath,
        verified: VerifiedAncestryTransfer,
        production: Production,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        Self::admit_ancestry(
            directory,
            bitcoin_cube,
            fork_cube,
            construction,
            path,
            verified,
            production,
            policy,
            false,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn resume_ancestry(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        construction: &AncestrySelfTransfer,
        path: &RetainedPath,
        verified: VerifiedAncestryTransfer,
        production: Production,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        Self::admit_ancestry(
            directory,
            bitcoin_cube,
            fork_cube,
            construction,
            path,
            verified,
            production,
            policy,
            true,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn admit_ancestry(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        construction: &AncestrySelfTransfer,
        path: &RetainedPath,
        verified: VerifiedAncestryTransfer,
        production: Production,
        policy: CheckPolicy,
        resume: bool,
    ) -> Result<Self, Error> {
        if production
            .daemon
            .config()
            .is_none_or(|c| c.main_descriptor != *construction.descriptor())
        {
            return Err(Error::InvalidBinding);
        }
        let context = production.context.clone();
        let generation = production.generation.clone();
        Self::open_ancestry(
            directory,
            bitcoin_cube,
            fork_cube,
            construction,
            path,
            verified,
            context,
            generation,
            Box::new(production),
            policy,
            resume,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn open_ancestry(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        construction: &AncestrySelfTransfer,
        path: &RetainedPath,
        verified: VerifiedAncestryTransfer,
        context: Context,
        generation: watch::Receiver<u64>,
        services: Box<dyn Services>,
        policy: CheckPolicy,
        resume: bool,
    ) -> Result<Self, Error> {
        if !policy.valid()
            || construction.chain() != ChainId::Bitcoin
            || !admits_descriptor(construction.descriptor())
            || construction
                .psbt()
                .unsigned_tx
                .input
                .iter()
                .any(|i| i.sequence.is_relative_lock_time())
            || verified
                .signatures_per_input()
                .iter()
                .any(|n| *n != primary_threshold(construction.descriptor()))
        {
            return Err(Error::Unsupported);
        }
        let mut unsigned = verified.transaction().clone();
        for input in &mut unsigned.input {
            input.witness.clear();
        }
        if verified.chain() != construction.chain()
            || verified.descriptor() != construction.descriptor()
            || unsigned != construction.psbt().unsigned_tx
            || verified.construction_txid() != unsigned.compute_txid()
            || verified.poison_input() != construction.poison_input()
            || verified.claimed_prevouts() != construction.claimed_prevouts()
            || path.selected() != construction.poison_input()
            || path.reverify().is_err()
            || *generation.borrow() != context.generation
            || generation.has_changed().is_err()
        {
            return Err(Error::InvalidBinding);
        }
        let identity = WalletIdentity {
            bitcoin_cube,
            fork_cube,
            descriptor_digest: sha256::Hash::hash(construction.descriptor().to_string().as_bytes()),
        };
        let mut controller = if resume {
            Controller::reopen(directory, &identity, context.clone())?
        } else {
            Controller::create_ancestry(
                directory,
                identity.bitcoin_cube,
                identity.fork_cube,
                construction,
                path,
                context.clone(),
            )?
        };
        controller.revalidate_ancestry_construction(&context, construction)?;
        let saved = controller
            .recorded_ancestry()?
            .ok_or(Error::InvalidBinding)?;
        if saved.selected() != path.selected() || saved.encode() != path.encode() {
            return Err(Error::InvalidBinding);
        }
        if controller.phase() != Phase::Intent
            && controller.recorded_bitcoin_transaction() != Some(verified.transaction())
        {
            return Err(Error::InvalidBinding);
        }
        let id = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| Error::Revoked)?;
        Ok(Self {
            id,
            revision: 0,
            context,
            generation,
            controller,
            verified: VerifiedStep1::Ancestry(Arc::new(verified)),
            services,
            policy,
            revoker: Revoker::new(),
        })
    }
}
