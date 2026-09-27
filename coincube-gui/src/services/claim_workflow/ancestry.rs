//! Retained ancestry is structural data, never restored spend authority.
use super::*;
use coincube_core::{
    claim_ancestry::retained::{RetainedPath, MAX_ENCODED_BYTES},
    claim_spend::AncestrySelfTransfer,
    miniscript::bitcoin::OutPoint,
};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredAncestry {
    selected: OutPoint,
    raw: String,
}
impl StoredAncestry {
    fn decode(&self) -> Result<RetainedPath, Error> {
        if self.raw.len() > 2 * MAX_ENCODED_BYTES {
            return Err(Error::InvalidJournal);
        }
        let raw = hex::decode(&self.raw).map_err(|_| Error::InvalidJournal)?;
        RetainedPath::decode(self.selected, &raw).map_err(|_| Error::InvalidJournal)
    }
}
pub(super) fn validate_poison(intent: &Intent) -> Result<(), Error> {
    let p = &intent.plan;
    let inputs: BTreeSet<_> = p.step1.input.iter().map(|i| i.previous_output).collect();
    let claimed: BTreeSet<_> = p.claimed_prevouts.iter().copied().collect();
    match (&intent.ancestry, p.poison) {
        (None, Poison::OpReturn) if intent.version != 7 => {
            if inputs != claimed
                || !p
                    .step1
                    .output
                    .iter()
                    .any(|o| o.script_pubkey.is_op_return() && o.script_pubkey.len() > 83)
            {
                return Err(Error::InvalidPlan);
            }
        }
        (Some(stored), Poison::InputAncestry) if intent.version == 7 => {
            stored.decode()?;
            let mut shared = inputs;
            if p.bitcoin_chain != ChainId::Bitcoin
                || p.fork_chain != ChainId::BitcoinBlake2b
                || !shared.remove(&stored.selected)
                || shared.is_empty()
                || shared != claimed
                || p.step1.output.len() != 1
                || !p.step1.output[0].script_pubkey.is_p2wsh()
                || p.step1.output[0].value == coincube_core::miniscript::bitcoin::Amount::ZERO
            {
                return Err(Error::InvalidPlan);
            }
        }
        _ => return Err(Error::InvalidPlan),
    }
    Ok(())
}
impl Controller {
    /// Persist the owned construction and its structurally verified raw path in
    /// the same atomic intent. Fresh qualification is deliberately not stored.
    pub fn create_ancestry(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        artifact: &AncestrySelfTransfer,
        path: &RetainedPath,
        context: Context,
    ) -> Result<Self, Error> {
        path.reverify().map_err(|_| Error::InvalidPlan)?;
        if path.selected() != artifact.poison_input() {
            return Err(Error::WrongIdentity);
        }
        let identity = WalletIdentity {
            bitcoin_cube,
            fork_cube,
            descriptor_digest: sha256::Hash::hash(artifact.descriptor().to_string().as_bytes()),
        };
        Self::create_intent_with_ancestry(
            directory,
            identity,
            ClaimPlan {
                bitcoin_chain: artifact.chain(),
                fork_chain: ChainId::BitcoinBlake2b,
                step1: artifact.psbt().unsigned_tx.clone(),
                claimed_prevouts: artifact.claimed_prevouts().to_vec(),
                poison: Poison::InputAncestry,
                previous_confirmation: None,
            },
            context,
            Some(u32::from(artifact.change_index())),
            Some(StoredAncestry {
                selected: path.selected(),
                raw: hex::encode(path.encode()),
            }),
        )
    }
    /// Reverified structural links only. Callers must obtain new positive chain
    /// observations and rebuild owned construction before any further action.
    pub fn recorded_ancestry(&self) -> Result<Option<RetainedPath>, Error> {
        self.intent
            .ancestry
            .as_ref()
            .map(StoredAncestry::decode)
            .transpose()
    }
    /// Bind a new positive ancestry collection to the saved intent. This clears
    /// prior checks and never makes the controller eligible by itself.
    pub fn validate_ancestry_observation(
        &mut self,
        current: &Context,
        observed: &crate::services::claim_observation::http::DiscoveredAncestry,
        policy: Policy,
        now: i64,
        tips: coincube_core::claim::PreflightTips,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        if !self.construction_verified {
            return Err(Error::Unchecked);
        }
        let path = self.recorded_ancestry()?.ok_or(Error::WrongIdentity)?;
        observed
            .validate_for_plan(
                &path,
                &self.intent.plan,
                crate::services::claim_observation::http::AncestryContext {
                    provider: &current.provider,
                    generation: current.generation,
                    policy,
                    now,
                    tips,
                },
            )
            .map_err(|_| Error::Unchecked)
    }
    pub fn revalidate_ancestry_construction(
        &mut self,
        current: &Context,
        artifact: &AncestrySelfTransfer,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        self.construction_verified = false;
        let path = self.recorded_ancestry()?.ok_or(Error::WrongIdentity)?;
        if artifact.chain() != self.intent.plan.bitcoin_chain
            || path.selected() != artifact.poison_input()
            || artifact.claimed_prevouts() != self.intent.plan.claimed_prevouts
            || digest(&artifact.psbt().unsigned_tx) != self.intent.unsigned_digest
            || sha256::Hash::hash(artifact.descriptor().to_string().as_bytes())
                != self.intent.identity.descriptor_digest
            || self.intent.bitcoin_change_index != Some(u32::from(artifact.change_index()))
        {
            return Err(Error::WrongIdentity);
        }
        self.construction_verified = true;
        Ok(())
    }
}
