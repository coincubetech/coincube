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
//! Best-effort throughout: every failure is logged and skipped. This can only
//! ever *add* routing metadata for a key the descriptor already commits to, so
//! there is no state it can corrupt by running too often.

use std::collections::HashSet;

use coincube_core::descriptors::CoincubeDescriptor;
use coincube_core::miniscript::bitcoin::bip32::Fingerprint;

use super::{
    classify_cube_key_ownership, AddVaultMemberRequest, CoincubeClient, ConnectVaultResponse,
    CubeKeyOwnership, CubeKeyRaw, VaultMemberRole,
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
/// A Cube with no Connect vault (404) is not an error here — plenty of Vaults
/// are local-only — so it returns `0` like any other no-op.
///
/// Returns the number of member rows attached, for logging.
pub async fn reconcile_cube_vault_members(
    client: &CoincubeClient,
    cube_server_id: u64,
    cube_uuid: &str,
    descriptor: &CoincubeDescriptor,
) -> usize {
    let vault = match client.get_connect_vault(cube_server_id).await {
        Ok(v) => v,
        Err(e) => {
            // 404 = no Connect vault for this Cube, the common local-only case.
            tracing::debug!(
                target: "coincube_gui::signing",
                cube_server_id,
                "Reconcile (cube open): no vault to reconcile: {}",
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
    if unattached_descriptor_keys(&vault, &cube_keys, descriptor).is_empty() {
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

    #[tokio::test]
    async fn cube_open_no_ops_when_the_cube_has_no_connect_vault() {
        let server = MockServer::start();
        let vault = server.mock(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(404)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": false,
                    "error": { "code": "NOT_FOUND", "message": "No vault for cube" }
                }));
        });
        let keys = server.mock(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/connect/cubes/abc-uuid/keys");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(cube_keys_body());
        });

        let client = CoincubeClient::for_test(server.base_url());
        let descriptor = CoincubeDescriptor::from_str(RECOVERY_DESC).unwrap();
        let added = reconcile_cube_vault_members(&client, 42, "abc-uuid", &descriptor).await;

        assert_eq!(added, 0);
        vault.assert();
        // A local-only Vault is not a broken Vault: stop at the 404.
        assert_eq!(keys.hits(), 0);
    }
}
