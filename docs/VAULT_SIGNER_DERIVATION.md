# Vault signer derivation and Developer mode

New Vault installers generate a fresh hot signer regardless of Developer mode
or whether the Cube master is available. Developer mode controls test-network
visibility; it neither selects the hot key nor changes the stored user setting
when a Cube signer is unavailable. The Cube master remains the authority for
Cube-level services, including encrypted Connect envelopes.

## Existing Vaults

Older Developer-mode installers could reuse the Cube master as a Vault hot
key. This fix does not rotate that key, rewrite a descriptor, or migrate any
seed. Existing encrypted per-Vault files still load by the descriptor's
fingerprints and Cube credential. The fingerprint-scoped session cache remains
a valid source for these legacy keys. The passkey Cube guard remains necessary:
a legacy Vault file bearing the Cube master's fingerprint does not mean the
Cube has a PIN-protected master seed on disk.

## Border Wallet grid phrases

BIP-85 provenance continues to name the exact source fingerprint. New Vaults
derive from their independent installer signer; old records can still name the
Cube master. Signing first checks the loaded hot signer and then the scoped
Cube session, without substituting another seed. As in ordinary non-Developer
creation before this fix, an installer signer not included in the descriptor is
not automatically persisted. If that source is absent, the user reconstructs
from the saved grid phrase, pattern and checksum required by the wizard.
This change adds no seed persistence or new secret-export path.
