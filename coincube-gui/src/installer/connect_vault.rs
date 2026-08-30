//! Orchestrates creation of the backend `ConnectVault` and its
//! `ConnectVaultMember` rows after the local wallet install completes.
//!
//! **Shape of the call** (`plans/PLAN-atomic-vault-create.md`): the vault and
//! its members go up in one `POST .../vault`, which the server commits in a
//! single transaction. The vault either exists complete or does not exist at
//! all. What this replaced — create the shell, then `POST .../vault/members`
//! once per keyholder — committed the shell first with status `active`, so any
//! failure partway through stranded a vault with an incomplete member set; and
//! those rows are the only thing the Keychain app resolves a key's vault from,
//! so a keyholder whose row never landed reported `vaultId: null` and could not
//! sign from the phone. That is a shipped incident, not a hypothetical.
//!
//! The fan-out below survives as the fallback for a server that predates the
//! `members` field, detected from the create response rather than a version
//! check. Everything in the "Failure UX" and retry notes applies to that path.
//!
//! Design decisions (2026-04-18, `PLAN-cube-membership-desktop.md`):
//! - **Timelock days** = `ceil(max_recovery_blocks / 144)` (≈ blocks per
//!   day), clamped to a minimum of 1. Carried through in
//!   `Context::connect_vault_timelock_days`. Inherently approximate —
//!   surfaced as such in the Final step's outcome caption.
//! - **Member mapping** is restricted to `KeySource::KeychainKey`. HW,
//!   xpub, master-signer, token, and border-wallet keys are skipped
//!   (with a `tracing::info!` log). Rationale: only keychain keys have
//!   backend `keys.id` rows, and W9's "used in another vault" guard
//!   only matters for those.
//! - **Role** defaults to `Keyholder` for every member. Refinement into
//!   Beneficiary/Observer is a follow-up.
//! - **Failure UX**: the W9 409 (`KEY_ALREADY_USED_IN_VAULT`) and the
//!   I2 409 (`KEY_IS_RECOVERY_RECIPIENT`) name the offending key either way —
//!   from the error body on the atomic create, from the loop variable on the
//!   fan-out. On the atomic path there is nothing to roll back. On the fan-out
//!   path both roll back the just-created vault: W9 so the user can restart
//!   with a clean slate, I2 because a retry can't help (the sealed descriptor
//!   still holds the recovery key, so it must be rebuilt first). Other errors
//!   leave the partial vault in place and surface a retry-able warning.
//! - **Transient failures are retried in place** ([`MEMBER_ATTACH_ATTEMPTS`]).
//!   A vault left one member row short is not a cosmetic problem: that row is
//!   what the Keychain app resolves a key's vault from, so a dropped call
//!   makes the phone report "no vault" for a key the descriptor genuinely
//!   commits to. A few hundred milliseconds of retry inside a step the user is
//!   already waiting on is cheap next to that.
//! - **The partial state is recoverable, not terminal.** Anything that still
//!   fails here is healed by the COIN-373 reconcile
//!   ([`crate::services::coincube::vault_reconcile`]), which re-attaches
//!   missing rows at Cube open and before Keychain signing. That is why the
//!   generic branch does *not* roll the vault back: deleting it would remove
//!   the only thing the repair can attach to, and nothing outside this
//!   installer can create a Connect vault.

use crate::services::coincube::{
    AddVaultMemberRequest, CoincubeClient, ConnectVaultResponse, CreateConnectVaultRequest,
    RegisterCubeRequest, VaultMemberRole,
};

use super::context::ConnectVaultMemberPayload;

/// How many times a single `add_vault_member` call is attempted before the
/// fan-out gives up. Only failures [`CoincubeError::is_transient`] accepts are
/// retried — a 409 conflict is answered on the first response, never re-sent.
const MEMBER_ATTACH_ATTEMPTS: u32 = 3;

/// Backoff before retrying a member attach, multiplied by the attempt number
/// (400ms, then 800ms). Deliberately short: this runs inside the installer's
/// final step with the user watching a spinner, and the failures worth
/// retrying here are the ones that clear in well under a second.
const MEMBER_ATTACH_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(400);

/// Successful outcome of the vault-create fan-out.
#[derive(Debug, Clone)]
pub struct ConnectVaultOutcome {
    pub vault_id: u64,
    pub cube_server_id: u64,
    pub timelock_days: i32,
    pub members_added: usize,
    pub members_skipped_non_keychain: usize,
}

/// Error kinds surfaced to the Final step so it can pick the right UX.
#[derive(Debug, Clone)]
pub enum ConnectVaultError {
    /// The inputs don't support backend vault creation — no authenticated
    /// client, no cube id, or no members to attach. Treated as
    /// "silently skipped" by the Final step (user sees nothing).
    NotApplicable,
    /// W9 409 `KEY_ALREADY_USED_IN_VAULT`. The vault shell was rolled
    /// back before the error surfaced, so the user can restart. Carries
    /// the offending `key_id` for the dialog.
    KeyAlreadyUsedInVault { key_id: u64 },
    /// I2 409 `KEY_IS_RECOVERY_RECIPIENT`. The descriptor was sealed with
    /// a recovery key, which can never be a Vault signer. Like W9 the
    /// vault shell is rolled back first, but retrying is useless here —
    /// the descriptor itself must be rebuilt without the recovery key.
    /// Carries the offending `key_id` for the dialog.
    KeyIsRecoveryRecipient { key_id: u64 },
    /// Any other failure (network, backend 5xx, partial success). The
    /// caller gets a message suitable for display.
    Other(String),
}

impl std::fmt::Display for ConnectVaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotApplicable => write!(f, "Not applicable"),
            Self::KeyAlreadyUsedInVault { key_id } => {
                write!(
                    f,
                    "Key #{} is already used in another Vault. A key can \
                     only participate in one Vault. Remove it from this \
                     configuration and pick a different key.",
                    key_id
                )
            }
            Self::KeyIsRecoveryRecipient { key_id } => {
                write!(
                    f,
                    "Key #{} is a recovery key and can't be a Vault signer. \
                     Rebuild the Vault descriptor without the recovery key, \
                     then try again.",
                    key_id
                )
            }
            Self::Other(msg) => write!(f, "{}", msg),
        }
    }
}

/// Map a failed atomic create onto the error the Final step renders.
///
/// The server names the offending member inside the error body
/// (`plans/PLAN-atomic-vault-create.md` requirement 4), which is what lets one
/// call keep the per-key dialogs the fan-out drove from its loop variable. When
/// the body carries no `keyId` — a contact-only member, or an older server's
/// plain envelope — a single-member request can still attribute the rejection
/// locally; anything else falls back to the generic message rather than naming
/// the wrong key.
fn classify_create_error(
    e: crate::services::coincube::CoincubeError,
    members: &[ConnectVaultMemberPayload],
) -> ConnectVaultError {
    let key_id = e
        .rejected_member_key_id()
        .or_else(|| (members.len() == 1).then(|| members[0].key_id));
    match key_id {
        Some(key_id) if e.is_key_already_used_in_vault() => {
            // Nothing was created, so unlike the fan-out path there is no vault
            // to roll back — the user can restart the Vault Builder straight
            // away with a different key.
            ConnectVaultError::KeyAlreadyUsedInVault { key_id }
        }
        Some(key_id) if e.is_key_is_recovery_recipient() => {
            ConnectVaultError::KeyIsRecoveryRecipient { key_id }
        }
        _ => ConnectVaultError::Other(format!("Failed to create Connect vault: {}", e)),
    }
}

/// Run the full vault-create + member-attach flow. Safe to call when
/// Connect isn't authenticated — returns `NotApplicable` in that case.
///
/// Cube registration is idempotent on `(user_id, uuid)` server-side so
/// calling `register_cube` every time is safe — it just reaches into
/// the existing row.
pub async fn create_connect_vault(
    client: Option<CoincubeClient>,
    cube_uuid: Option<String>,
    cube_name: Option<String>,
    network: String,
    members: Vec<ConnectVaultMemberPayload>,
    timelock_days: Option<i32>,
    // This vault's descriptor fingerprint (8 lowercase hex), asserted at
    // creation so Keychain has an id to show for it from the first sync
    // (`plans/PLAN-vault-identity-unification.md` D3). `None` only when the
    // Final step had no descriptor in context; the desktop's backfill supplies
    // it on the next open.
    fingerprint: Option<String>,
) -> Result<ConnectVaultOutcome, ConnectVaultError> {
    let (Some(client), Some(cube_uuid), Some(cube_name)) = (client, cube_uuid, cube_name) else {
        return Err(ConnectVaultError::NotApplicable);
    };
    if members.is_empty() {
        // No keychain-sourced members means nothing for the backend to
        // track. Skip silently — the Final step translates this into a
        // no-op.
        return Err(ConnectVaultError::NotApplicable);
    }
    // `div_ceil` upstream already clamps to ≥ 1; defensive default here
    // in case we're called with `None` (shouldn't happen when members
    // is non-empty because a recovery path always exists in a valid
    // descriptor, but cheap insurance).
    let timelock_days = timelock_days.unwrap_or(1).max(1);

    // 1. Register cube (idempotent — returns the existing row if the
    //    uuid + owner already match).
    let cube = client
        .register_cube(RegisterCubeRequest {
            uuid: cube_uuid,
            name: cube_name,
            network,
            // This flow always accompanies a Vault (a Connect vault shell is
            // about to be created for this Cube), so report Vault presence
            // (PLAN-duress-vault-gate PR 3). Upgrade-only `Some(true)`.
            has_vault: Some(true),
        })
        .await
        .map_err(|e| ConnectVaultError::Other(format!("Failed to register cube: {}", e)))?;

    // 2. Create the vault and its members in one call. The server writes them
    //    in a single transaction, so the vault either exists complete or does
    //    not exist at all — nothing to roll back, and no window in which a
    //    keyholder row can go missing.
    let member_reqs: Vec<AddVaultMemberRequest> = members
        .iter()
        .map(|payload| AddVaultMemberRequest {
            contact_id: payload.contact_id,
            key_id: Some(payload.key_id),
            role: VaultMemberRole::Keyholder,
        })
        .collect();
    let vault: ConnectVaultResponse = client
        .create_connect_vault(
            cube.id,
            CreateConnectVaultRequest {
                timelock_days,
                fingerprint,
                members: member_reqs,
            },
        )
        .await
        .map_err(|e| classify_create_error(e, &members))?;

    // The atomic create returns the vault with its members preloaded. A server
    // that predates `members` ignores the field and hands back a member-less
    // vault instead — detect that by the response, not by a version check, and
    // fall back to the per-member fan-out below.
    if vault.members.len() >= members.len() {
        return Ok(ConnectVaultOutcome {
            vault_id: vault.id,
            cube_server_id: cube.id,
            timelock_days: vault.timelock_days,
            members_added: vault.members.len(),
            members_skipped_non_keychain: 0, // already filtered upstream
        });
    }
    tracing::info!(
        "Connect vault {} came back with {}/{} members — falling back to the \
         member fan-out for the missing rows",
        vault.id,
        vault.members.len(),
        members.len()
    );

    // Attach only what the response is actually missing. Usually that is
    // everything (a server predating `members` ignores the field and returns a
    // member-less vault), but it need not be: the server commits the members and
    // then re-reads the vault to preload them, and treats a failed re-read as a
    // successful create — so a complete vault can come back reporting no
    // members. Re-sending those would answer 409 DUPLICATE_RESOURCE per member
    // and turn a correct vault into a user-visible failure.
    let already_attached: std::collections::HashSet<u64> =
        vault.members.iter().filter_map(|m| m.key_id).collect();
    let members_added_at_create = members
        .iter()
        .filter(|p| already_attached.contains(&p.key_id))
        .count();

    // 3. Legacy fan-out: create-then-attach, one call per member. Reachable
    //    only against a server without atomic create. On the W9 or I2 409, roll
    //    back and bail; on a transient failure, retry the same member before
    //    giving up.
    let mut members_added = members_added_at_create;
    'members: for payload in &members {
        if already_attached.contains(&payload.key_id) {
            continue;
        }
        let mut attempt = 1u32;
        loop {
            let req = AddVaultMemberRequest {
                contact_id: payload.contact_id,
                key_id: Some(payload.key_id),
                role: VaultMemberRole::Keyholder,
            };
            match client.add_vault_member(cube.id, req).await {
                Ok(_) => {
                    members_added += 1;
                    continue 'members;
                }
                // The row exists, which is all the caller wanted. Reachable
                // from the retry below: an attempt whose write landed but whose
                // response was lost comes back here on the next try, and
                // failing the whole install over a member that IS attached
                // would be the retry causing the outage it exists to prevent.
                Err(e) if e.is_duplicate_member() => {
                    tracing::info!(
                        "add_vault_member (key {}) reports the member already exists — \
                         counting it as attached",
                        payload.key_id
                    );
                    members_added += 1;
                    continue 'members;
                }
                Err(e) if e.is_key_already_used_in_vault() => {
                    // Roll back the vault we just created so the user can
                    // restart the Vault Builder with a clean slate. The
                    // delete is best-effort — a failure to roll back just
                    // means the user will see a "vault already exists" on
                    // their next attempt and the backend's `delete_connect_vault`
                    // can be retried.
                    if let Err(rollback_err) = client.delete_connect_vault(cube.id).await {
                        tracing::warn!(
                            "W9 rollback failed to delete vault {}: {}",
                            vault.id,
                            rollback_err
                        );
                    }
                    return Err(ConnectVaultError::KeyAlreadyUsedInVault {
                        key_id: payload.key_id,
                    });
                }
                Err(e) if e.is_key_is_recovery_recipient() => {
                    // I2 backstop: the descriptor was sealed with a recovery
                    // key, which can never fan out into a signer. Unlike W9,
                    // retrying is hopeless — the sealed descriptor still
                    // contains the recovery key — so we roll the partial
                    // vault back (same best-effort delete as W9) and surface a
                    // distinct "rebuild the descriptor" error. PR 2 should make
                    // this unreachable from a current build, but stale desktops
                    // and future pickers keep it worth having.
                    if let Err(rollback_err) = client.delete_connect_vault(cube.id).await {
                        tracing::warn!(
                            "I2 rollback failed to delete vault {}: {}",
                            vault.id,
                            rollback_err
                        );
                    }
                    return Err(ConnectVaultError::KeyIsRecoveryRecipient {
                        key_id: payload.key_id,
                    });
                }
                Err(e) if e.is_transient() && attempt < MEMBER_ATTACH_ATTEMPTS => {
                    // A dropped connection or a 5xx here is the exact failure that
                    // strands a Vault one member row short — and that row is what
                    // the Keychain app resolves a key's vault from, so the phone
                    // then reports "no vault" for a key the descriptor genuinely
                    // commits to. Retrying the same request costs a few hundred
                    // milliseconds inside a step the user is already waiting on.
                    tracing::warn!(
                        "add_vault_member (key {}) attempt {}/{} failed, retrying: {}",
                        payload.key_id,
                        attempt,
                        MEMBER_ATTACH_ATTEMPTS,
                        e
                    );
                    tokio::time::sleep(MEMBER_ATTACH_RETRY_BACKOFF * attempt).await;
                    attempt += 1;
                }
                Err(e) => {
                    return Err(ConnectVaultError::Other(format!(
                        "Failed to add vault member (key {}): {}",
                        payload.key_id, e
                    )));
                }
            }
        }
    }

    Ok(ConnectVaultOutcome {
        vault_id: vault.id,
        cube_server_id: cube.id,
        timelock_days: vault.timelock_days,
        members_added,
        members_skipped_non_keychain: 0, // already filtered upstream
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::installer::descriptor::PathKind;
    use coincube_core::miniscript::bitcoin::bip32::Fingerprint;
    use httpmock::{Method, MockServer};
    use serde_json::json;
    use std::str::FromStr;

    fn sample_member(fp: &str, key_id: u64, contact_id: Option<u64>) -> ConnectVaultMemberPayload {
        ConnectVaultMemberPayload {
            fingerprint: Fingerprint::from_str(fp).expect("valid fp"),
            key_id,
            contact_id,
            path_kind: PathKind::Primary,
        }
    }

    #[tokio::test]
    async fn not_applicable_when_client_missing() {
        let err = create_connect_vault(
            None,
            Some("uuid".to_string()),
            Some("My Cube".to_string()),
            "mainnet".to_string(),
            vec![sample_member("deadbeef", 1, None)],
            Some(180),
            Some("8099ee80".to_string()),
        )
        .await
        .expect_err("should short-circuit");
        assert!(matches!(err, ConnectVaultError::NotApplicable));
    }

    #[tokio::test]
    async fn not_applicable_when_members_empty() {
        let server = MockServer::start();
        let client = CoincubeClient::for_test(server.base_url());
        let err = create_connect_vault(
            Some(client),
            Some("uuid".to_string()),
            Some("My Cube".to_string()),
            "mainnet".to_string(),
            vec![],
            Some(180),
            Some("8099ee80".to_string()),
        )
        .await
        .expect_err("should short-circuit");
        assert!(matches!(err, ConnectVaultError::NotApplicable));
    }

    #[tokio::test]
    async fn happy_path_creates_vault_and_members_in_one_call() {
        let server = MockServer::start();

        let register = server.mock(|when, then| {
            when.method(Method::POST).path("/api/v1/connect/cubes");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": true,
                    "data": {
                        "id": 42,
                        "uuid": "abc-uuid",
                        "name": "My Cube",
                        "network": "mainnet",
                        "lightningAddress": null,
                        "bolt12Offer": null,
                        "status": "active"
                    }
                }));
        });

        let create_vault = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault")
                // The vault's identity rides along with the create — pinned
                // here so a caller that stops threading it through fails the
                // route match rather than silently registering an
                // identity-less vault
                // (`plans/PLAN-vault-identity-unification.md` D3).
                // ...and so does the quorum: members ride along with the
                // create so the server can commit them in one transaction
                // (`plans/PLAN-atomic-vault-create.md`).
                .json_body(json!({
                    "timelockDays": 180,
                    "fingerprint": "8099ee80",
                    "members": [{ "keyId": 99, "role": "keyholder" }]
                }));
            then.status(201)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": true,
                    "data": {
                        "id": 5,
                        "cubeId": 42,
                        "timelockDays": 180,
                        "timelockExpiresAt": "2026-10-15T00:00:00Z",
                        "lastResetAt": "2026-04-18T00:00:00Z",
                        "status": "active",
                        "members": [{
                            "id": 7,
                            "keyId": 99,
                            "role": "keyholder",
                            "createdAt": "2026-04-18T00:00:00Z"
                        }],
                        "fingerprint": "8099ee80",
                        "createdAt": "2026-04-18T00:00:00Z",
                        "updatedAt": "2026-04-18T00:00:00Z"
                    }
                }));
        });

        // Must never be reached: the fan-out is the legacy fallback only.
        let add_member = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault/members");
            then.status(201)
                .header("content-type", "application/json")
                .json_body(json!({ "success": true, "data": {} }));
        });

        let client = CoincubeClient::for_test(server.base_url());
        let outcome = create_connect_vault(
            Some(client),
            Some("abc-uuid".to_string()),
            Some("My Cube".to_string()),
            "mainnet".to_string(),
            vec![sample_member("deadbeef", 99, None)],
            Some(180),
            Some("8099ee80".to_string()),
        )
        .await
        .expect("happy path");

        register.assert();
        create_vault.assert();
        assert_eq!(
            add_member.hits(),
            0,
            "the atomic create must not be followed by a fan-out"
        );
        assert_eq!(outcome.vault_id, 5);
        assert_eq!(outcome.cube_server_id, 42);
        assert_eq!(outcome.timelock_days, 180);
        assert_eq!(outcome.members_added, 1);
    }

    #[tokio::test]
    async fn fallback_w9_409_rolls_back_vault_and_surfaces_key_id() {
        let server = MockServer::start();

        let register = server.mock(|when, then| {
            when.method(Method::POST).path("/api/v1/connect/cubes");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": true,
                    "data": {
                        "id": 42,
                        "uuid": "abc-uuid",
                        "name": "My Cube",
                        "network": "mainnet",
                        "lightningAddress": null,
                        "bolt12Offer": null,
                        "status": "active"
                    }
                }));
        });

        let create_vault = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(201)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": true,
                    "data": {
                        "id": 5,
                        "cubeId": 42,
                        "timelockDays": 180,
                        "timelockExpiresAt": "2026-10-15T00:00:00Z",
                        "lastResetAt": "2026-04-18T00:00:00Z",
                        "status": "active",
                        "members": [],
                        "createdAt": "2026-04-18T00:00:00Z",
                        "updatedAt": "2026-04-18T00:00:00Z"
                    }
                }));
        });

        let add_member = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault/members");
            then.status(409)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": false,
                    "error": {
                        "code": "KEY_ALREADY_USED_IN_VAULT",
                        "message": "Key has already been used in another vault"
                    }
                }));
        });

        let rollback = server.mock(|when, then| {
            when.method(Method::DELETE)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": true,
                    "data": { "deleted": true }
                }));
        });

        let client = CoincubeClient::for_test(server.base_url());
        let err = create_connect_vault(
            Some(client),
            Some("abc-uuid".to_string()),
            Some("My Cube".to_string()),
            "mainnet".to_string(),
            vec![sample_member("deadbeef", 99, None)],
            Some(180),
            Some("8099ee80".to_string()),
        )
        .await
        .expect_err("expected W9 409 error");

        register.assert();
        create_vault.assert();
        add_member.assert();
        rollback.assert();
        assert!(
            matches!(err, ConnectVaultError::KeyAlreadyUsedInVault { key_id: 99 }),
            "expected W9 error with key_id=99, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn fallback_i2_409_recovery_recipient_rolls_back_vault_and_surfaces_key_id() {
        let server = MockServer::start();

        let register = server.mock(|when, then| {
            when.method(Method::POST).path("/api/v1/connect/cubes");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": true,
                    "data": {
                        "id": 42,
                        "uuid": "abc-uuid",
                        "name": "My Cube",
                        "network": "mainnet",
                        "lightningAddress": null,
                        "bolt12Offer": null,
                        "status": "active"
                    }
                }));
        });

        let create_vault = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(201)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": true,
                    "data": {
                        "id": 5,
                        "cubeId": 42,
                        "timelockDays": 180,
                        "timelockExpiresAt": "2026-10-15T00:00:00Z",
                        "lastResetAt": "2026-04-18T00:00:00Z",
                        "status": "active",
                        "members": [],
                        "createdAt": "2026-04-18T00:00:00Z",
                        "updatedAt": "2026-04-18T00:00:00Z"
                    }
                }));
        });

        // I2 guard: 409 with the recovery-recipient code.
        let add_member = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault/members");
            then.status(409)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": false,
                    "error": {
                        "code": "KEY_IS_RECOVERY_RECIPIENT",
                        "message": "This key is a recovery key and cannot be a Vault signer"
                    }
                }));
        });

        let rollback = server.mock(|when, then| {
            when.method(Method::DELETE)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": true,
                    "data": { "deleted": true }
                }));
        });

        let client = CoincubeClient::for_test(server.base_url());
        let err = create_connect_vault(
            Some(client),
            Some("abc-uuid".to_string()),
            Some("My Cube".to_string()),
            "mainnet".to_string(),
            vec![sample_member("deadbeef", 99, None)],
            Some(180),
            Some("8099ee80".to_string()),
        )
        .await
        .expect_err("expected I2 409 error");

        register.assert();
        create_vault.assert();
        add_member.assert();
        // The partial vault is rolled back, exactly like W9 — retrying can't
        // help, so we don't strand a vault the descriptor can never fan out.
        rollback.assert();
        assert!(
            matches!(
                err,
                ConnectVaultError::KeyIsRecoveryRecipient { key_id: 99 }
            ),
            "expected I2 error with key_id=99, got: {:?}",
            err
        );
    }

    /// Shared fixtures for the retry tests: cube registration + vault create
    /// both succeed, leaving the member fan-out as the only variable.
    fn register_and_create_mocks(server: &MockServer) -> (httpmock::Mock, httpmock::Mock) {
        let register = register_mock(server);
        let create_vault = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(201)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": true,
                    "data": {
                        "id": 5,
                        "cubeId": 42,
                        "timelockDays": 180,
                        "timelockExpiresAt": "2026-10-15T00:00:00Z",
                        "lastResetAt": "2026-04-18T00:00:00Z",
                        "status": "active",
                        "members": [],
                        "createdAt": "2026-04-18T00:00:00Z",
                        "updatedAt": "2026-04-18T00:00:00Z"
                    }
                }));
        });
        (register, create_vault)
    }

    /// Cube registration only, for the tests that answer the create themselves.
    fn register_mock(server: &MockServer) -> httpmock::Mock {
        server.mock(|when, then| {
            when.method(Method::POST).path("/api/v1/connect/cubes");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": true,
                    "data": {
                        "id": 42,
                        "uuid": "abc-uuid",
                        "name": "My Cube",
                        "network": "mainnet",
                        "lightningAddress": null,
                        "bolt12Offer": null,
                        "status": "active"
                    }
                }));
        })
    }

    #[tokio::test]
    async fn fallback_transient_member_failure_is_retried_before_giving_up() {
        let server = MockServer::start();
        let (register, create_vault) = register_and_create_mocks(&server);

        // A 5xx on every attempt: the fan-out should exhaust its budget rather
        // than strand the vault a member short on the first blip.
        let add_member = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault/members");
            then.status(503)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": false,
                    "error": { "code": "UNAVAILABLE", "message": "upstream unavailable" }
                }));
        });

        let client = CoincubeClient::for_test(server.base_url());
        let err = create_connect_vault(
            Some(client),
            Some("abc-uuid".to_string()),
            Some("My Cube".to_string()),
            "mainnet".to_string(),
            vec![sample_member("deadbeef", 99, None)],
            Some(180),
            Some("8099ee80".to_string()),
        )
        .await
        .expect_err("expected the exhausted-retry error");

        register.assert();
        create_vault.assert();
        assert_eq!(
            add_member.hits(),
            MEMBER_ATTACH_ATTEMPTS as usize,
            "a transient failure should be retried up to the attempt budget"
        );
        assert!(
            matches!(err, ConnectVaultError::Other(_)),
            "expected Other after exhausting retries, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn fallback_conflict_responses_are_not_retried() {
        let server = MockServer::start();
        let (register, create_vault) = register_and_create_mocks(&server);

        // W9 is a decision, not a blip. Re-sending it would only delay the
        // dialog the user needs.
        let add_member = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault/members");
            then.status(409)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": false,
                    "error": {
                        "code": "KEY_ALREADY_USED_IN_VAULT",
                        "message": "Key has already been used in another vault"
                    }
                }));
        });
        let rollback = server.mock(|when, then| {
            when.method(Method::DELETE)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "success": true, "data": { "deleted": true } }));
        });

        let client = CoincubeClient::for_test(server.base_url());
        let err = create_connect_vault(
            Some(client),
            Some("abc-uuid".to_string()),
            Some("My Cube".to_string()),
            "mainnet".to_string(),
            vec![sample_member("deadbeef", 99, None)],
            Some(180),
            Some("8099ee80".to_string()),
        )
        .await
        .expect_err("expected W9 409 error");

        register.assert();
        create_vault.assert();
        rollback.assert();
        assert_eq!(
            add_member.hits(),
            1,
            "a 409 conflict must be answered on the first response"
        );
        assert!(
            matches!(err, ConnectVaultError::KeyAlreadyUsedInVault { key_id: 99 }),
            "expected W9 error with key_id=99, got: {:?}",
            err
        );
    }

    /// Two members, so a key id in the outcome can only have come from the
    /// server's error body — not from the single-member local fallback.
    async fn atomic_create_rejection(code: &str, key_id: u64) -> ConnectVaultError {
        let server = MockServer::start();
        let register = register_mock(&server);

        let create_vault = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(409)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": false,
                    "error": {
                        "code": code,
                        "message": "rejected",
                        "memberIndex": 1,
                        "keyId": key_id
                    }
                }));
        });
        // Nothing was created, so nothing may be deleted.
        let rollback = server.mock(|when, then| {
            when.method(Method::DELETE)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "success": true, "data": { "deleted": true } }));
        });

        let client = CoincubeClient::for_test(server.base_url());
        let err = create_connect_vault(
            Some(client),
            Some("abc-uuid".to_string()),
            Some("My Cube".to_string()),
            "mainnet".to_string(),
            vec![
                sample_member("deadbeef", 11, None),
                sample_member("f5acc2fd", key_id, None),
            ],
            Some(180),
            Some("8099ee80".to_string()),
        )
        .await
        .expect_err("expected the create to be rejected");

        register.assert();
        create_vault.assert();
        assert_eq!(
            rollback.hits(),
            0,
            "an atomic create that failed left nothing to roll back"
        );
        err
    }

    #[tokio::test]
    async fn atomic_w9_names_the_key_from_the_error_body() {
        let err = atomic_create_rejection("KEY_ALREADY_USED_IN_VAULT", 77).await;
        assert!(
            matches!(err, ConnectVaultError::KeyAlreadyUsedInVault { key_id: 77 }),
            "expected W9 naming key 77, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn atomic_i2_names_the_key_from_the_error_body() {
        let err = atomic_create_rejection("KEY_IS_RECOVERY_RECIPIENT", 88).await;
        assert!(
            matches!(
                err,
                ConnectVaultError::KeyIsRecoveryRecipient { key_id: 88 }
            ),
            "expected I2 naming key 88, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn server_that_ignores_members_falls_back_to_the_fan_out() {
        let server = MockServer::start();
        let (register, create_vault) = register_and_create_mocks(&server);

        // `register_and_create_mocks` answers with `members: []` — exactly what
        // a server predating the `members` field returns, since it ignores the
        // field rather than rejecting it. The desktop must notice and attach
        // the members itself instead of reporting a complete vault.
        let add_member = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault/members")
                .json_body(json!({ "keyId": 99, "role": "keyholder" }));
            then.status(201)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": true,
                    "data": {
                        "id": 7,
                        "keyId": 99,
                        "role": "keyholder",
                        "createdAt": "2026-04-18T00:00:00Z"
                    }
                }));
        });

        let client = CoincubeClient::for_test(server.base_url());
        let outcome = create_connect_vault(
            Some(client),
            Some("abc-uuid".to_string()),
            Some("My Cube".to_string()),
            "mainnet".to_string(),
            vec![sample_member("deadbeef", 99, None)],
            Some(180),
            Some("8099ee80".to_string()),
        )
        .await
        .expect("the fallback should still produce a complete vault");

        register.assert();
        create_vault.assert();
        assert_eq!(add_member.hits(), 1, "the fan-out should have run");
        assert_eq!(outcome.members_added, 1);
    }

    #[tokio::test]
    async fn a_partial_create_response_only_attaches_the_missing_rows() {
        let server = MockServer::start();
        let register = register_mock(&server);

        // The server committed both members, then failed to re-read the vault
        // and answered with what it had — a complete vault reporting one member.
        // Re-sending the one it already has would 409 and fail the install.
        let create_vault = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(201)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": true,
                    "data": {
                        "id": 5,
                        "cubeId": 42,
                        "timelockDays": 180,
                        "timelockExpiresAt": "2026-10-15T00:00:00Z",
                        "lastResetAt": "2026-04-18T00:00:00Z",
                        "status": "active",
                        "members": [{
                            "id": 7,
                            "keyId": 11,
                            "role": "keyholder",
                            "createdAt": "2026-04-18T00:00:00Z"
                        }],
                        "createdAt": "2026-04-18T00:00:00Z",
                        "updatedAt": "2026-04-18T00:00:00Z"
                    }
                }));
        });

        // Pinned to key 22: a request for key 11 must never be sent.
        let add_member = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault/members")
                .json_body(json!({ "keyId": 22, "role": "keyholder" }));
            then.status(201)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": true,
                    "data": {
                        "id": 8,
                        "keyId": 22,
                        "role": "keyholder",
                        "createdAt": "2026-04-18T00:00:00Z"
                    }
                }));
        });
        let rollback = server.mock(|when, then| {
            when.method(Method::DELETE)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "success": true, "data": { "deleted": true } }));
        });

        let client = CoincubeClient::for_test(server.base_url());
        let outcome = create_connect_vault(
            Some(client),
            Some("abc-uuid".to_string()),
            Some("My Cube".to_string()),
            "mainnet".to_string(),
            vec![
                sample_member("deadbeef", 11, None),
                sample_member("f5acc2fd", 22, None),
            ],
            Some(180),
            Some("8099ee80".to_string()),
        )
        .await
        .expect("a vault missing one row should be completed, not failed");

        register.assert();
        create_vault.assert();
        assert_eq!(
            add_member.hits(),
            1,
            "only the missing row should be posted"
        );
        assert_eq!(
            rollback.hits(),
            0,
            "a correct vault must never be rolled back"
        );
        assert_eq!(outcome.members_added, 2, "both rows are on the vault");
    }

    #[tokio::test]
    async fn a_duplicate_member_response_counts_as_attached() {
        let server = MockServer::start();
        let (register, create_vault) = register_and_create_mocks(&server);

        // What a retry sees when the first attempt's write landed but its
        // response was lost. The row exists; failing here would make the retry
        // cause the failure it exists to prevent.
        let add_member = server.mock(|when, then| {
            when.method(Method::POST)
                .path("/api/v1/connect/cubes/42/vault/members");
            then.status(409)
                .header("content-type", "application/json")
                .json_body(json!({
                    "success": false,
                    "error": {
                        "code": "DUPLICATE_RESOURCE",
                        "message": "This member already exists on the vault"
                    }
                }));
        });
        let rollback = server.mock(|when, then| {
            when.method(Method::DELETE)
                .path("/api/v1/connect/cubes/42/vault");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "success": true, "data": { "deleted": true } }));
        });

        let client = CoincubeClient::for_test(server.base_url());
        let outcome = create_connect_vault(
            Some(client),
            Some("abc-uuid".to_string()),
            Some("My Cube".to_string()),
            "mainnet".to_string(),
            vec![sample_member("deadbeef", 99, None)],
            Some(180),
            Some("8099ee80".to_string()),
        )
        .await
        .expect("an already-attached member is not an install failure");

        register.assert();
        create_vault.assert();
        assert_eq!(add_member.hits(), 1, "a duplicate must not be retried");
        assert_eq!(rollback.hits(), 0);
        assert_eq!(outcome.members_added, 1);
    }
}
