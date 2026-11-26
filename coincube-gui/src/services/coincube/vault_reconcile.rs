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
    CubeKeyOwnership, CubeKeyRaw, PutSealedRolesRequest, SealedRoleUpload, VaultMemberRole,
};
use crate::services::connect::crypto::is_sealable_transport_pubkey;
use crate::services::connect::crypto::key_role::{
    recipient_key_hash, seal_key_role, KeyRole, KeyRoleBinding,
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

/// Whether a recovery path is the Safety Net: the maximum timelock, held only
/// by provider keys. Shared by the Vault settings view (which names the path)
/// and the sealed key role (which tells a phone its key sits there), so the
/// two cannot drift. The installer's own view (`installer/view/mod.rs`) has an
/// older copy of the test over its in-progress key list; it is not a source
/// for a role.
pub(crate) fn is_safety_net_path(
    sequence: u16,
    keys: &[Fingerprint],
    is_provider_key: impl Fn(&Fingerprint) -> bool,
) -> bool {
    sequence == u16::MAX && keys.iter().all(is_provider_key)
}

/// Each descriptor signer's role: Primary by membership of the primary path;
/// otherwise Safety Net or Recovery by the path it sits in
/// ([`is_safety_net_path`]).
///
/// A key sits in one path as Vaults are built today. If one is ever in two,
/// the first wins (primary, then recovery paths by timelock) and it is logged.
pub(crate) fn key_roles(
    descriptor: &CoincubeDescriptor,
    is_provider_key: impl Fn(&Fingerprint) -> bool,
) -> std::collections::HashMap<Fingerprint, KeyRole> {
    let policy = descriptor.policy();
    let mut roles = std::collections::HashMap::new();
    let mut place = |fingerprint: Fingerprint, role: KeyRole| {
        if let Some(first) = roles.get(&fingerprint) {
            if *first != role {
                tracing::warn!(
                    target: "coincube_gui::signing",
                    %fingerprint,
                    "Key role: a key sits in more than one path; keeping the first",
                );
            }
            return;
        }
        roles.insert(fingerprint, role);
    };
    for fingerprint in policy.primary_path().thresh_origins().1.into_keys() {
        place(fingerprint, KeyRole::Primary);
    }
    // `recovery_paths` is ordered by timelock.
    for (sequence, path) in policy.recovery_paths().iter() {
        let mut keys: Vec<Fingerprint> = path.thresh_origins().1.into_keys().collect();
        keys.sort();
        let role = if is_safety_net_path(*sequence, &keys, &is_provider_key) {
            KeyRole::SafetyNet
        } else {
            KeyRole::Recovery
        };
        for fingerprint in keys {
            place(fingerprint, role);
        }
    }
    roles
}

/// A phone `ResolveSigners` returned for a Vault: what a role is sealed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoleTarget {
    pub device_id: String,
    pub key_id: String,
    pub key_fingerprint: String,
    pub transport_pubkey: Vec<u8>,
}

/// The sealed roles this Vault's phones are missing: one per resolved phone
/// that has no seal yet, or whose seal was made to a key it no longer has.
///
/// Skipped, to be picked up by a later pass: a target whose ids are not
/// numbers, whose transport key cannot be sealed to, whose key is not a
/// keyholder member of this Vault, or whose fingerprint is not a signer of
/// this descriptor. Nothing here is fatal and nothing is guessed.
///
/// Which key id carries which fingerprint is as Connect reports it in the
/// target. A server that mislabels a target gets a seal for the wrong role,
/// which is no more than it could seal itself with the phone's public key:
/// one more reason the role is a label and gates nothing.
pub(crate) fn missing_sealed_roles(
    vault: &ConnectVaultResponse,
    roles: &std::collections::HashMap<Fingerprint, KeyRole>,
    targets: &[RoleTarget],
) -> Vec<SealedRoleUpload> {
    use base64::Engine as _;
    let mut out = Vec::new();
    for target in targets {
        let (Ok(device_id), Ok(key_id)) = (
            target.device_id.parse::<u64>(),
            target.key_id.parse::<u64>(),
        ) else {
            continue;
        };
        if !is_sealable_transport_pubkey(&target.transport_pubkey) {
            continue;
        }
        let Some(member) = vault
            .members
            .iter()
            .find(|m| m.key_id == Some(key_id) && m.role == VaultMemberRole::Keyholder)
        else {
            continue;
        };
        let Some(role) = target
            .key_fingerprint
            .parse::<Fingerprint>()
            .ok()
            .and_then(|fingerprint| roles.get(&fingerprint).copied())
        else {
            continue;
        };
        let hash = recipient_key_hash(&target.transport_pubkey);
        // Already sealed to this phone's current key: nothing to do.
        if member
            .sealed_roles
            .iter()
            .any(|s| s.device_id == device_id && s.recipient_key_hash == hash)
        {
            continue;
        }
        // One seal per (member, phone), whatever the resolver repeats.
        if out
            .iter()
            .any(|u: &SealedRoleUpload| u.member_id == member.id && u.device_id == device_id)
        {
            continue;
        }
        let binding = KeyRoleBinding {
            vault_id: vault.id,
            key_id,
            device_id,
        };
        match seal_key_role(&target.transport_pubkey, &binding, role) {
            Ok(blob) => out.push(SealedRoleUpload {
                member_id: member.id,
                device_id,
                sealed: base64::engine::general_purpose::STANDARD.encode(blob),
                recipient_key_hash: hash,
            }),
            Err(e) => tracing::warn!(
                target: "coincube_gui::signing",
                key_id,
                device_id,
                "Key role: could not seal: {}",
                e,
            ),
        }
    }
    out
}

/// Seals and stores the roles this Vault's phones are missing. `Some(n)`: the
/// pass ran to the end and stored `n` (zero when nothing was missing). `None`:
/// the store was refused or failed, and a later pass may try again.
///
/// Best-effort, like everything in this module: a server without the endpoint,
/// a viewer who is not the Vault's owner, or any other failure is logged and
/// costs nothing but a missing role tag on the phone until a later pass. It
/// must never fail Vault creation, the launch path or signing, so it returns
/// no error for a caller to act on.
///
/// What the server receives is ciphertext, ids and a hash of a public key.
/// That keeps the role out of Connect's storage and out of anything a passive
/// observer of Connect sees. It is not protection against a Connect that
/// actively substitutes a phone's transport key: the key sealed to is the one
/// Connect relays, unpinned, exactly as for signing payloads. And the role
/// stays a label on the phone: a seal proves nothing about who wrote it.
pub(crate) async fn seal_missing_key_roles(
    client: &CoincubeClient,
    cube_server_id: u64,
    vault: &ConnectVaultResponse,
    roles: &std::collections::HashMap<Fingerprint, KeyRole>,
    targets: &[RoleTarget],
) -> Option<usize> {
    let sealed_roles = missing_sealed_roles(vault, roles, targets);
    if sealed_roles.is_empty() {
        return Some(0);
    }
    let count = sealed_roles.len();
    match client
        .put_vault_sealed_roles(cube_server_id, PutSealedRolesRequest { sealed_roles })
        .await
    {
        Ok(()) => Some(count),
        Err(e) => {
            tracing::debug!(
                target: "coincube_gui::signing",
                cube_server_id,
                "Key role: sealed roles not stored (retried at the next pass): {}",
                e,
            );
            None
        }
    }
}

/// Cube-open pass: read the Vault as its owner sees it, ask `resolve` which
/// phones hold its keys (`ResolveSigners`), and seal what is missing.
///
/// `resolve` is the gRPC call, passed in so this stays a REST-only module and
/// tests can stand in for the resolver. `None` when the pass could not finish
/// (no Vault readable yet, the resolver failed, the store failed), so the
/// caller may try again at its next trigger; `Some(n)` when it did.
pub(crate) async fn seal_cube_key_roles<F, Fut>(
    client: &CoincubeClient,
    cube_server_id: u64,
    roles: &std::collections::HashMap<Fingerprint, KeyRole>,
    resolve: F,
) -> Option<usize>
where
    F: FnOnce(u64) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<RoleTarget>, String>>,
{
    if roles.is_empty() {
        return Some(0);
    }
    let vault = client.get_connect_vault(cube_server_id).await.ok()?;
    let targets = match resolve(vault.id).await {
        Ok(targets) => targets,
        Err(e) => {
            tracing::debug!(
                target: "coincube_gui::signing",
                cube_server_id,
                "Key role: signers not resolved (retried at the next pass): {}",
                e,
            );
            return None;
        }
    };
    seal_missing_key_roles(client, cube_server_id, &vault, roles, &targets).await
}

impl From<&crate::services::connect::grpc::connect_v1::SignerTarget> for RoleTarget {
    fn from(target: &crate::services::connect::grpc::connect_v1::SignerTarget) -> Self {
        Self {
            device_id: target.device_id.clone(),
            key_id: target.key_id.clone(),
            key_fingerprint: target.key_fingerprint.clone(),
            transport_pubkey: target.transport_pubkey.clone(),
        }
    }
}

/// The targets of a `ResolveSigners` reply, when the reply is for `network`.
pub(crate) fn role_targets_for_network(
    resolved: &crate::services::connect::grpc::connect_v1::ResolveSignersResponse,
    network: &str,
) -> Result<Vec<RoleTarget>, String> {
    if resolved.network != network {
        return Err("signers resolved for another network".to_string());
    }
    Ok(resolved.targets.iter().map(RoleTarget::from).collect())
}

/// `ResolveSigners(vault_id)` over the signing rail, as [`RoleTarget`]s. The
/// one network call of the cube-open pass that is not REST.
///
/// `network` is this Cube's chain as its Connect network id. A reply for
/// another network, or one that names none, is refused: the same
/// substituted-identity check signing makes before it seals anything.
pub(crate) async fn resolve_role_targets(
    grpc_url: &str,
    tokens: std::sync::Arc<
        tokio::sync::RwLock<crate::services::connect::client::auth::AccessTokenResponse>,
    >,
    desktop_device_id: String,
    vault_id: u64,
    network: &str,
) -> Result<Vec<RoleTarget>, String> {
    use crate::services::connect::grpc::{
        create_channel, interceptor::AuthInterceptor, session::GrpcSessionClient,
    };
    let channel = create_channel(grpc_url)
        .await
        .map_err(|e| format!("gRPC channel: {}", e))?;
    let access_token = tokens.read().await.access_token.clone();
    let mut client = GrpcSessionClient::new(
        channel,
        AuthInterceptor::with_device_id(&access_token, desktop_device_id),
    );
    let resolved = client
        .resolve_signers(vault_id.to_string())
        .await
        .map_err(|status| status.code().to_string())?;
    role_targets_for_network(&resolved, network)
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

/// Mobile V3 Phase 1, Desktop PR T2: a key's role, sealed to its phone.
#[cfg(test)]
mod key_role_tests {
    use super::*;
    use crate::services::connect::crypto::key_role::{open_key_role, KEY_ROLE_BLOB_LEN};
    use base64::Engine as _;
    use coincube_core::miniscript::bitcoin::secp256k1::{PublicKey, Secp256k1, SecretKey};
    use httpmock::prelude::HttpMockRequest;
    use httpmock::{Method, MockServer};
    use serde_json::json;
    use std::collections::HashMap;
    use std::str::FromStr;

    /// Primary `f5acc2fd`, one recovery path holding `8a64f2a9`.
    const RECOVERY_DESC: &str = "wsh(or_d(pk([f5acc2fd]tpubD6NzVbkrYhZ4YgUx2ZLNt2rLYAMTdYysCRzKoLu2BeSHKvzqPaBDvf17GeBPnExUVPkuBpx4kniP964e2MxyzzazcXLptxLXModSVCVEV1T/<0;1>/*),and_v(v:pkh([8a64f2a9]tpubD6NzVbkrYhZ4WmzFjvQrp7sDa4ECUxTi9oby8K4FZkd3XCBtEdKwUiQyYJaxiJo5y42gyDWEczrFpozEjeLxMPxjf2WtkfcbpUdfvNnozWF/<0;1>/*),older(10))))#d72le4dr";
    const PRIMARY: &str = "f5acc2fd";
    const RECOVERY: &str = "8a64f2a9";

    fn fp(s: &str) -> Fingerprint {
        Fingerprint::from_str(s).unwrap()
    }

    fn roles() -> HashMap<Fingerprint, KeyRole> {
        key_roles(
            &CoincubeDescriptor::from_str(RECOVERY_DESC).unwrap(),
            |_| false,
        )
    }

    /// A phone: its transport secret (which only the phone holds) and key.
    fn phone(seed: u8) -> (SecretKey, Vec<u8>) {
        let sk = SecretKey::from_slice(&[seed; 32]).unwrap();
        let pk = PublicKey::from_secret_key(&Secp256k1::new(), &sk)
            .serialize()
            .to_vec();
        (sk, pk)
    }

    fn target(device_id: u64, key_id: u64, fingerprint: &str, pubkey: &[u8]) -> RoleTarget {
        RoleTarget {
            device_id: device_id.to_string(),
            key_id: key_id.to_string(),
            key_fingerprint: fingerprint.to_string(),
            transport_pubkey: pubkey.to_vec(),
        }
    }

    /// The Vault as its owner reads it: member 100 holds key 1 (primary),
    /// member 101 key 2 (recovery). `sealed` lists `(member, device, hash)`.
    fn vault_json(sealed: &[(u64, u64, String)]) -> serde_json::Value {
        let member = |id: u64, key_id: u64| {
            let seals: Vec<serde_json::Value> = sealed
                .iter()
                .filter(|(member_id, _, _)| *member_id == id)
                .map(
                    |(_, device_id, hash)| json!({"deviceId": device_id, "recipientKeyHash": hash}),
                )
                .collect();
            let mut m = json!({
                "id": id, "keyId": key_id, "role": "keyholder",
                "createdAt": "2026-04-18T00:00:00Z"
            });
            if !seals.is_empty() {
                m["sealedRoles"] = json!(seals);
            }
            m
        };
        json!({
            "id": 5, "cubeId": 42, "timelockDays": 180,
            "timelockExpiresAt": "2026-10-15T00:00:00Z",
            "lastResetAt": "2026-04-18T00:00:00Z", "status": "active",
            "members": [member(100, 1), member(101, 2)],
            "createdAt": "2026-04-18T00:00:00Z", "updatedAt": "2026-04-18T00:00:00Z"
        })
    }

    fn vault(sealed: &[(u64, u64, String)]) -> ConnectVaultResponse {
        serde_json::from_value(vault_json(sealed)).unwrap()
    }

    fn open(upload: &SealedRoleUpload, secret: &SecretKey, key_id: u64) -> KeyRole {
        let blob = base64::engine::general_purpose::STANDARD
            .decode(&upload.sealed)
            .unwrap();
        assert_eq!(blob.len(), KEY_ROLE_BLOB_LEN);
        open_key_role(
            secret,
            &KeyRoleBinding {
                vault_id: 5,
                key_id,
                device_id: upload.device_id,
            },
            &blob,
        )
        .expect("the phone opens its own role")
    }

    #[test]
    fn roles_follow_the_path_a_key_sits_in() {
        let roles = roles();
        assert_eq!(roles.get(&fp(PRIMARY)), Some(&KeyRole::Primary));
        assert_eq!(roles.get(&fp(RECOVERY)), Some(&KeyRole::Recovery));
        assert_eq!(roles.len(), 2);
    }

    /// The Safety Net rule the Vault settings view names a path by: the
    /// maximum timelock, held only by provider keys.
    #[test]
    fn safety_net_is_the_maximum_timelock_held_only_by_provider_keys() {
        let a = fp("aaaaaaaa");
        let b = fp("bbbbbbbb");
        let only_a = |f: &Fingerprint| *f == a;
        assert!(is_safety_net_path(u16::MAX, &[a], only_a));
        assert!(
            !is_safety_net_path(u16::MAX, &[a, b], only_a),
            "a non-provider key"
        );
        assert!(
            !is_safety_net_path(u16::MAX - 1, &[a], only_a),
            "not the maximum"
        );
        assert!(!is_safety_net_path(10, &[a], only_a));
        assert!(!is_safety_net_path(u16::MAX, &[b], only_a));

        // A descriptor whose recovery path is that path gives its key the
        // Safety Net role; the same path held by an ordinary key is Recovery.
        let desc = CoincubeDescriptor::from_str(
            &RECOVERY_DESC
                .split('#')
                .next()
                .unwrap()
                .replace("older(10)", "older(65535)"),
        )
        .unwrap();
        let as_provider = key_roles(&desc, |f| *f == fp(RECOVERY));
        assert_eq!(as_provider.get(&fp(RECOVERY)), Some(&KeyRole::SafetyNet));
        assert_eq!(as_provider.get(&fp(PRIMARY)), Some(&KeyRole::Primary));
        let ordinary = key_roles(&desc, |_| false);
        assert_eq!(ordinary.get(&fp(RECOVERY)), Some(&KeyRole::Recovery));
    }

    #[test]
    fn one_seal_per_resolved_phone_and_each_opens_to_its_keys_role() {
        let (primary_sk, primary_pk) = phone(0x41);
        let (recovery_sk, recovery_pk) = phone(0x42);
        let uploads = missing_sealed_roles(
            &vault(&[]),
            &roles(),
            &[
                target(34, 1, PRIMARY, &primary_pk),
                target(35, 2, RECOVERY, &recovery_pk),
            ],
        );
        assert_eq!(uploads.len(), 2);
        assert_eq!((uploads[0].member_id, uploads[0].device_id), (100, 34));
        assert_eq!((uploads[1].member_id, uploads[1].device_id), (101, 35));
        assert_eq!(open(&uploads[0], &primary_sk, 1), KeyRole::Primary);
        assert_eq!(open(&uploads[1], &recovery_sk, 2), KeyRole::Recovery);
        assert_eq!(
            uploads[0].recipient_key_hash,
            recipient_key_hash(&primary_pk)
        );
        // Every blob is the same size, whatever the role.
        assert_eq!(uploads[0].sealed.len(), uploads[1].sealed.len());
        assert_eq!(uploads[0].sealed.len(), 104);
    }

    #[test]
    fn only_phones_with_no_seal_or_a_changed_key_are_sealed_to() {
        let (_, primary_pk) = phone(0x41);
        let (recovery_sk, recovery_pk) = phone(0x42);
        let targets = [
            target(34, 1, PRIMARY, &primary_pk),
            target(35, 2, RECOVERY, &recovery_pk),
        ];
        // Both already sealed to their current keys: quiet.
        let settled = vault(&[
            (100, 34, recipient_key_hash(&primary_pk)),
            (101, 35, recipient_key_hash(&recovery_pk)),
        ]);
        assert!(missing_sealed_roles(&settled, &roles(), &targets).is_empty());

        // The recovery phone re-registered: its seal was made to an old key.
        let (_, old_pk) = phone(0x43);
        let changed = vault(&[
            (100, 34, recipient_key_hash(&primary_pk)),
            (101, 35, recipient_key_hash(&old_pk)),
        ]);
        let uploads = missing_sealed_roles(&changed, &roles(), &targets);
        assert_eq!(uploads.len(), 1);
        assert_eq!((uploads[0].member_id, uploads[0].device_id), (101, 35));
        assert_eq!(open(&uploads[0], &recovery_sk, 2), KeyRole::Recovery);

        // A second phone for a key that has a seal on another phone.
        let (second_sk, second_pk) = phone(0x44);
        let uploads =
            missing_sealed_roles(&settled, &roles(), &[target(36, 2, RECOVERY, &second_pk)]);
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].device_id, 36);
        assert_eq!(open(&uploads[0], &second_sk, 2), KeyRole::Recovery);
    }

    #[test]
    fn what_cannot_be_sealed_is_skipped_not_guessed() {
        let (_, pk) = phone(0x41);
        let skipped = [
            // No registered transport key, or one that is not a sealable key.
            target(34, 1, PRIMARY, &[]),
            target(34, 1, PRIMARY, &[0u8; 33]),
            target(34, 1, PRIMARY, &pk[..32]),
            // Ids that are not numbers.
            RoleTarget {
                device_id: "phone".into(),
                ..target(34, 1, PRIMARY, &pk)
            },
            RoleTarget {
                key_id: String::new(),
                ..target(34, 1, PRIMARY, &pk)
            },
            // A key that is not a member of this Vault.
            target(34, 9, PRIMARY, &pk),
            // A fingerprint that is not a signer of this descriptor.
            target(34, 1, "deadbeef", &pk),
            target(34, 1, "not-hex", &pk),
        ];
        for t in &skipped {
            assert!(
                missing_sealed_roles(&vault(&[]), &roles(), std::slice::from_ref(t)).is_empty(),
                "{:?}",
                t
            );
        }
        // An observer row for the key is not a keyholder: no role.
        let mut observer = vault(&[]);
        observer.members[0].role = VaultMemberRole::Observer;
        assert!(
            missing_sealed_roles(&observer, &roles(), &[target(34, 1, PRIMARY, &pk)]).is_empty()
        );
        // The resolver repeating a phone yields one seal.
        let twice = [target(34, 1, PRIMARY, &pk), target(34, 1, PRIMARY, &pk)];
        assert_eq!(missing_sealed_roles(&vault(&[]), &roles(), &twice).len(), 1);
    }

    #[test]
    fn the_upload_names_ids_ciphertext_and_a_hash_and_nothing_else() {
        let (_, pk) = phone(0x41);
        let uploads = missing_sealed_roles(&vault(&[]), &roles(), &[target(34, 1, PRIMARY, &pk)]);
        let body = serde_json::to_value(PutSealedRolesRequest {
            sealed_roles: uploads,
        })
        .unwrap();
        let entry = body["sealedRoles"][0].as_object().unwrap();
        let mut keys: Vec<&str> = entry.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["deviceId", "memberId", "recipientKeyHash", "sealed"]);
        assert_eq!(body.as_object().unwrap().len(), 1);
    }

    /// A request body with a role in the clear, or blobs of differing size,
    /// does not match this mock, so the test's `assert_hits` fails.
    fn carries_only_sealed_roles(req: &HttpMockRequest) -> bool {
        let Some(body) = req.body.as_ref() else {
            return false;
        };
        let text = String::from_utf8_lossy(body).to_lowercase();
        for word in ["primary", "recovery", "safety", "path", "role\"", "kind"] {
            // `sealedRoles` is the one place "role" may appear.
            if text.replace("sealedroles", "").contains(word) {
                return false;
            }
        }
        let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) else {
            return false;
        };
        let Some(entries) = json["sealedRoles"].as_array() else {
            return false;
        };
        !entries.is_empty()
            && entries.iter().all(|entry| {
                entry["sealed"]
                    .as_str()
                    .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
                    .is_some_and(|blob| blob.len() == KEY_ROLE_BLOB_LEN)
                    && entry["recipientKeyHash"]
                        .as_str()
                        .is_some_and(|h| h.len() == 64)
            })
    }

    fn client(server: &MockServer) -> CoincubeClient {
        let mut client = CoincubeClient::for_test(server.base_url());
        client.set_token("role-jwt");
        client
    }

    async fn resolved(targets: Vec<RoleTarget>) -> Result<Vec<RoleTarget>, String> {
        Ok(targets)
    }

    /// After a Vault is registered it has no seals: the pass that follows
    /// stores one per resolved phone, and nothing in the request says which
    /// role is which.
    #[tokio::test]
    async fn a_new_vault_gets_one_seal_per_resolved_phone_and_no_role_in_the_clear() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(200)
                .json_body(json!({"success": true, "data": vault_json(&[])}));
        });
        let put = server.mock(|when, then| {
            when.method(Method::PUT)
                .path("/api/v1/connect/cubes/42/vault/sealed-roles")
                .header("authorization", "Bearer role-jwt")
                .matches(carries_only_sealed_roles);
            then.status(200)
                .json_body(json!({"success": true, "data": {"stored": []}}));
        });
        let (_, primary_pk) = phone(0x41);
        let (_, recovery_pk) = phone(0x42);
        let stored = seal_cube_key_roles(&client(&server), 42, &roles(), |vault_id| {
            assert_eq!(vault_id, 5);
            resolved(vec![
                target(34, 1, PRIMARY, &primary_pk),
                target(35, 2, RECOVERY, &recovery_pk),
            ])
        })
        .await;
        assert_eq!(stored, Some(2));
        put.assert_hits(1);
    }

    #[tokio::test]
    async fn a_settled_vault_sends_nothing() {
        let server = MockServer::start();
        let (_, primary_pk) = phone(0x41);
        let sealed = [(100, 34, recipient_key_hash(&primary_pk))];
        server.mock(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(200)
                .json_body(json!({"success": true, "data": vault_json(&sealed)}));
        });
        let put = server.mock(|when, then| {
            when.method(Method::PUT)
                .path("/api/v1/connect/cubes/42/vault/sealed-roles");
            then.status(200);
        });
        // Key 2's owner has no phone registered: `ResolveSigners` lists it as
        // unresolved, so it is not among the targets.
        let stored = seal_cube_key_roles(&client(&server), 42, &roles(), |_| {
            resolved(vec![target(34, 1, PRIMARY, &primary_pk)])
        })
        .await;
        assert_eq!(
            stored,
            Some(0),
            "settled: the pass ran and had nothing to do"
        );
        put.assert_hits(0);
    }

    /// An older server, a viewer who does not own the Vault, a Cube with no
    /// Connect vault, an unreachable rail: each is a quiet `None` ("could not
    /// finish, may try again"), never an error for the caller to act on.
    #[tokio::test]
    async fn every_failure_is_quiet_and_may_be_tried_again() {
        let (_, pk) = phone(0x41);
        let targets = vec![target(34, 1, PRIMARY, &pk)];

        for put_status in [404, 405, 400, 423, 500] {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(Method::GET)
                    .path("/api/v1/connect/cubes/42/vault");
                then.status(200)
                    .json_body(json!({"success": true, "data": vault_json(&[])}));
            });
            let put = server.mock(|when, then| {
                when.method(Method::PUT)
                    .path("/api/v1/connect/cubes/42/vault/sealed-roles");
                then.status(put_status).body("{}");
            });
            let t = targets.clone();
            let stored = seal_cube_key_roles(&client(&server), 42, &roles(), |_| resolved(t)).await;
            assert_eq!(stored, None, "PUT {}", put_status);
            put.assert_hits(1);
        }

        // No Connect vault for this Cube: the resolver is never asked.
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(404).body("{}");
        });
        let stored = seal_cube_key_roles(&client(&server), 42, &roles(), |_| async {
            panic!("no vault, so nothing to resolve")
        })
        .await;
        assert_eq!(stored, None);

        // The rail is down: nothing is sent.
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(Method::GET)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(200)
                .json_body(json!({"success": true, "data": vault_json(&[])}));
        });
        let put = server.mock(|when, then| {
            when.method(Method::PUT)
                .path("/api/v1/connect/cubes/42/vault/sealed-roles");
            then.status(200);
        });
        let stored = seal_cube_key_roles(&client(&server), 42, &roles(), |_| async {
            Err("unavailable".to_string())
        })
        .await;
        assert_eq!(stored, None);
        put.assert_hits(0);

        // A Vault with no descriptor signers to name: no request at all.
        let stored = seal_cube_key_roles(&client(&server), 42, &HashMap::new(), |_| async {
            panic!("nothing to seal, so nothing to resolve")
        })
        .await;
        assert_eq!(stored, Some(0));
    }

    /// A reply for another network, or naming none, yields no targets: the
    /// check signing makes before it seals anything.
    #[test]
    fn signers_resolved_for_another_network_are_not_sealed_to() {
        use crate::services::connect::grpc::connect_v1::{ResolveSignersResponse, SignerTarget};
        let (_, pk) = phone(0x41);
        let reply = |network: &str| ResolveSignersResponse {
            targets: vec![SignerTarget {
                device_id: "34".into(),
                key_fingerprint: PRIMARY.into(),
                key_id: "1".into(),
                transport_pubkey: pk.clone(),
                capabilities: Vec::new(),
            }],
            unresolved: Vec::new(),
            network: network.into(),
        };
        let ok = role_targets_for_network(&reply("mainnet"), "mainnet").unwrap();
        assert_eq!(ok, vec![target(34, 1, PRIMARY, &pk)]);
        assert!(role_targets_for_network(&reply("bitcoin-blake2b"), "mainnet").is_err());
        assert!(role_targets_for_network(&reply(""), "mainnet").is_err());
    }

    /// A server that predates sealed roles sends no `sealedRoles` on members.
    #[test]
    fn a_vault_response_without_sealed_roles_still_parses() {
        let v = vault(&[]);
        assert!(v.members.iter().all(|m| m.sealed_roles.is_empty()));
        let v = vault(&[(100, 34, "ab".repeat(32))]);
        assert_eq!(
            v.members[0].sealed_roles,
            vec![crate::services::coincube::SealedRoleRef {
                device_id: 34,
                recipient_key_hash: "ab".repeat(32),
            }]
        );
    }
}
