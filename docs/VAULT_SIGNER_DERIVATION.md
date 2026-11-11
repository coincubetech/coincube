# Vault signer derivation and Developer mode

The Cube Key in a new Vault derives from that Cube's master seed using the
descriptor's normal BIP32 paths. Developer mode controls test-network
visibility and never selects another mnemonic or changes itself when a master
is unavailable. Bitcoin, Bitcoin Blake2b and their test networks follow this
same rule. Mainnet/testnet account paths and xpub encodings remain unchanged;
Bitcoin Blake2b still uses its existing unified-signature rules.

For an existing Bitcoin-family Cube, construction requires its authenticated,
Cube-ID- and fingerprint-scoped master session. An SDK client is not a source
of Cube identity. Bitcoin Blake2b deliberately keeps its root out of that
Bitcoin signer cache; its admitted installer reads only the Cube's chain-bound
encrypted seed with its current scoped PIN. Revoked loaders cannot enter this
path. Missing or inconsistent identity, credentials or seed material returns an
error instead of generating an unrelated Vault key. Passkey Cubes use their
authenticated master session without requiring a PIN.

A fresh standalone Bitcoin Blake2b Cube generates its single Cube master and
requires its existing seed backup step. Ordinary Bitcoin Vault creation needs
an existing unlocked Cube. Claim and recovery flows keep their explicit source
material and existing admission checks.

## Recovery and existing Vaults

A Cube master backup and the Vault descriptor reproduce newly created Cube
Keys. Recovery Kits keep their existing one-master-seed format; this change adds
no seed slot, remote secret payload or key migration. External hardware,
Keychain and separately imported keys retain their own recovery requirements.

Existing descriptors, encrypted Vault seed files and keys are not rewritten.
Historical Developer-mode Vaults that used the Cube master remain recoverable
from it. Historical independently generated Vault keys still require their
original seed backup or encrypted seed file; the Cube master cannot recreate an
unrelated historical seed. Loading continues to match the recorded fingerprint.
The passkey no-PIN classification guard and exact-source BIP85 provenance guard
remain necessary and unchanged.
