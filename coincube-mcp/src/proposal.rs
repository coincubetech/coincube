//! Bounded session-only topology drafts. No keys, descriptors or wallet engine.
use crate::{Error, Reader, Request, Response};
use rand::RngCore;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// A requested quorum, not evidence of independently controlled signers.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Quorum {
    /// Required signatures for this path.
    #[schemars(range(min = 1, max = 5))]
    pub threshold: u8,
    /// Number of signer slots in this path.
    #[schemars(range(min = 1, max = 5))]
    pub key_count: u8,
}
impl Quorum {
    fn valid(self) -> bool {
        self.threshold > 0 && self.threshold <= self.key_count && self.key_count <= 5
    }
}

/// Explicitly requested recovery, separate from the primary spending quorum.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Recovery {
    /// Signing requirement after the relative delay.
    pub quorum: Quorum,
    /// Positive block delay; not a calendar date or eligibility claim.
    #[schemars(range(min = 1, max = 65535))]
    pub relative_delay_blocks: u16,
}

/// Restricted first-demo policy. Real keys are selected only inside Tenshu.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Topology {
    /// First demo supports 3-of-5 primary spending only.
    pub primary: Quorum,
    /// Explicit chosen recovery; no default is silently added.
    pub recovery: Recovery,
}
impl Topology {
    /// Validate the bounded first-demo topology, not a descriptor.
    pub fn valid(self) -> bool {
        self.primary
            == (Quorum {
                threshold: 3,
                key_count: 5,
            })
            && self.recovery.quorum.valid()
            && self.recovery.relative_delay_blocks > 0
    }
}

fn opaque_input(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Prepare one in-memory draft with bounded retry identity and explicit topology.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrepareInput {
    /// Grant-local retry key, never a path, URL, label or credential.
    #[schemars(length(min = 1, max = 64), regex(pattern = "^[A-Za-z0-9_-]+$"))]
    pub request_id: String,
    /// No key material, descriptors or network override can be supplied here.
    pub topology: Topology,
}
impl PrepareInput {
    /// Check bounded fields independently of host-side schema enforcement.
    pub fn valid(&self) -> bool {
        opaque_input(&self.request_id) && self.topology.valid()
    }
}

/// Public account-key assignments supplied by an explicitly paired setup client.
/// Device ownership and backups must still be verified by the user in Tenshu.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublicKeys {
    /// Five origin-bearing test-network account xpubs, without suffixes.
    #[schemars(length(min = 5, max = 5))]
    pub primary: Vec<String>,
    /// Account xpubs for the explicitly chosen recovery quorum.
    #[schemars(length(min = 1, max = 5))]
    pub recovery: Vec<String>,
}

/// Prepare an actual key-bound policy, never approve or install a wallet.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreationInput {
    /// Client-local immutable retry identity, not a path or credential.
    #[schemars(length(min = 1, max = 64), regex(pattern = "^[A-Za-z0-9_-]+$"))]
    pub request_id: String,
    /// The same bounded Regtest topology as the initial proposal experiment.
    pub topology: Topology,
    /// Public account keys only. Native parsing and compilation are mandatory.
    pub keys: PublicKeys,
}
impl CreationInput {
    /// Bound allocations/work independently of the host's JSON schema checks.
    pub fn valid(&self) -> bool {
        opaque_input(&self.request_id)
            && self.topology.valid()
            && self.keys.primary.len() == 5
            && self.keys.recovery.len() == usize::from(self.topology.recovery.quorum.key_count)
            && self
                .keys
                .primary
                .iter()
                .chain(&self.keys.recovery)
                .all(|key| {
                    !key.is_empty() && key.len() <= 256 && key.bytes().all(|b| b.is_ascii_graphic())
                })
    }
}

/// Read only a proposal returned by the current preparation grant.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposalInput {
    /// Session-scoped proposal identity, never a wallet or Cube identifier.
    #[schemars(length(min = 1, max = 64), regex(pattern = "^[A-Za-z0-9_-]+$"))]
    pub proposal_id: String,
}
impl ProposalInput {
    /// Reject unbounded or non-opaque references at the desktop boundary.
    pub fn valid(&self) -> bool {
        opaque_input(&self.proposal_id)
    }
}

/// Draft state currently supported by this service.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProposalState {
    /// No actual keys or descriptor have been selected/validated.
    NeedsKeys,
    /// The native wallet engine validated real keys; only native UI can approve.
    ReadyForNativeReview,
}

/// Minimized topology draft; never a wallet or validated descriptor.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    /// Random opaque reference, scoped to this store/grant only.
    pub proposal_id: String,
    /// Immutable draft revision. Native edits need a new reviewed revision.
    pub revision: u64,
    /// Only the disposable Bitcoin Regtest demonstration is supported.
    pub network: String,
    /// Requested first-demo script form, not a compiled descriptor.
    pub script_type: String,
    /// Human-requested paths, not claims about real signers or devices.
    pub topology: Topology,
    /// Native setup must collect actual keys before it can validate a descriptor.
    pub state: ProposalState,
    /// False until the actual wallet engine validates real bound keys.
    pub descriptor_validated: bool,
}

struct Draft {
    input: PrepareInput,
    proposal: Proposal,
    creation: Option<CreationInput>,
}
struct State {
    revoked: bool,
    draft: Option<Draft>,
}

/// One bounded draft per live preparation session. No persistence or callbacks.
///
/// A fresh store belongs to each preparation grant. Native code must recheck
/// that grant and session before offering any proposal-derived action. This
/// service never authorizes, installs, opens windows, compiles or invents keys.
pub struct Store {
    state: Mutex<State>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                revoked: false,
                draft: None,
            }),
        }
    }
}
impl Store {
    /// Irreversibly discard the session draft, including future queued work.
    pub fn revoke(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.revoked = true;
            state.draft = None;
        }
    }

    /// Return the existing exact retry or prepare the sole allowed draft.
    pub fn prepare(&self, input: PrepareInput) -> Result<Proposal, Error> {
        let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
        if state.revoked {
            return Err(Error::AccessDenied);
        }
        if !input.valid() {
            return Err(Error::InvalidInput);
        }
        if let Some(draft) = &state.draft {
            return if draft.creation.is_none() && draft.input == input {
                Ok(draft.proposal.clone())
            } else {
                Err(Error::ProposalConflict)
            };
        }
        let mut random = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let proposal = Proposal {
            proposal_id: random.iter().map(|b| format!("{b:02x}")).collect(),
            revision: 1,
            network: "regtest".into(),
            script_type: "p2wsh".into(),
            topology: input.topology,
            state: ProposalState::NeedsKeys,
            descriptor_validated: false,
        };
        state.draft = Some(Draft {
            input,
            proposal: proposal.clone(),
            creation: None,
        });
        Ok(proposal)
    }

    /// Only the trusted desktop supplies the actual descriptor validator.
    /// Validation and immutable insertion share the draft lock. No wire field
    /// can assert that compilation, device verification or approval succeeded.
    pub fn prepare_creation(
        &self,
        input: CreationInput,
        validate: impl FnOnce(&CreationInput) -> Result<(), Error>,
    ) -> Result<Proposal, Error> {
        let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
        if state.revoked {
            return Err(Error::AccessDenied);
        }
        if !input.valid() {
            return Err(Error::InvalidInput);
        }
        if let Some(draft) = &state.draft {
            return if draft.creation.as_ref() == Some(&input) {
                Ok(draft.proposal.clone())
            } else {
                Err(Error::ProposalConflict)
            };
        }
        validate(&input)?;
        let mut random = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let proposal = Proposal {
            proposal_id: random.iter().map(|b| format!("{b:02x}")).collect(),
            revision: 1,
            network: "regtest".into(),
            script_type: "p2wsh".into(),
            topology: input.topology,
            state: ProposalState::ReadyForNativeReview,
            descriptor_validated: true,
        };
        state.draft = Some(Draft {
            input: PrepareInput {
                request_id: input.request_id.clone(),
                topology: input.topology,
            },
            proposal: proposal.clone(),
            creation: Some(input),
        });
        Ok(proposal)
    }

    /// Native-only projection for the builder. Never returned through MCP.
    pub fn creation_input(&self, id: &str) -> Result<Option<CreationInput>, Error> {
        let state = self.state.lock().map_err(|_| Error::Unavailable)?;
        if state.revoked {
            return Err(Error::AccessDenied);
        }
        state
            .draft
            .as_ref()
            .filter(|draft| draft.proposal.proposal_id == id)
            .map(|draft| draft.creation.clone())
            .ok_or(Error::AccessDenied)
    }

    /// Native inbox projection. It contains only the same minimized public DTO.
    pub fn pending(&self) -> Result<Option<Proposal>, Error> {
        let state = self.state.lock().map_err(|_| Error::Unavailable)?;
        if state.revoked {
            return Err(Error::AccessDenied);
        }
        Ok(state.draft.as_ref().map(|draft| draft.proposal.clone()))
    }

    /// Trusted native callback after actual descriptor validation, or invalidation.
    /// No MCP request can invoke this method or supply its validation result.
    pub fn native_validation(&self, id: &str, validated: bool) -> Result<(), Error> {
        let mut state = self.state.lock().map_err(|_| Error::Unavailable)?;
        if state.revoked {
            return Err(Error::AccessDenied);
        }
        let draft = state
            .draft
            .as_mut()
            .filter(|d| d.proposal.proposal_id == id)
            .ok_or(Error::AccessDenied)?;
        if draft.proposal.descriptor_validated != validated {
            draft.proposal.revision = draft
                .proposal
                .revision
                .checked_add(1)
                .ok_or(Error::Unavailable)?;
        }
        draft.proposal.descriptor_validated = validated;
        draft.proposal.state = if validated {
            ProposalState::ReadyForNativeReview
        } else {
            ProposalState::NeedsKeys
        };
        Ok(())
    }

    /// Return only this grant's exact proposal, without existence hints.
    pub fn get(&self, input: &ProposalInput) -> Result<Proposal, Error> {
        let state = self.state.lock().map_err(|_| Error::Unavailable)?;
        if state.revoked {
            return Err(Error::AccessDenied);
        }
        if !input.valid() {
            return Err(Error::InvalidInput);
        }
        state
            .draft
            .as_ref()
            .filter(|draft| draft.proposal.proposal_id == input.proposal_id)
            .map(|draft| draft.proposal.clone())
            .ok_or(Error::AccessDenied)
    }
}
impl Reader for Store {
    fn read(
        &self,
        request: Request,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, Error>> + Send + '_>>
    {
        Box::pin(async move {
            match request {
                Request::PrepareProposal(input) => self.prepare(input).map(Response::Proposal),
                Request::Proposal(input) => self.get(&input).map(Response::Proposal),
                _ => Err(Error::AccessDenied),
            }
        })
    }
}
