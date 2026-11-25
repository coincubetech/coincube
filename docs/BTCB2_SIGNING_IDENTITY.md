# BTCB2 signing identity: chain-bound LAN pairing (v3) and the desktop's Rail 1 obligations

Status: **frozen interface contract, v1 — documentation only; no code, proto or
protocol change in this PR.** This is the desktop half of the cross-repository
contract whose canonical text is `coincube-mobile/docs/btcb2-signing-identity-contract.md`
(parent record [keychain-app#145](https://github.com/coincubetech/keychain-app/issues/145),
tracker [coincube-api#276](https://github.com/coincubetech/coincube-api/issues/276)).
Read that file for the identity model, the full refusal matrix and the open product
questions; this file quotes what a desktop implementer needs and must not drift from
it. Verified against `coincube` `964e6ac1d6dd04f60a4cc56999c3b3c83fa676a9`.

**Amendment 1** (coincubetech/coincube-mobile#276, merged; master plan decisions 16,
21, 22 and 30). The canonical copy is amended in three ways, tagged as it tags them,
and this mirror follows: **A1**, the BTCB2 admission capability is
`btcb2-unified-v2` and `btcb2-unified-v1` is retired; **A2**, no compatibility
window on the LAN rail: v3 only, a pre-v3 pairing is never used on any chain;
**A3**, no compatibility window on Connect: a session with no chain is refused on
every chain. Where this mirror and the canonical copy differ, the canonical copy
wins.

**Implementation status (Rail 2).** Desktop PR 8a implements §4.1 to §4.3 and the
listener's network check in §4.4: the desktop emits v3 only with `net`, computes and
verifies the `coincube-pair-v3` proof over `net`, and refuses a missing or different
`SignerBinding.network`; `grpc/local_envelope.proto` carries the two `network` fields
with the comments quoted in §4.3 and the `network_mismatch` code. Desktop PR 8b stores
the row's `network` and `capabilities` (§4.4): a row without a network is a pre-v3
pairing, kept listed and removable and refused for every Vault with the R2.6 legacy
copy, and pairing the same phone for another chain replaces its row, which the pairing
screen says before the user scans. Desktop PR 8c puts the dial predicate (§4.5) in `exact_signer_against` with the
Vault's chain: another chain's row gives R2.7, a Bitcoin Blake2b row without both
capabilities gives the R2.6 capability copy, the settings and signer lists show them,
and the blanket Bitcoin Blake2b Wi-Fi refusals are gone. The network on
`PresentSession`/`PartialSignature` follows in PR 8d. The v3 known-answer vector
is `phone_signer/pairing.rs::pairing_proof_known_answer_vector`: psk = the 16 bytes
`0x00..0x0f` (`AAECAwQFBgcICQoLDA0ODw`), desktop fp `"aa"×32`, phone fp `"bb"×32`;
`net = bitcoin-blake2b` →
`ccc65575005c19fbb4826f49c68db719fc1c7f2bc0de4deea2c6e4d722c5263c`, `mainnet` →
`e7f46e42e05dd6a69c1f8372353d38fbfce4ee54d1c7ddd13bee54759ef5854a`,
`bitcoin-blake2b-testnet4` →
`1c81ce3dea97020338d1dc09f88e9eaafdd100187f92fb7d3895d9fd8f5a6114`.

## 1. Why the desktop is involved

A BTCB2 Vault is the same descriptor, xpubs and fingerprints as its Bitcoin twin
(`docs/BTCB2_CHAIN_BINDING.md` explains why the daemon needed `ChainId`). The two
Mobile signing rails inherit the ambiguity:

- **Rail 1 (Connect)** — `keychain_sign.rs` sends `vault_id`, `descriptor_id` and
  `SignerTarget{device_id, key_id, key_fingerprint}` (`coincube-gui/src/app/state/vault/keychain_sign.rs:1016-1073`),
  never the chain, although it has it: `Wallet.chain: ChainId` (`coincube-gui/src/app/wallet.rs:141`).
- **Rail 2 (LAN)** — before v3, the v2 `PairingOffer` carried `key`, `dh`, `wfp`,
  `psk` (`coincube-gui/src/phone_signer/pairing.rs` at the commit above), no chain. The phone picks a
  local record by xpub alone and echoes its backend id in `SignerBinding.key_id`,
  which `sign_tx` copies into every LAN session's target (`phone_signer/mod.rs:293-298`).
  The pairing store is datadir-global (`pairing_store.rs:183-190`), one row per
  phone cert (`pairing_transaction.rs:99`), scoped to a Vault by
  `vault_fingerprint = sha256(descriptor)[..4]` — equal for both twins — so a phone
  paired for a Bitcoin Vault is dialled for its BTCB2 twin.

`ReplayProtection` for Mobile keys defaults to `Legacy`; after Rail 1 resolves
a target, only a BTCB2 target advertising both capability literals may classify
that exact signer as `Capable` for signer/path selection. Transaction replay
status still comes only from cryptographically verified retained signatures.

## 2. Chain identity, as the desktop states it

Always `Wallet.chain.api_str()` (`coincube_core::chain::ChainId::api_str`, one of
`mainnet | testnet | testnet4 | signet | regtest | bitcoin-blake2b | bitcoin-blake2b-testnet4`).
**Bitcoin-family** means exactly the five pre-fork ids (`!chain.is_blake2b()`); the two
BTCB2 ids are never Bitcoin-family (canonical §2.1).
Never `bitcoin::Network`, never derived from the descriptor. An unknown string from
any peer is a refusal, never mapped to Bitcoin (`ChainId::from_api_str` already
returns `None`).

Capability strings (`RegisterDeviceRequest.capabilities`, `PairingComplete.capabilities`):
`chain-identity-v1` = the peer implements this contract; `btcb2-unified-v2` = the
peer can sign `ALL|UNIFIED`, sent only by a build that loads the unified signing
library (A1). A phone missing either literal is outdated for BTCB2 signing: the
desktop must not create or present a BTCB2 session to it. `btcb2-unified-v1` is
retired: released builds sent it without being able to sign, so it admits nothing,
and a phone that stores it without `btcb2-unified-v2` is outdated.

## 3. Rail 1 — desktop obligations (`keychain_sign.rs`, `coincube-gui/src/services/connect/grpc/`)

Proto additions land in `coincube-api` first and reach this repo via `make sync-proto`
(`grpc/connect.proto` is a vendored copy):
`CreateSigningSessionRequest.network = 13`, `SigningSession.network = 21`,
`SubmitPartialSignatureRequest.network = 5`, `SignerTarget.capabilities = 5`,
`ResolveSignersResponse.network = 3`, `UnresolvedSigner.reason = "signer_app_outdated"`.

| when | rule | refusal |
|---|---|---|
| device registration (`coincube-gui/src/services/connect/grpc/device.rs:47`) | add `chain-identity-v1` to the capabilities sent (`create_session, cancel_session` today) | — |
| after `ResolveSigners` (`on_signers_resolved`) | `resp.network` **non-empty** and ≠ `wallet.chain.api_str()` → create **nothing**. The comparison is a literal string compare: Connect canonicalises the Cube's stored value first (a legacy `bitcoin` row is echoed as `mainnet`; canonical §2.1), so the desktop never maps aliases | R1.10: "Connect reports this Vault on a different network than this Cube. Nothing was sent. Reopen the Cube; if this repeats, contact support at coincube.io/support." — rendered as a link to `https://coincube.io/support` |
| same | `resp.network` **empty** (pre-identity Connect), on **any** Vault, BTCB2 or Bitcoin-family → create **nothing** (A3: a Bitcoin-family Vault proceeded here during the compatibility window, which is closed) | R1.11: "Connect needs updating before COINCUBE Mobile can sign on `<label>`. Nothing was sent to the signer.", where `<label>` is the Vault's §2.1 display label (`ChainIdExt::label()`): "Bitcoin Blake2b" on BTCB2; the Bitcoin-family strings are the canonical copy's proposal, owner to confirm |
| same | on a BTCB2 Vault, a target whose `capabilities` lack `chain-identity-v1` or `btcb2-unified-v2` (one storing only the retired `btcb2-unified-v1` included), or an `unresolved` entry with `signer_app_outdated` | R1.7 row: "`<name>`'s COINCUBE Mobile needs updating before it can sign on Bitcoin Blake2b." (`<name>` = the signer's name in the Vault) — no session for that signer |
| `create_session_for` | `network: wallet.chain.api_str()` on every `CreateSigningSessionRequest` | — |
| create response and every fetch | `session.network` must equal `wallet.chain.api_str()`. Empty on any Vault ⇒ pre-identity Connect ⇒ cancel the session (defensive: the session exists and a signer may already have been notified — no "nothing was sent" promise here; that promise belongs to the resolve-time check above) | R1.11 session variant: "Connect needs updating before COINCUBE Mobile can sign on `<label>`. The request was cancelled." (A3: an empty network on a Bitcoin-family Vault was tolerated during the compatibility window; it no longer is) |
| `SIGNATURE_SUBMITTED` merge | unchanged: the signature is verified under `wallet.chain`'s rule by the existing verifier (#392 replay model). That verification is not a substitute for the identity checks above | — |

Server-side refusals the desktop must render (message prefix is the token):
`NETWORK_INVALID`, `NETWORK_MISMATCH`, `CHAIN_IDENTITY_REQUIRED`, `TARGET_KEY_NOT_ON_VAULT`,
`SIGNER_APP_OUTDATED`, `NETWORK_DISABLED`, and — on an ordinary spend — `NotFound "vault not found"`
for a vault the **authenticated Connect account** does not own (ordinary create is owner-only — decided, Robert
2026-09-18; recovery spends keep the unchanged heir gate's `PermissionDenied`). Copy for
each is in the canonical §8.

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
  screen should say: "If COINCUBE Mobile reports an unsupported QR version, update COINCUBE Mobile."
- `net` is required and must be one of the closed set, canonical spelling only (the
  phone refuses `bitcoin`). The worst-case fragment `"net":"bitcoin-blake2b-testnet4",`
  measures 33 UTF-8 bytes of raw JSON (`len('"net":"bitcoin-blake2b-testnet4",'.encode())`;
  the value is 24 characters), ≈ 44 characters after base64url. The phone's cap is on
  the **encoded** payload (`pairing_qr.dart:118`, 2048); with the ECDSA P-256
  self-signed certificate this desktop emits (`phone_signer/identity.rs`, ~400–500-byte
  DER) a v3 payload encodes to an estimated ≈ 1.3–1.45 K characters. That is an
  estimate: the implementing slice pins the longest emitted v3 payload under the cap
  with a test (§5).
- v1 and v2 offers are **not read**: a phone that implements this contract accepts
  `v: 3` only and refuses v1 and v2 at scan (R2.3). (A2: this bullet read "v1 offers
  are untouched (their removal keeps its own schedule)".)

### 4.2 Proof v3 (`phone_signer/pairing.rs`)

```
pairing_proof = HMAC-SHA256(psk, utf8("coincube-pair-v3") || ascii(desktop_cert_fp_hex) || ascii(phone_cert_fp_hex) || utf8(net))
```

Verified with the **handshake** cert fp and the desktop's **own** `net` (never the
phone-reported strings). `coincube-pair-v3` is the only domain either app computes
or verifies (A2: the v2 domain is not kept, since no v2 offer is read). The
known-answer vector for v3 must be shared with
`coincube-mobile/test/services/local_signer/pairing_qr_test.dart` ("matches the locked Rust ↔ Dart vector").

### 4.3 `grpc/local_envelope.proto` delta (byte-identical in both repos)

This repo's `grpc/local_envelope.proto` is the **source of truth** for the LAN
protocol; `coincube-gui/build.rs` compiles it. There is **no** sync target for it in
this repo's `Makefile` (only `sync-proto`, which copies `connect.proto` *from*
`coincube-api`); the `sync-local-envelope-proto` target named in the proto's own
header comment lives in **coincube-mobile**'s Makefile and copies *from* this repo
(`make sync-local-envelope-proto COINCUBE_DESKTOP_PATH=<this checkout>` then
`make proto-gen` there). The implementing slice edits the file here, commits, and the
Mobile slice pulls it; byte-identity is checked by diffing the two files.

```proto
message SignerBinding {
  // ... 1-4 unchanged ...
  // Connect network id of the local record selected for this pairing. Must equal
  // the scanned offer's `net`; the desktop refuses otherwise. Required: every
  // pairing is v3, and the desktop refuses an empty value.
  string network = 5;
}

message PartialSignature {
  // ... 1-4 unchanged ...
  // The chain the phone signed for: the presented session's network, which the
  // desktop always states. Required: the desktop refuses an empty or different
  // value before verifying or merging.
  string network = 5;
}
```

A2 rewords these two comments; field names, numbers and types are unchanged.
`grpc/local_envelope.proto` carries them verbatim since Desktop PR 8a.

`ErrorEnvelope.code` gains `"network_mismatch"` (phone → desktop). `PresentSession`
is unchanged: the chain rides `connect.v1.SigningSession.network` (field 21).
`PairingComplete.completion_protocol` stays `1`.

### 4.4 `PairedPhone` and the pairing listener

- `PairedPhone` gains `#[serde(default)] pub network: Option<String>` (api string) and
  `#[serde(default)] pub capabilities: Vec<String>` (from `PairingComplete`; dropped
  today). `network: None` is a **legacy row** (paired before v3), which is never
  dialled for any Vault (A2).
- `pairing_listener.rs`, after the existing proof / `SignerBinding` / fingerprint
  checks (`:300-419`), where `reported` is the existing alias for
  `complete.signer_binding.as_ref()` (`coincube-gui/src/phone_signer/pairing_listener.rs:372`) — the new field is
  `SignerBinding.network` (§4.3), **not** a field on `PairingComplete`: on a v3 offer
  `reported.network` (i.e. `complete.signer_binding.network`) must be non-empty (else
  R2.5 "Update COINCUBE Mobile and pair again.") and equal `offer.net` (else R2.4 "Exact
  pairing identity mismatch; pair again."). Store that value and
  `complete.capabilities` on the row — the capability list is what the BTCB2 dial
  predicate below reads; answering a v3 offer is evidence of v3 pairing, not of the
  advertised capability strings. The durable pairing transaction
  (`PAIRING_PROTOCOL.md`) is unchanged.
- One row per phone cert stays the model (`pairing_transaction.rs:99`): a pairing
  is now for one `(vault, key, chain)`; switching chains on the same desktop is a
  re-pair, as switching Vaults already is (canonical Q2 — decided (a), Robert
  2026-09-18: an advanced feature, correctness over UX for now).

### 4.5 Presenting and receiving (`phone_signer/mod.rs`, `pairing_store.rs`)

- `PairedPhone::exact_signer(descriptor)` takes the Vault's chain. A row is usable
  only if `row.network == Some(wallet.chain.api_str())`; a legacy row (`None`) is
  **never usable and never dialled, for any Vault** (R2.6, legacy variant; A2: a
  legacy row was usable for Bitcoin-family Vaults until the release that closed the
  compatibility window). For a **BTCB2** Vault the row's stored `capabilities` must
  additionally contain the literals `chain-identity-v1` **and** `btcb2-unified-v2`;
  a v3 row lacking either is not usable and not dialled, and a row that stores
  `btcb2-unified-v1` without `btcb2-unified-v2` is such a row (A1: the allowance that
  a row with `chain-identity-v1` alone may be dialled is removed). Bitcoin-family
  Vaults require no capability. The hw refresh loop applies the same predicate when
  deciding which phones to dial for the loaded Vault.
  - any Vault + legacy row → not dialled; signer list: "This pairing predates
    network-aware signing. Pair this phone again." (R2.6, legacy variant; the
    separate BTCB2 string "Pair this phone again for Bitcoin Blake2b." is retired,
    proposed by A2, owner to confirm)
  - BTCB2 Vault + v3 row without the required capability → not dialled; signer list:
    "This phone's COINCUBE Mobile needs updating before it can sign on Bitcoin Blake2b." (R2.6,
    capability variant)
  - row on another chain → not dialled; "Paired for `<other network>`. Pair again to
    use it here." (R2.7) — `<other network>` is the **display label** from the
    canonical §2.1 table (`bitcoin-blake2b` → "Bitcoin Blake2b"). Core `ChainId`'s
    `Display` writes `dir_name()` (`coincube-core/src/chain.rs:119-129`, `:160-163`)
    and is **not** a user-facing label — a naive `{chain}` would render "Paired for
    bitcoin-blake2b." This repo **already has** the label: `ChainIdExt::label()`
    (`coincube-gui/src/chain.rs:84-93`, trait at `:55-64`, pinned by `labels_and_tickers`
    at `coincube-gui/src/chain.rs:180`) returns exactly the canonical table, plain "Bitcoin" for mainnet
    included. The implementing slice **reuses** it — no new accessor (earlier
    revisions wrongly said no label existed).
- `sign_tx` sets `session.network = wallet.chain.api_str()` on the `PresentSession`.
- On `PartialSignature`, **before** the signature is verified or merged:
  `partial.network` is **required** and must equal `session.network`; empty or
  different → discard, R2.13 ("Your signature wasn't accepted and nothing was
  broadcast from it. The signature COINCUBE Mobile returned is for a different
  network. Pair again." / "Your signature wasn't accepted and nothing was broadcast
  from it. Update COINCUBE Mobile and pair again." when missing — canonical R2.13,
  byte-identical; desktop-side copy: the desktop discards, so the phone user may see
  nothing — recorded, not decided). Every dialled row is a v3 pairing, so there is
  no other case (A2: the legacy-row admission of an empty `partial.network` is
  removed).
  A phone `ErrorEnvelope` with code `network_mismatch` or `pair_again` is surfaced
  with its message.

### 4.6 Old-client matrix (Rail 2)

| desktop | COINCUBE Mobile | outcome |
|---|---|---|
| this contract (v3 offer) | pre-identity | scan refuses `v: 3`; nothing is paired |
| this contract, legacy `PairedPhone` row | pre-identity | the row is never dialled, for any Vault (R2.6, legacy variant). The remedy is a phone update and a re-pair (A2) |
| this contract, legacy `PairedPhone` row | new | the row is never dialled, for any Vault (R2.6, legacy variant); on the phone a legacy binding is never used (R2.12). The remedy is a re-pair with a v3 offer (A2) |
| pre-identity (v2 offer) | new | the v2 offer is refused at scan (R2.3): the phone reads `v: 3` only. No binding is written and nothing is signed until the desktop is updated and the pair is made again with a v3 offer (A2) |
| this contract | new | full v3 |

**Deployment prerequisite (not a protocol property).** BTCB2 Mobile signing over
LAN is enabled only on desktops that implement this contract and emit v3 offers
(LANE-B3 §B3.4: BTCB2 stays Connect-rail-only, or is refused on LAN, until
chain-bound pairing ships). A pre-identity desktop must never have BTCB2 Mobile
signing enabled — it may load a `ChainId::BitcoinBlake2b` Vault (`coincube-core/src/chain.rs`),
but the v2 offer such a desktop emits carries no
chain, so a phone holding only the Bitcoin twin cannot tell a Bitcoin session from a
BTCB2 one and would sign on its Bitcoin-family record. A2: before the amendment this
prerequisite was what made the admitted legacy path safe. That path no longer exists
(a phone that implements this contract refuses every v2 offer and every legacy
binding), so the prerequisite now constrains only desktops that predate this
contract; it remains a deployment gate, not a check. Canonical §4.6 says the same.

## 5. Tests the implementing slice must add (fail-before / pass-after)

- v3 offer serialisation and the 2048-**encoded-character** bound with the longest
  `net` and the largest certificate the desktop can emit.
- Proof v3 known-answer vector (shared with COINCUBE Mobile), plus: wrong `net`, wrong psk,
  swapped fps, empty — all reject.
- Listener: v3 offer + empty `reported.network` → R2.5; `reported.network != offer.net`
  → R2.4; success stores `network` and `capabilities`.
- `exact_signer`: same-descriptor twin Vaults (`ChainId::Bitcoin` vs
  `ChainId::BitcoinBlake2b`) resolve only the row on their own chain; a legacy row
  is not dialled for a Bitcoin-family Vault or for a BTCB2 Vault (A2); a v3 row whose
  stored capabilities lack the literal `chain-identity-v1` or `btcb2-unified-v2` (one
  storing only `btcb2-unified-v1` included) is not dialled
  for a BTCB2 Vault, while a Bitcoin-family Vault dials a row **whose `network`
  matches it** regardless of capabilities (the positive control uses a matching
  Bitcoin-family row; a BTCB2-network row is never dialable for a Bitcoin-family
  Vault — the network predicate is checked first).
- `sign_tx`: `session.network` present; `PartialSignature.network` mismatch
  discarded and empty discarded, before `signatures::verify…` (A2: the legacy-row
  shapes, "legacy row + empty accepted on a Bitcoin Vault" and "existing v2 pairing +
  pre-identity phone completes a Bitcoin signature", are removed); the
  `lan_keychain_native` example round-trips on a Bitcoin regtest Vault and on a
  `bitcoin-blake2b-testnet4` Vault — the only non-mainnet BTCB2 identity in the
  closed set; there is no BTCB2 regtest `ChainId`, and the B4 harness tests the node
  pair, not a Vault identity (canonical §11) — the latter reaching the phone's B3.2
  unified signature, not BDK.
- Rail 1: `on_signers_resolved` non-empty mismatch sends no `CreateSigningSession`;
  **resolve → create against an old Connect** (empty `resp.network`): a Bitcoin-family
  Vault and a BTCB2 Vault each send nothing (A3); `network` present on every create;
  empty `network` on a created/fetched session cancels it, on every chain;
  capability registered; a target storing only `btcb2-unified-v1` reads as outdated.
  Bitcoin-family sessions that state their network render and merge exactly as
  before; existing tests that relied on an empty Bitcoin session network now state
  it or expect the refusal (Desktop PR 4).

## 6. Not in this contract

The unified verifier and replay model (#392), the signer-index `Capable` flip and
B3.1–B3.3, the Claim wizard's creation of the BTCB2 `Key` row on Connect, and any
change to `docs/SIGNING_FLOW.md`'s Bitcoin behaviour. The contradictions this
contract has with `company-brain/plans/bitcoin-blake2b/PLAN-bitcoin-blake2b-coincube.md:137`
("only the sighash policy and signature verification change") and the other plan
paragraphs are listed in the canonical §12; brain files are not edited here.
