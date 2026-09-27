//! Consume transient chain evidence only in the context that collected it.
use super::*;
use coincube_core::claim_ancestry::retained::RetainedPath;

/// Current caller context, not stored authority. Both tips must come from the
/// fresh ordinary transaction/deployment preflight collection.
pub struct AncestryContext<'a> {
    pub provider: &'a str,
    pub generation: u64,
    pub policy: Policy,
    pub now: i64,
    pub tips: PreflightTips,
}
impl DiscoveredAncestry {
    /// Check the exact retained dependency against the intended input partition,
    /// current session, and both current chain tips. No ownership, maturity,
    /// unspentness, mempool acceptance, or signing permission is established.
    /// Call again immediately before use; a prior success is not a capability.
    pub fn validate_for_plan(
        &self,
        path: &RetainedPath,
        plan: &ClaimPlan,
        context: AncestryContext<'_>,
    ) -> Result<(), FailureKind> {
        if context.generation != self.generation()
            || *self.live_generation.borrow() != self.generation()
            || self.live_generation.has_changed().is_err()
        {
            return Err(FailureKind::Cancelled);
        }
        if context.provider.trim_end_matches('/') != self.provider {
            return Err(FailureKind::Changed);
        }
        if context.policy.max_observation_age_seconds <= 0
            || context.policy.expiry_margin_seconds <= 0
        {
            return Err(FailureKind::InvalidPlan);
        }
        if !super::super::super::fresh(
            self.observed_at(),
            context.now,
            context.policy.max_observation_age_seconds,
        ) {
            return Err(FailureKind::Stale);
        }
        if context.tips.bitcoin != self.pair.bitcoin().tip
            || context.tips.fork != self.pair.fork().tip
        {
            return Err(FailureKind::Changed);
        }
        if plan.bitcoin_chain != ChainId::Bitcoin || plan.fork_chain != ChainId::BitcoinBlake2b {
            return Err(FailureKind::WrongChain);
        }
        if path.selected() != self.pair.selected()
            || path.links().len() != self.links.len()
            || path
                .links()
                .iter()
                .zip(&self.links)
                .any(|(saved, observed)| {
                    saved.parent_input != observed.parent_input
                        || saved.transaction != observed.transaction
                })
        {
            return Err(FailureKind::Changed);
        }
        path.reverify().map_err(|_| FailureKind::Malformed)?;
        validate_partition(path, plan)
    }
}

pub(super) fn validate_partition(path: &RetainedPath, plan: &ClaimPlan) -> Result<(), FailureKind> {
    if plan.bitcoin_chain != ChainId::Bitcoin || plan.fork_chain != ChainId::BitcoinBlake2b {
        return Err(FailureKind::WrongChain);
    }
    let mut inputs: BTreeSet<_> = plan
        .step1
        .input
        .iter()
        .map(|input| input.previous_output)
        .collect();
    let claimed: BTreeSet<_> = plan.claimed_prevouts.iter().copied().collect();
    if plan.poison != Poison::InputAncestry
        || inputs.len() != plan.step1.input.len()
        || claimed.len() != plan.claimed_prevouts.len()
        || claimed.is_empty()
        || !inputs.remove(&path.selected())
        || inputs != claimed
        || plan.step1.input.iter().any(|input| {
            input.previous_output.is_null()
                || !input.script_sig.is_empty()
                || !input.witness.is_empty()
        })
        || plan.step1.output.len() != 1
        || !plan.step1.output[0].script_pubkey.is_p2wsh()
        || plan.step1.output[0].value == coincube_core::miniscript::bitcoin::Amount::ZERO
    {
        return Err(FailureKind::InvalidPlan);
    }
    Ok(())
}
