# BTCB2 encrypted seed storage

The installer passes its explicit ChainId to encrypted seed persistence and
fingerprint-based retry verification. Bitcoin keeps its historical network
folders; BTCB2 uses `bitcoin-blake2b/mnemonics` or
`bitcoin-blake2b-testnet4/mnemonics`. Seed encryption, Cube AAD, filenames,
permissions and overwrite refusal are unchanged. No existing file is migrated.

The original Network-taking signer APIs remain compatible Bitcoin wrappers.
The explicit `*_for_chain` APIs select only the requested chain directory.
They never search the Bitcoin sibling when a fork seed is absent.

This does not enable BTCB2 creation or seed-only product flows. Runtime gates
remain closed. General signer discovery, unlock/restore entry points and
creation must carry the same identity before activation; Connect signing
identity enforcement and safe feature exposure are separate acceptance gates.
The seed-only helper's chain support is shared persistence plumbing, not an
assertion that Liquid/Spark wallets are supported for BTCB2 Cubes.

Validation uses generated seeds and disposable directories only. Live wallets,
server services, production flags and deployment remain outside this change.
