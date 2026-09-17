# BTCB2 signing identity: chain-bound LAN pairing (v3) and the desktop's Rail 1 obligations

Status: **frozen interface contract, v1 — documentation only; no code, proto or
protocol change in this PR.** This is the desktop half of the cross-repository
contract whose canonical text is `keychain-app/docs/btcb2-signing-identity-contract.md`
(parent record [keychain-app#145](https://github.com/coincubetech/keychain-app/issues/145),
tracker [coincube-api#276](https://github.com/coincubetech/coincube-api/issues/276)).
Read that file for the identity model, the full refusal matrix and the open product
questions; this file quotes what a desktop implementer needs and must not drift from
it. Verified against `coincube` `964e6ac1d6dd04f60a4cc56999c3b3c83fa676a9`.

## 1. Why the desktop is involved

A BTCB2 Vault is the same descriptor, xpubs and fingerprints as its Bitcoin twin
(`docs/BTCB2_CHAIN_BINDING.md` explains why the daemon needed `ChainId`). The two
Keychain rails inherit the ambiguity:

- **Rail 1 (Connect)** — `keychain_sign.rs` sends `vault_id`, `descriptor_id` and
  `SignerTarget{device_id, key_id, key_fingerprint}` (`coincube-gui/src/app/state/vault/keychain_sign.rs:1016-1073`),
  never the chain, although it has it: `Wallet.chain: ChainId` (`coincube-gui/src/app/wallet.rs:141`).
- **Rail 2 (LAN)** — the v2 `PairingOffer` carries `key`, `dh`, `wfp`, `psk`
  (`coincube-gui/src/phone_signer/pairing.rs:65-105`), no chain. The phone picks a
  local record by xpub alone and echoes its backend id in `SignerBinding.key_id`,
  which `sign_tx` copies into every LAN session's target (`phone_signer/mod.rs:293-298`).
  The pairing store is datadir-global (`pairing_store.rs:183-190`), one row per
  phone cert (`pairing_transaction.rs:99`), scoped to a Vault by
  `vault_fingerprint = sha256(descriptor)[..4]` — equal for both twins — so a phone
  paired for a Bitcoin Vault is dialled for its BTCB2 twin.

`ReplayProtection` for Keychain keys stays `Legacy` (`app/state/vault/signers.rs:36-40`)
until Lane B3.2; this contract supplies the version handshake that lets it flip.

## 2. Chain identity, as the desktop states it

Always `Wallet.chain.api_str()` (`coincube_core::chain::ChainId::api_str`, one of
`mainnet | testnet | testnet4 | signet | regtest | bitcoin-blake2b | bitcoin-blake2b-testnet4`).
Never `bitcoin::Network`, never derived from the descriptor. An unknown string from
any peer is a refusal, never mapped to Bitcoin (`ChainId::from_api_str` already
returns `None`).

Capability strings (`RegisterDeviceRequest.capabilities`, `PairingComplete.capabilities`):
`chain-identity-v1` = the peer implements this contract; `btcb2-unified-v1` = the
peer can sign `ALL|UNIFIED` (B3.2). A Keychain without `chain-identity-v1` is a
**pre-identity phone**: the desktop must not create or present a BTCB2 session to it.

## 3. Rail 1 — desktop obligations (`keychain_sign.rs`, `coincube-gui/src/services/connect/grpc/`)

Proto additions land in `coincube-api` first and reach this repo via `make sync-proto`
(`grpc/connect.proto` is a vendored copy):
`CreateSigningSessionRequest.network = 13`, `SigningSession.network = 21`,
`SubmitPartialSignatureRequest.network = 5`, `SignerTarget.capabilities = 5`,
`ResolveSignersResponse.network = 3`, `UnresolvedSigner.reason = "signer_app_outdated"`.

| when | rule | refusal |
|---|---|---|
| device registration (`coincube-gui/src/services/connect/grpc/device.rs:47`) | add `chain-identity-v1` to the capabilities sent (`create_session, cancel_session` today) | — |
| after `ResolveSigners` (`on_signers_resolved`) | `resp.network` must equal `wallet.chain.api_str()`; otherwise create **nothing** | R1.10: "Connect reports this Vault on a different network than this Cube. Nothing was sent. Reopen the Cube; if this repeats, contact support." |
| same | on a BTCB2 Vault, a target whose `capabilities` lack `chain-identity-v1` (and, after B3.2, `btcb2-unified-v1`), or an `unresolved` entry with `signer_app_outdated` | R1.7 row: "<name>'s Keychain needs updating before it can sign on Bitcoin Blake2b." — no session for that signer |
| `create_session_for` | `network: wallet.chain.api_str()` on every `CreateSigningSessionRequest` | — |
| create response and every fetch | `session.network` must equal `wallet.chain.api_str()`. Empty on a BTCB2 Vault ⇒ pre-identity Connect ⇒ cancel the session | R1.11: "Connect needs updating before Keychain can sign on Bitcoin Blake2b. Nothing was sent to the signer." Empty on a Bitcoin-family Vault is tolerated during the compatibility window (canonical §7, Q1) |
| `SIGNATURE_SUBMITTED` merge | unchanged: the signature is verified under `wallet.chain`'s rule by the existing verifier (#392 replay model). That verification is not a substitute for the identity checks above | — |

Server-side refusals the desktop must render (message prefix is the token):
`NETWORK_INVALID`, `NETWORK_MISMATCH`, `CHAIN_IDENTITY_REQUIRED`, `TARGET_KEY_NOT_ON_VAULT`,
`SIGNER_APP_OUTDATED`, `NETWORK_DISABLED`, and `NotFound "vault not found"` for a
vault the account neither owns nor is a member of. Copy for each is in the canonical §8.

## 4. Rail 2 — pairing protocol v3

### 4.1 `PairingOffer` v3 (QR JSON)

```jsonc
{
  "v": 3,
  "key": "<signer xpub, exact>",          // unchanged
  "dh": "<hex sha256(descriptor)>",       // unchanged
  "net": "bitcoin-blake2b",              // NEW: Wallet.chain.api_str()
  "cert": "...", "certFp": "...", "svc": "...", "wfp": "...", "exp": 0,
  "psk": "<base64url 16 bytes>"          // as v2
}
```

- A desktop that implements this contract emits **v3 only**, for every chain
  (`PAIRING_PROTOCOL_VERSION` → 3). It never emits v2 for a BTCB2 Vault and no
  longer emits v2 at all; a pre-identity phone refuses `v: 3` at scan
  ("unsupported version"), which is the intended fail-closed outcome. The offer
  screen should say: "If Keychain reports an unsupported QR version, update Keychain."
- `net` is required and must be one of the closed set. The 35-byte worst case
  (`"net":"bitcoin-blake2b-testnet4",`) fits the 2048-byte QR cap.
- v1 offers are untouched (their removal keeps its own schedule).

### 4.2 Proof v3 (`phone_signer/pairing.rs`)

```
pairing_proof = HMAC-SHA256(psk, utf8("coincube-pair-v3") || ascii(desktop_cert_fp_hex) || ascii(phone_cert_fp_hex) || utf8(net))
```

Verified with the **handshake** cert fp and the desktop's **own** `net` (never the
phone-reported strings). `PAIRING_PROOF_DOMAIN` becomes per-version; keep
`coincube-pair-v2` only for verifying nothing — the desktop no longer emits v2 — but
the known-answer vector for v3 must be shared with
`keychain-app/test/services/local_signer/pairing_qr_test.dart` ("matches the locked Rust ↔ Dart vector").

### 4.3 `grpc/local_envelope.proto` delta (byte-identical in both repos; `make sync-local-envelope-proto`)

```proto
message SignerBinding {
  // ... 1-4 unchanged ...
  // Connect network id of the local record selected for this pairing. Must equal
  // the scanned offer's `net`; the desktop refuses otherwise. Empty only from a
  // pre-identity phone answering a v2 offer.
  string network = 5;
}

message PartialSignature {
  // ... 1-4 unchanged ...
  // The chain the phone signed for (== the presented session's network). The
  // desktop refuses a mismatch before verifying or merging the signature.
  string network = 5;
}
```

`ErrorEnvelope.code` gains `"network_mismatch"` (phone → desktop). `PresentSession`
is unchanged: the chain rides `connect.v1.SigningSession.network` (field 21).
`PairingComplete.completion_protocol` stays `1`.

### 4.4 `PairedPhone` and the pairing listener

- `PairedPhone` gains `#[serde(default)] pub network: Option<String>` (api string) and
  `#[serde(default)] pub capabilities: Vec<String>` (from `PairingComplete`; dropped
  today). `network: None` is a **legacy row** (paired under v2).
- `pairing_listener.rs`, after the existing proof / `SignerBinding` / fingerprint
  checks (`:300-419`): on a v3 offer `reported.network` must be non-empty (else
  R2.5 "Update Keychain and pair again.") and equal `offer.net` (else R2.4 "Exact
  pairing identity mismatch; pair again."). Store both fields on the row. The
  durable pairing transaction (`PAIRING_PROTOCOL.md`) is unchanged.
- One row per phone cert stays the model (`pairing_transaction.rs:99`): a pairing
  is now for one `(vault, key, chain)`; switching chains on the same desktop is a
  re-pair, as switching Vaults already is (canonical Q2).

### 4.5 Presenting and receiving (`phone_signer/mod.rs`, `pairing_store.rs`)

- `PairedPhone::exact_signer(descriptor)` takes the Vault's chain. A row is usable
  only if `row.network == Some(wallet.chain.api_str())`; a legacy row (`None`) is
  usable only for Bitcoin-family Vaults. The hw refresh loop applies the same
  predicate when deciding which phones to dial for the loaded Vault.
  - BTCB2 Vault + legacy row → not dialled; signer list: "Pair this Keychain again
    for Bitcoin Blake2b." (R2.6)
  - row on another chain → not dialled; "Paired for <other network>. Pair again to
    use it here." (R2.7)
- `sign_tx` sets `session.network = wallet.chain.api_str()` on the `PresentSession`.
- On `PartialSignature`: `partial.network` must equal `session.network` **before**
  the signature is verified or merged; otherwise discard with R2.13 "The signature
  Keychain returned is for a different network. Pair again." A phone `ErrorEnvelope`
  with code `network_mismatch` or `pair_again` is surfaced with its message.

### 4.6 Old-client matrix (Rail 2)

| desktop | Keychain | outcome |
|---|---|---|
| this contract (v3 offer) | pre-identity | scan refuses `v: 3`; nothing is paired |
| this contract, legacy `PairedPhone` row | new | Bitcoin-family Vaults keep working (`session.network` set; the phone compares it with its record); a BTCB2 Vault never dials the row (R2.6) |
| pre-identity (v2 offer) | new | the phone accepts v2 only while it holds no BTCB2 record for `key`; sessions carry no `network` and are admitted as the record's Bitcoin-family network. A pre-identity desktop cannot run a BTCB2 Vault |
| this contract | new | full v3 |

## 5. Tests the implementing slice must add (fail-before / pass-after)

- v3 offer serialisation and the 2048-byte bound with the longest `net`.
- Proof v3 known-answer vector (shared with Keychain), plus: wrong `net`, wrong psk,
  swapped fps, empty — all reject.
- Listener: v3 offer + empty `reported.network` → R2.5; `reported.network != offer.net`
  → R2.4; success stores `network` and `capabilities`.
- `exact_signer`: same-descriptor twin Vaults (`ChainId::Bitcoin` vs
  `ChainId::BitcoinBlake2b`) resolve only the row on their own chain; legacy row
  usable on Bitcoin only.
- `sign_tx`: `session.network` present; `PartialSignature.network` mismatch is
  discarded before `signatures::verify…`; the `lan_keychain_native` example
  round-trips on a Bitcoin regtest Vault and on a BTCB2 regtest Vault (the latter
  reaching the phone's B3.2 refusal, not BDK).
- Rail 1: `on_signers_resolved` mismatch sends no `CreateSigningSession`; `network`
  present on every create; empty `network` on a BTCB2 Vault cancels; capability
  registered. Bitcoin-family sessions render and merge exactly as today (existing
  tests unchanged).

## 6. Not in this contract

The unified verifier and replay model (#392), the signer-index `Capable` flip and
B3.1–B3.3, the Claim wizard's creation of the BTCB2 `Key` row on Connect, and any
change to `docs/SIGNING_FLOW.md`'s Bitcoin behaviour. The contradictions this
contract has with `company-brain/plans/bitcoin-blake2b/PLAN-bitcoin-blake2b-coincube.md:137`
("only the sighash policy and signature verification change") and the other plan
paragraphs are listed in the canonical §12; brain files are not edited here.
