# BTCB2 descriptor source gates

The descriptor picker carries explicit ChainId. BTCB2 creation exposes the
local Cube key, hardware devices, Border Wallet keys, Mobile keys attached
to the Cube and imported/pasted xpubs. A manually supplied xpub is watch-only
here; accepting its encoding makes no claim about signature capability or
replay protection. The Wizardsardine provider tokens (safety-net and
cosigner) have been removed and are no longer a key source.

## Hardware devices and Border Wallet (HW-1, 2026-10-06)

Robert lifted the #422 creation gate for hardware devices on 2026-10-06
("I'd like to lift it"), and in the same session added Border Wallet
(Amendment A). The decision is recorded on coincube #568. Mobile keys and
provider tokens remain gated: Mobile keys wait for Lane B3, and tokens need
their own decision.

- **Hardware devices.** The picker shows a Hardware Device card, runs device
  discovery, and fetches the xpub on the chain's Bitcoin network (BTCB2
  mainnet uses `Bitcoin`, BTCB2 testnet4 uses `Testnet4`). Device firmware
  gates and policy registration (`RegisterDescriptor`) are the Bitcoin ones,
  unchanged. A device signs with the legacy sighash, so on BTCB2 its
  signatures are replayable: `KeySource::replay_capable` is false, the
  installer shows the non-blocking `NoReplayCapableSigner` advice for a path
  with only devices and xpubs, and a loaded Vault classifies an unmarked
  device as `UserMarked(false)`. The card's tip says so and recommends the
  Cube Key on the same path. Vault Settings' "Register wallet" also works on
  BTCB2 (Amendment B): it sends the device the same Vault name and descriptor
  string as on Bitcoin and stores the HMAC in that chain's settings only.
  Before a device signs a BTCB2 Vault spend, the
  signing picker shows the Split flow's device copy ("Your hardware wallet
  will call this a Bitcoin transaction...").
- **Border Wallet.** The card and the wizard work as on Bitcoin, with the
  same path-kind rules. A Vault spend on BTCB2 signs a Border Wallet key with
  the unified signer (`ALL|UNIFIED`); Bitcoin keeps the legacy signer. A
  Border Wallet fingerprint is `Capable` in `signers.rs`.

## Mobile keys (2026-11-19)

Mobile keys were held back above until Lane B3. COINCUBE Mobile now signs
BTCB2 with the unified signer and advertises `chain-identity-v1` and
`btcb2-unified-v2` (which replaced `btcb2-unified-v1`, master plan decisions 16
and 22), and the desktop's signing client checks both before it
prepares a BTCB2 session (`BTCB2_CONNECT_SIGNING_CLIENT.md`). This change
re-enables the Mobile Keys card on BTCB2. The earlier gate is recorded on
coincube #568, so this lift needs the same owner sign-off before merge.

- **What the card lists.** Signed in to Connect, the card opens the keys
  attached to this Cube (`get_cube_keys`), split into the user's own keys
  and contacts' keys, exactly as on Bitcoin. Each key arrives as an envelope
  sealed to the Cube's encryption key, so it can be opened only once the
  Cube has published that key (`BTCB2_CUBE_OPEN.md`). Signed out, the card
  asks for Connect, as on Bitcoin.
- **Only the Cube's chain.** On BTCB2 the list keeps only keys whose network
  is the Cube's chain (`CubeKeyRaw::listed_on_chain`, one filter on the
  shared list); on a Bitcoin Cube it drops only keys on a BTCB2 chain.
- **Whether the phone can sign.** Connect's key list says, per key on a
  BTCB2 Cube, whether its phone can sign BTCB2 (`btcb2Signing`,
  coincube-api#526, master decisions 12 and 28). Only `can_sign` is
  selectable. The other rows are shown disabled with why: `cannot_sign`
  needs a COINCUBE Mobile update, `unavailable` means the phone is no longer
  registered, and `unknown` means the phone could not be confirmed and asks
  the user to open COINCUBE Mobile on it. An unrecognised value, or no value
  (an API older than that field), fails closed the same way as `unknown`.
  The card itself has no tip when signed in, as on Bitcoin.
- **Creation.** A chosen key becomes a Connect Vault member, as on Bitcoin,
  so COINCUBE Mobile can find the Vault it signs for. `KeySource::KeychainKey`
  records the value seen when the key was picked; `available_for_creation`
  admits it on BTCB2 only when it is `can_sign`, and
  `KeySource::replay_capable` counts it for the path advice only then. That
  is a snapshot for path selection: a loaded Vault classifies the phone at
  sign time, and Connect re-resolves it there.
- **Still separate.** Mobile LAN signing on BTCB2 stays refused
  (`BTCB2_CONNECT_SIGNING_CLIENT.md`).

**Being allowed at creation is not replay evidence.** Only verified unified
witnesses make the replay pill say "protected"; neither the key source nor
its capability flag does.

## Enforcement

Every key source this picker offers is now allowed on BTCB2.
`available_for_creation` still lists them explicitly, so a new source is
refused on BTCB2 until it is added there. The picker refuses such a source's
key results through `LoadKey` and existing-key selection through `SelectKey`,
and descriptor apply independently refuses it, so stale editor state cannot
bypass the picker. The global runtime gate is unchanged.

This creation policy is distinct from `KeySource::replay_capable`: creation
capability says which sources may build a Vault, not whether its spends are
protected. Claim/Split, Mobile LAN signing and unsupported signer transports remain
separate launch gates.

Synthetic tests cover the per-chain creation rule, the picker's Mobile key,
hardware and Border Wallet messages, listing and selecting a key attached to
the Cube, device discovery and key fetch with a fake device, the grid's cards
signed in and out, descriptor apply with a `can_sign` Mobile key (including its
Connect Vault member) and its refusal of every other value, the key list's
chain filter, each `btcb2Signing` value per BTCB2 chain, the path advice and the spend-time device copy.
Bitcoin picker behavior stays unchanged. No live devices, phones, funds or
existing-wallet changes are used.
