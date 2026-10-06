# BTCB2 descriptor source gates

The descriptor picker carries explicit ChainId. BTCB2 creation exposes the
local Cube key, hardware devices, Border Wallet keys and imported/pasted
xpubs. Keychain and provider-token (safety-net and cosigner) entry points stay
hidden until their complete chain-bound workflow is verified. A manually
supplied xpub is watch-only here; accepting its encoding makes no claim about
signature capability or replay protection.

## Hardware devices and Border Wallet (HW-1, 2026-10-06)

Robert lifted the #422 creation gate for hardware devices on 2026-10-06
("I'd like to lift it"), and in the same session added Border Wallet
(Amendment A). The decision is recorded on coincube #568. Keychain keys and
provider tokens remain gated: Keychain waits for Lane B3, and tokens need
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

**Being allowed at creation is not replay evidence.** Only verified unified
witnesses make the replay pill say "protected"; neither the key source nor
its capability flag does.

## Enforcement

Keychain and token navigation/fetch/result messages are refused before work,
including stale key results through `LoadKey` and existing-key selection
through `SelectKey`, which use the same `available_for_creation` rule.
Descriptor apply independently refuses unsupported sources, so a stale editor
state cannot bypass the picker. The global runtime gate is unchanged.

This creation policy is distinct from `KeySource::replay_capable`: creation
capability says which sources may build a Vault, not whether its spends are
protected. Claim/Split, Keychain LAN and unsupported signer transports remain
separate launch gates.

Synthetic tests cover the per-chain creation rule, the picker's refused and
allowed messages, device discovery and key fetch with a fake device, the
grid's cards, descriptor apply, the path advice and the spend-time device
copy. Bitcoin picker behavior stays unchanged. No live devices, funds or
existing-wallet changes are used.
