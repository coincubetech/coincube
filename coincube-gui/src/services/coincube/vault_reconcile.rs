//! COIN-373 self-heal for Connect vault membership.
//!
//! A Vault's `connect_vault_members` rows are written one call at a time
//! after the vault shell is created (`installer::connect_vault`). A failure
//! partway through that fan-out leaves the vault short a keyholder row, and
//! that row is the *only* thing the Keychain app resolves a key's vault from
//! (`services/keychain/key/handlers/key.go` builds `vaultByKeyID` from it) —
//! so the phone reports "no vault" for a key that is genuinely a descriptor
//! signer, and Keychain signing dead-ends with "no Keychain signers required".
//!
//! The backend allows the repair explicitly: its W16 keyholder lock carries a
//! reconcile exception that permits a keyholder add on an `active` vault when
//! the key is already bound to that vault's cube, because the cube's keys are
//! exactly the set the sealed descriptor was built from (see
//! `coincube-api services/connect/vault/handlers/vault.go`, the
//! `isCubeKeyReconcile` branch). Attaching a key that is *not* on the cube
//! stays blocked — that would be a real quorum change against a sealed
//! descriptor, which no amount of member rows can effect.
//!
//! Two callers:
//! - [`reconcile_vault_members`] — the sign-time pass, which already holds the
//!   vault, cube keys and viewer id from its own fetches.
//! - [`reconcile_cube_vault_members`] — the cube-open pass, which fetches what
//!   it needs. Without it a broken vault stays broken on the phone until
//!   somebody starts a desktop sign, which is how the original incident went
//!   unnoticed long enough to strand funds.
//!
//! [`create_missing_vault`] handles the harder case one step earlier: a Cube
//! with **no vault row at all**, which the member-level reconcile cannot touch
//! because it has nothing to attach members to. That is the state left behind
//! when the post-install `create_connect_vault` never landed — see its own
//! docs for the two ways that happens. Both reconcile entry points route
//! through it, so the repair happens whether the user opens the Cube or
//! presses Sign.
//!
//! Best-effort throughout: every failure is logged and skipped. This can only
//! ever *add* routing metadata for a key the descriptor already commits to, so
//! there is no state it can corrupt by running too often.

use std::collections::HashSet;

use coincube_core::descriptors::CoincubeDescriptor;
use coincube_core::miniscript::bitcoin::bip32::Fingerprint;

use super::{
    classify_cube_key_ownership, AddVaultMemberRequest, CoincubeClient, ConnectVaultResponse,
    CreateConnectVaultRequest, CubeKeyOwnership, CubeKeyRaw, VaultMemberRole,
};

/// Every signer fingerprint the descriptor uses, across the primary path
/// and all recovery paths. Used by the COIN-373 reconcile to decide which
/// registered cube keys are actually part of this wallet.
pub(crate) fn descriptor_fingerprints(descriptor: &CoincubeDescriptor) -> HashSet<Fingerprint> {
    let policy = descriptor.policy();
    let mut fps: HashSet<Fingerprint> = policy
        .primary_path()
        .thresh_origins()
        .1
        .into_keys()
        .collect();
    for path in policy.recovery_paths().values() {
        fps.extend(path.thresh_origins().1.into_keys());
    }
    fps
}

/// Registered cube keys that this wallet's descriptor commits to but that the
/// vault carries no member row for — the exact set the fan-out failed to
/// attach. Split out so a caller can cheaply answer "is there anything to do
/// here?" before spending further round-trips (see
/// [`reconcile_cube_vault_members`]).
pub(crate) fn unattached_descriptor_keys<'a>(
    vault: &ConnectVaultResponse,
    cube_keys: &'a [CubeKeyRaw],
    descriptor: &CoincubeDescriptor,
) -> Vec<&'a CubeKeyRaw> {
    let descriptor_fps = descriptor_fingerprints(descriptor);
    let existing_key_ids: HashSet<u64> = vault.members.iter().filter_map(|m| m.key_id).collect();

    cube_keys
        .iter()
        .filter(|k| !existing_key_ids.contains(&k.id))
        .filter(|k| {
            k.fingerprint
                .parse::<Fingerprint>()
                .map(|fp| descriptor_fps.contains(&fp))
                .unwrap_or(false)
        })
        .collect()
}

/// Blocks per day, the rate the Vault Builder uses to turn a recovery path's
/// relative timelock into the `timelockDays` the backend vault carries.
const BLOCKS_PER_DAY: u32 = 144;

/// The Vault Builder marks its Safety Net path with `u16::MAX` rather than a
/// real timelock, and excludes it when deriving `timelockDays`. Mirrored here
/// so a vault created by the self-heal doesn't inherit a 455-year timelock.
const SAFETY_NET_SEQUENCE: u16 = u16::MAX;

/// `timelockDays` for a vault created from this descriptor, derived from its
/// longest real recovery path.
///
/// Mirrors the Vault Builder's own computation (`installer::step::descriptor::
/// editor::DefineDescriptor::apply`) — round up, floor at 1 — so a vault this
/// module creates is indistinguishable from one the builder would have made.
/// Rounding up matters: a server timelock that expired before the on-chain one
/// would advertise a recovery path that cannot yet be spent.
fn timelock_days_from_descriptor(descriptor: &CoincubeDescriptor) -> i32 {
    descriptor
        .policy()
        .recovery_paths()
        .keys()
        .copied()
        .filter(|seq| *seq != SAFETY_NET_SEQUENCE)
        .max()
        .map(|blocks| u32::from(blocks).div_ceil(BLOCKS_PER_DAY).max(1) as i32)
        .unwrap_or(1)
}

/// Build the keyholder member list for a vault covering `keys`, resolving each
/// owner to a `contact_id` (or `None` for the viewer's own keys).
///
/// Keys whose owner is neither the viewer nor an addressable contact are
/// dropped with a warning: sending one without a `contact_id` earns a 400
/// ("Key does not belong to the specified user") and would fail the whole
/// atomic create, taking the addressable members down with it.
fn keyholder_members(
    keys: &[&CubeKeyRaw],
    contacts: &[super::Contact],
    self_user_id: u64,
) -> Vec<AddVaultMemberRequest> {
    keys.iter()
        .filter_map(|key| {
            let contact_id = match classify_cube_key_ownership(key, contacts, self_user_id) {
                CubeKeyOwnership::SelfOwned { .. } => None,
                CubeKeyOwnership::ContactOwned { contact, .. } => Some(contact.id),
                CubeKeyOwnership::Unresolved { owner_id } => {
                    tracing::warn!(
                        target: "coincube_gui::signing",
                        key_id = key.id,
                        owner_user_id = owner_id,
                        "Reconcile: descriptor cube key owner is not a contact — leaving it out",
                    );
                    return None;
                }
            };
            Some(AddVaultMemberRequest {
                contact_id,
                key_id: Some(key.id),
                role: VaultMemberRole::Keyholder,
            })
        })
        .collect()
}

/// What [`create_missing_vault`] found, for a caller that has to explain the
/// outcome to someone.
///
/// The three states want three different things said, which is why this isn't
/// an `Option`: "there was nothing to create" points at how the Vault was
/// built, while "the create was refused" points at the server — and must *not*
/// send anyone off rebuilding a descriptor that was fine.
pub(crate) enum VaultCreation {
    /// The vault was created; its members are the descriptor's Keychain keys.
    Created(Box<ConnectVaultResponse>),
    /// No descriptor key is a registered Keychain key on this Cube, so there is
    /// no signer a vault row could route to. The ordinary local-only Vault —
    /// and also what a Vault looks like when its phone key was added as a plain
    /// xpub instead of from Keychain Keys.
    NotNeeded,
    /// A create was attempted and the server refused it. Carries a user-facing
    /// reason; what to do about it is the caller's call, not this module's.
    Refused(String),
}

/// Create the Connect vault a Vault Builder run should have created, for a Cube
/// whose descriptor commits to registered Keychain keys but which has no vault
/// row at all.
///
/// [`reconcile_vault_members`] repairs a vault that is *missing members*. This
/// covers the harder case one step earlier: **no vault**. A Cube reaches that
/// state whenever the post-install `create_connect_vault` didn't land — the
/// `POST` failed and was swallowed into a warning, or, more quietly, the
/// builder harvested no members at all because the phone key entered the
/// descriptor through "Paste xpub" (`KeySource::Manual`) rather than the
/// Keychain Keys card, which is the only source kind that records one.
///
/// Both roads end at the same place, and it is not a place the user can leave:
/// `KeychainSignModal::launch` opens with `GET .../vault`, the 404 aborts
/// classification, and every Keychain signer renders as an unidentified key
/// asking to be plugged in. If that key holds the only immediate spending
/// path, the coins are unspendable until the row exists.
///
/// Safe to run on every Cube open. It only ever creates a vault whose quorum is
/// drawn from keys the sealed descriptor already commits to *and* that are
/// already registered on this Cube, so it cannot invent a signer or change what
/// the wallet can spend — the same argument that lets the member-level
/// reconcile run unattended. A Cube with no Keychain keys in its descriptor
/// (the ordinary local-only Vault) is left alone.
///
/// Never returns an error: this is best-effort throughout, and every outcome —
/// a refusal included — is something the caller reports rather than propagates.
/// See [`VaultCreation`] for why the three cases stay distinct.
pub(crate) async fn create_missing_vault(
    client: &CoincubeClient,
    cube_server_id: u64,
    cube_keys: &[CubeKeyRaw],
    descriptor: &CoincubeDescriptor,
    self_user_id: u64,
) -> VaultCreation {
    let descriptor_fps = descriptor_fingerprints(descriptor);
    let candidates: Vec<&CubeKeyRaw> = cube_keys
        .iter()
        .filter(|k| {
            k.fingerprint
                .parse::<Fingerprint>()
                .map(|fp| descriptor_fps.contains(&fp))
                .unwrap_or(false)
        })
        .collect();
    if candidates.is_empty() {
        // A local-only Vault: no Keychain key in the descriptor, so there is no
        // signer for a vault row to route to. Nothing to heal.
        return VaultCreation::NotNeeded;
    }

    let contacts = client.get_contacts().await.unwrap_or_default();
    let members = keyholder_members(&candidates, &contacts, self_user_id);
    if members.is_empty() {
        tracing::warn!(
            target: "coincube_gui::signing",
            cube_server_id,
            candidates = candidates.len(),
            "Reconcile: Cube has no Connect vault and none of its descriptor's \
             Keychain keys resolve to an addressable owner — cannot create one",
        );
        return VaultCreation::Refused(
            "This Vault's Keychain keys belong to someone who isn't one of your contacts, \
             so they can't be asked to sign."
                .to_string(),
        );
    }

    let timelock_days = timelock_days_from_descriptor(descriptor);
    let fingerprint = crate::app::wallet::descriptor_id_fingerprint(descriptor).to_string();
    tracing::info!(
        target: "coincube_gui::signing",
        cube_server_id,
        members = members.len(),
        timelock_days,
        %fingerprint,
        "Reconcile: Cube has no Connect vault but its descriptor commits to \
         registered Keychain keys — creating the missing vault",
    );

    match client
        .create_connect_vault(
            cube_server_id,
            CreateConnectVaultRequest {
                timelock_days,
                fingerprint: Some(fingerprint),
                members,
            },
        )
        .await
    {
        Ok(vault) => {
            tracing::info!(
                target: "coincube_gui::signing",
                cube_server_id,
                vault_id = vault.id,
                members = vault.members.len(),
                "Reconcile: created the missing Connect vault",
            );
            VaultCreation::Created(Box::new(vault))
        }
        Err(e) if e.is_plan_estate_required() => {
            // Not transient, and not something a retry or a rebuild fixes:
            // say so plainly rather than leaving a bare 403 in the log.
            tracing::warn!(
                target: "coincube_gui::signing",
                cube_server_id,
                "Reconcile: cannot create the missing Connect vault — this \
                 account's plan does not include Connect vaults. Keychain \
                 signing stays unavailable for this Cube until the plan covers \
                 it: {}",
                e,
            );
            VaultCreation::Refused(
                "This account's plan doesn't include Connect vaults, so the Keychain keys in \
                 this Vault can't be asked to sign."
                    .to_string(),
            )
        }
        Err(e) => {
            tracing::warn!(
                target: "coincube_gui::signing",
                cube_server_id,
                "Reconcile: failed to create the missing Connect vault: {}",
                e,
            );
            VaultCreation::Refused(format!(
                "This Cube's Connect vault is missing and couldn't be created: {}",
                e
            ))
        }
    }
}

/// What [`reconcile_vault_members`] did: the vault to carry on with (re-read
/// when anything was attached), and how many member rows it wrote.
pub(crate) struct ReconciledVault {
    pub vault: ConnectVaultResponse,
    pub attached: usize,
}

/// Best-effort reconcile of vault membership before classification
/// (COIN-373). A Vault Builder run whose `add_vault_member` fan-out failed
/// can leave the backend vault with missing keyholder members, which makes
/// `build_keychain_index` blind to a descriptor signer and dead-ends the
/// Keychain sign flow ("no Keychain signers required") with no recovery.
///
/// Here we attach any cube key that (a) is a signer in this wallet's
/// descriptor and (b) isn't already a vault member, resolving the owner to a
/// `contact_id` for keyholder-contact keys (or `None` for the user's own
/// keys). Returns the vault re-fetched when at least one member was added.
/// Failures (including an owner we can't map to a keyholder contact) are
/// logged and skipped — classification then proceeds with whatever members
/// exist, exactly as before.
pub(crate) async fn reconcile_vault_members(
    client: &CoincubeClient,
    cube_server_id: u64,
    vault: ConnectVaultResponse,
    cube_keys: &[CubeKeyRaw],
    descriptor: &CoincubeDescriptor,
    self_user_id: u64,
) -> ReconciledVault {
    let candidates = unattached_descriptor_keys(&vault, cube_keys, descriptor);
    if candidates.is_empty() {
        return ReconciledVault { vault, attached: 0 };
    }

    // Needed to resolve a contact-owned key's `contact_id`. If this fails we
    // can still attach self-owned keys (which need no contact_id).
    let contacts = client.get_contacts().await.unwrap_or_default();

    let mut added = 0usize;
    for key in candidates {
        // Same identity-only classification the Vault Builder picker uses
        // (never on `ContactRole`); see [`classify_cube_key_ownership`].
        let contact_id = match classify_cube_key_ownership(key, &contacts, self_user_id) {
            CubeKeyOwnership::SelfOwned { .. } => None,
            CubeKeyOwnership::ContactOwned { contact, .. } => Some(contact.id),
            CubeKeyOwnership::Unresolved { owner_id } => {
                // Owner isn't a contact we can address — sending this without a
                // contact_id would 400 ("Key does not belong to the specified
                // user"), so skip and let classification surface it as Local.
                tracing::warn!(
                    target: "coincube_gui::signing",
                    key_id = key.id,
                    owner_user_id = owner_id,
                    "Reconcile: descriptor cube key owner is not a contact — skipping attach",
                );
                continue;
            }
        };
        match client
            .add_vault_member(
                cube_server_id,
                AddVaultMemberRequest {
                    contact_id,
                    key_id: Some(key.id),
                    role: VaultMemberRole::Keyholder,
                },
            )
            .await
        {
            Ok(_) => {
                added += 1;
                tracing::info!(
                    target: "coincube_gui::signing",
                    key_id = key.id,
                    contact_id = ?contact_id,
                    "Reconcile: attached missing keychain key to vault (COIN-373)",
                );
            }
            Err(e) => {
                // Best-effort: a failure here just means this signer stays
                // unattached and classification falls back to Local, the prior
                // behavior. Don't fail the whole sign flow.
                tracing::warn!(
                    target: "coincube_gui::signing",
                    key_id = key.id,
                    "Reconcile: failed to attach keychain key to vault: {}",
                    e,
                );
            }
        }
    }

    if added == 0 {
        return ReconciledVault { vault, attached: 0 };
    }
    // Re-fetch so the returned member list reflects the attachments.
    let vault = match client.get_connect_vault(cube_server_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "coincube_gui::signing",
                "Reconcile: re-fetch of vault after attaching members failed: {}",
                e,
            );
            vault
        }
    };
    ReconciledVault {
        vault,
        attached: added,
    }
}

/// Cube-open pass: fetch what the reconcile needs, then run it.
///
/// The sign-time caller already holds the vault, the cube keys and the viewer
/// id, so it calls [`reconcile_vault_members`] directly. This wrapper exists
/// for the launch-time trigger, where none of that is in hand.
///
/// Cost on a healthy Vault is two GETs (`vault` + `keys`); the viewer and
/// contact lookups only happen once there is actually a member row to write.
///
/// A Cube with no Connect vault (404) is not an error — plenty of Vaults are
/// local-only — but it is no longer an unconditional no-op either: when the
/// descriptor commits to registered Keychain keys, the vault itself is missing
/// and [`create_missing_vault`] creates it. A descriptor with no Keychain keys
/// still returns `0`.
///
/// Returns the number of member rows written, for logging — attached to an
/// existing vault, or carried by one this pass created.
pub async fn reconcile_cube_vault_members(
    client: &CoincubeClient,
    cube_server_id: u64,
    cube_uuid: &str,
    descriptor: &CoincubeDescriptor,
) -> usize {
    let vault = match client.get_connect_vault(cube_server_id).await {
        Ok(v) => Some(v),
        // No vault row at all. Usually the ordinary local-only Vault, but it is
        // also the state a Cube lands in when the post-install create never
        // landed — and there, the descriptor's Keychain keys are the tell.
        // `create_missing_vault` decides which of the two this is; it needs the
        // cube keys, so fall through rather than returning here.
        Err(e) if e.is_http_not_found() => None,
        Err(e) => {
            tracing::warn!(
                target: "coincube_gui::signing",
                cube_server_id,
                "Reconcile (cube open): vault fetch failed: {}",
                e,
            );
            return 0;
        }
    };
    let cube_keys = match client.get_cube_keys(cube_uuid).await {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(
                target: "coincube_gui::signing",
                cube_server_id,
                "Reconcile (cube open): cube-key fetch failed: {}",
                e,
            );
            return 0;
        }
    };
    // Nothing missing — the healthy path, and where this stops after two GETs.
    // A Cube with no vault always has work to consider, so it skips the check.
    if vault
        .as_ref()
        .is_some_and(|v| unattached_descriptor_keys(v, &cube_keys, descriptor).is_empty())
    {
        return 0;
    }
    let self_user_id: u64 = match client.get_user().await {
        Ok(u) => u.id.into(),
        Err(e) => {
            tracing::warn!(
                target: "coincube_gui::signing",
                cube_server_id,
                "Reconcile (cube open): viewer lookup failed: {}",
                e,
            );
            return 0;
        }
    };
    let Some(vault) = vault else {
        // Best-effort, like everything else here: a refusal is already logged
        // with its reason, and the Cube opens regardless.
        return match create_missing_vault(
            client,
            cube_server_id,
            &cube_keys,
            descriptor,
            self_user_id,
        )
        .await
        {
            VaultCreation::Created(v) => v.members.len(),
            VaultCreation::NotNeeded | VaultCreation::Refused(_) => 0,
        };
    };
    reconcile_vault_members(
        client,
        cube_server_id,
        vault,
        &cube_keys,
        descriptor,
        self_user_id,
    )
    .await
    .attached
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::{Method, MockServer};
    use serde_json::json;
    use std::str::FromStr;

    const RECOVERY_DESC: &str = "wsh(or_d(pk([f5acc2fd]tpubD6NzVbkrYhZ4YgUx2ZLNt2rLYAMTdYysCRzKoLu2BeSHKvzqPaBDvf17GeBPnExUVPkuBpx4kniP964e2MxyzzazcXLptxLXModSVCVEV1T/<0;1>/*),and_v(v:pkh([8a64f2a9]tpubD6NzVbkrYhZ4WmzFjvQrp7sDa4ECUxTi9oby8K4FZkd3XCBtEdKwUiQyYJaxiJo5y42gyDWEczrFpozEjeLxMPxjf2WtkfcbpUdfvNnozWF/<0;1>/*),older(10))))#d72le4dr";

    /// Same shape, with the Vault Builder's one-year inheritance sequence — the
    /// timelock a "Simple Inheritance" Vault actually ships with.
    const YEAR_RECOVERY_DESC: &str = "wsh(or_d(pk([f5acc2fd]tpubD6NzVbkrYhZ4YgUx2ZLNt2rLYAMTdYysCRzKoLu2BeSHKvzqPaBDvf17GeBPnExUVPkuBpx4kniP964e2MxyzzazcXLptxLXModSVCVEV1T/<0;1>/*),and_v(v:pkh([8a64f2a9]tpubD6NzVbkrYhZ4WmzFjvQrp7sDa4ECUxTi9oby8K4FZkd3XCBtEdKwUiQyYJaxiJo5y42gyDWEczrFpozEjeLxMPxjf2WtkfcbpUdfvNnozWF/<0;1>/*),older(52596))))";

    #[test]
    fn descriptor_fingerprints_covers_primary_and_recovery() {
        let desc = CoincubeDescriptor::from_str(RECOVERY_DESC).unwrap();
        let fps = descriptor_fingerprints(&desc);
        // The recovery signer must be included — dropping recovery-path
        // fingerprints would make the COIN-373 reconcile blind to exactly
        // the contact-owned recovery keys it exists to attach.
        assert!(fps.contains(&Fingerprint::from_str("f5acc2fd").unwrap()));
        assert!(fps.contains(&Fingerprint::from_str("8a64f2a9").unwrap()));
        assert_eq!(fps.len(), 2);
    }

    /// The descriptor's two signers, as the `/keys` endpoint would report them
    /// for this cube. `deadbeef` is not in the descriptor, so it must never be
    /// attached — the reconcile heals dropped rows, it does not invent members.
    fn cube_keys_body() -> serde_json::Value {
        json!({
            "success": true,
            "data": [
                {
                    "id": 1,
                    "name": "Primary",
                    "xpub": "xpub661...",
                    "fingerprint": "f5acc2fd",
                    "derivationPath": "m/48'/0'/0'/2'",
                    "network": "bitcoin",
                    "status": "active",
                    "ownerUserId": 7,
                    "ownerEmail": "me@example.com",
                    "isOwnKey": true,
                    "usedByVault": true
                },
                {
                    "id": 2,
                    "name": "Recovery",
                    "xpub": "xpub662...",
                    "fingerprint": "8a64f2a9",
                    "derivationPath": "m/48'/0'/1'/2'",
                    "network": "bitcoin",
                    "status": "active",
                    "ownerUserId": 7,
                    "ownerEmail": "me@example.com",
                    "isOwnKey": true,
                    "usedByVault": true
                },
                {
                    "id": 3,
                    "name": "Unrelated",
                    "xpub": "xpub663...",
                    "fingerprint": "deadbeef",
                    "derivationPath": "m/48'/0'/2'/2'",
                    "network": "bitcoin",
                    "status": "active",
                    "ownerUserId": 7,
                    "ownerEmail": "me@example.com",
                    "isOwnKey": true,
                    "usedByVault": false
                }
            ]
        })
    }

    fn vault_body(member_key_ids: &[u64]) -> serde_json::Value {
        let members: Vec<serde_json::Value> = member_key_ids
            .iter()
            .enumerate()
            .map(|(i, key_id)| {
                json!({
                    "id": i as u64 + 100,
                    "keyId": key_id,
                    "role": "keyholder",
                    "createdAt": "2026-04-18T00:00:00Z"
                })
            })
            .collect();
        json!({
            "success": true,
            "data": {
                "id": 5,
                "cubeId": 42,
                "timelockDays": 180,
                "timelockExpiresAt": "2026-10-15T00:00:00Z",
                "lastResetAt": "2026-04-18T00:00:00Z",
                "status": "active",
                "members": members,
                "createdAt": "2026-04-18T00:00:00Z",
                "updatedAt": "2026-04-18T00:00:00Z"
            }
        })
    }

    #[tokio::test]
    async fn cube_open_attaches_the_member_row_the_fan_out_dropped() {
        let server = MockServer::start();
        // Vault holds the primary signer only — key 2's row never landed.
        let vault = server.mock(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(vault_body(&[1]));
        });
        let keys = server.mock(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/connect/cubes/abc-uuid/keys");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(cube_keys_body());
        });
        let user = server.mock(|when, then| {
            when.method(Method::GET).path("/api/v1/user");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "id": 7, "email": "me@example.com" }));
        });
        let contacts = server.mock(|when, then| {
            when.method(Method::GET).path("/api/v1/connect/contacts");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "success": true, "data": [] }));
        });
        let add_member = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault/members")
                // Only the missing descriptor signer, and never the unrelated
                // cube key — attaching that would be a real quorum-change
                // attempt, which the backend blocks anyway.
                // `contactId` is skipped entirely for a self-owned key.
                .json_body(json!({ "keyId": 2, "role": "keyholder" }));
            then.status(201)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": true,
                    "data": {
                        "id": 101,
                        "keyId": 2,
                        "role": "keyholder",
                        "createdAt": "2026-04-18T00:00:00Z"
                    }
                }));
        });

        let client = CoincubeClient::for_test(server.base_url());
        let descriptor = CoincubeDescriptor::from_str(RECOVERY_DESC).unwrap();
        let added = reconcile_cube_vault_members(&client, 42, "abc-uuid", &descriptor).await;

        keys.assert();
        user.assert();
        let _ = contacts.hits();
        assert_eq!(add_member.hits(), 1, "the dropped row should be attached");
        // One GET to read the vault, one more to re-read it after the attach.
        assert_eq!(vault.hits(), 2);
        assert_eq!(added, 1);
    }

    #[tokio::test]
    async fn cube_open_is_two_gets_when_membership_is_already_complete() {
        let server = MockServer::start();
        let vault = server.mock(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(vault_body(&[1, 2]));
        });
        let keys = server.mock(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/connect/cubes/abc-uuid/keys");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(cube_keys_body());
        });
        let user = server.mock(|when, then| {
            when.method(Method::GET).path("/api/v1/user");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "id": 7, "email": "me@example.com" }));
        });
        let add_member = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault/members");
            then.status(201)
                .header("content-type", "application/json")
                .json_body(json!({ "success": true, "data": {} }));
        });

        let client = CoincubeClient::for_test(server.base_url());
        let descriptor = CoincubeDescriptor::from_str(RECOVERY_DESC).unwrap();
        let added = reconcile_cube_vault_members(&client, 42, "abc-uuid", &descriptor).await;

        assert_eq!(added, 0);
        assert_eq!(vault.hits(), 1);
        assert_eq!(keys.hits(), 1);
        // The healthy path must not spend a viewer lookup or write anything.
        assert_eq!(user.hits(), 0, "viewer lookup is only for the repair path");
        assert_eq!(add_member.hits(), 0);
    }

    /// A 404 with no `data`, the shape the API returns for a Cube that has no
    /// vault row.
    fn no_vault(server: &MockServer) -> httpmock::Mock {
        server.mock(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(404)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": false,
                    "error": { "code": "NOT_FOUND", "message": "No vault for cube" }
                }));
        })
    }

    fn user_and_contacts(server: &MockServer) -> (httpmock::Mock, httpmock::Mock) {
        let user = server.mock(|when, then| {
            when.method(Method::GET).path("/api/v1/user");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "id": 7, "email": "me@example.com" }));
        });
        let contacts = server.mock(|when, then| {
            when.method(Method::GET).path("/api/v1/connect/contacts");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "success": true, "data": [] }));
        });
        (user, contacts)
    }

    #[test]
    fn timelock_days_rounds_up_and_floors_at_one() {
        // `older(10)` is well under a day; a vault whose server timelock
        // rounded to 0 would advertise a recovery path immediately.
        let short = CoincubeDescriptor::from_str(RECOVERY_DESC).unwrap();
        assert_eq!(timelock_days_from_descriptor(&short), 1);

        // 52_596 blocks is the Vault Builder's own one-year inheritance
        // sequence: 365.25 days, which must round *up* so the server timelock
        // never expires before the on-chain one.
        let year = CoincubeDescriptor::from_str(YEAR_RECOVERY_DESC).unwrap();
        assert_eq!(timelock_days_from_descriptor(&year), 366);
    }

    #[tokio::test]
    async fn cube_open_creates_the_vault_the_installer_never_made() {
        let server = MockServer::start();
        let vault = no_vault(&server);
        let keys = server.mock(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/connect/cubes/abc-uuid/keys");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(cube_keys_body());
        });
        let (user, contacts) = user_and_contacts(&server);
        let descriptor = CoincubeDescriptor::from_str(RECOVERY_DESC).unwrap();
        // The vault's asserted identity must be the descriptor's own id
        // fingerprint — the same value the Vault Builder sends at create time
        // and `assert_vault_fingerprint` re-sends later. A vault created here
        // with a different one would show up in Keychain as a stranger.
        let fingerprint = crate::app::wallet::descriptor_id_fingerprint(&descriptor).to_string();
        let create = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault")
                // Both descriptor signers, and never the unrelated cube key:
                // the quorum is drawn from what the sealed descriptor already
                // commits to, so this can't change what the wallet can spend.
                .json_body(json!({
                    "timelockDays": 1,
                    "fingerprint": fingerprint,
                    "members": [
                        { "keyId": 1, "role": "keyholder" },
                        { "keyId": 2, "role": "keyholder" }
                    ]
                }));
            then.status(201)
                .header("content-type", "application/json")
                .json_body(vault_body(&[1, 2]));
        });

        let client = CoincubeClient::for_test(server.base_url());
        let added = reconcile_cube_vault_members(&client, 42, "abc-uuid", &descriptor).await;

        vault.assert();
        keys.assert();
        user.assert();
        let _ = contacts.hits();
        assert_eq!(create.hits(), 1, "the missing vault should be created");
        assert_eq!(added, 2);
    }

    #[tokio::test]
    async fn cube_open_leaves_a_genuinely_local_only_vault_alone() {
        let server = MockServer::start();
        let vault = no_vault(&server);
        // Cube keys exist, but none of them is a signer in this descriptor —
        // an ordinary local-only Vault on a Cube that happens to hold a phone
        // key for some other wallet. Creating a vault here would invent a
        // quorum out of keys the descriptor never committed to.
        let keys = server.mock(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/connect/cubes/abc-uuid/keys");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": true,
                    "data": [{
                        "id": 3,
                        "name": "Unrelated",
                        "xpub": "xpub663...",
                        "fingerprint": "deadbeef",
                        "derivationPath": "m/48'/0'/2'/2'",
                        "network": "bitcoin",
                        "status": "active",
                        "ownerUserId": 7,
                        "ownerEmail": "me@example.com",
                        "isOwnKey": true,
                        "usedByVault": false
                    }]
                }));
        });
        let create = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(201)
                .header("content-type", "application/json")
                .json_body(vault_body(&[]));
        });

        let client = CoincubeClient::for_test(server.base_url());
        let descriptor = CoincubeDescriptor::from_str(RECOVERY_DESC).unwrap();
        let added = reconcile_cube_vault_members(&client, 42, "abc-uuid", &descriptor).await;

        assert_eq!(added, 0);
        assert_eq!(create.hits(), 0, "must not invent a quorum");
        vault.assert();
        keys.assert();
    }

    #[tokio::test]
    async fn cube_open_survives_a_plan_gated_vault_create() {
        let server = MockServer::start();
        let vault = no_vault(&server);
        let keys = server.mock(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/connect/cubes/abc-uuid/keys");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(cube_keys_body());
        });
        let (_user, _contacts) = user_and_contacts(&server);
        let create = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(403)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": false,
                    "error": {
                        "code": "PLAN_ESTATE_REQUIRED",
                        "message": "Estate plan required"
                    }
                }));
        });

        let client = CoincubeClient::for_test(server.base_url());
        let descriptor = CoincubeDescriptor::from_str(RECOVERY_DESC).unwrap();
        // Best-effort throughout: a refused create is logged and skipped, never
        // propagated into the Cube-open path that called it.
        let added = reconcile_cube_vault_members(&client, 42, "abc-uuid", &descriptor).await;

        assert_eq!(added, 0);
        vault.assert();
        keys.assert();
        assert_eq!(create.hits(), 1);
    }
}
