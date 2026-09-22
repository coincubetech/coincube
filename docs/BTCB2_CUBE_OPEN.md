# BTCB2 Cube creation and reopening

Fork Cube creation uses the Connect-backed Vault installer. A fresh Cube receives
an independent identifier, a PIN, and its own master seed even when its Vault is
watch-only. The seed is displayed for offline backup and advancing requires an
explicit written-backup confirmation. Encrypted persistence follows successful
backend admission; Cube settings retain the exact ChainId and master fingerprint.
The post-create continuation reopens the Cube through PIN verification.

Successful fork PIN verification keeps the decrypted signer in a private,
one-use slot owned by that unlock screen. It does not put the seed in cloned UI
messages or the legacy shared signer cache. The slot derives the Cube encryption
key directly. No Liquid or Spark SDK is constructed. Passkey and remote legacy
wallet routes remain unavailable for the fork.

The Loader requires the current authenticated Connect client and a local Vault.
It reads only that ChainId's wallet directory, rejects config/ChainId/datadir
mismatch, and starts the authenticated embedded daemon. It never attaches to an
external daemon socket, migrates provider fallback configuration, or starts a
local node. Startup retries are user-triggered, one attempt per click, with a
Loader-owned 30-second minimum interval; failed admission cannot lose that bound
when its temporary provider is dropped.

Home or App logout/token replacement invalidates every open fork tab before the
account change propagates. The fork App is dropped into a fresh Home, removing
its wallet, signing/export panels and matching Cube PIN session. App tasks are
owned and cancelled; task, subscription and view callbacks carry the session
generation so stale signing results cannot affect a later reopened App. A CPU
operation already running may finish, but its revoked completion is discarded.
Pending installer, metadata-save and Loader tasks are
aborted; their ephemeral authority is dropped. Save and PIN completions carry a
session generation, so already queued results cannot revive a replaced session.
A late blocking decrypt writes only to its detached old slot. The fork metadata
save never opens a PIN session; the current PIN screen owns that transition.
Fork settings updates share a stable sibling writer lock and commit by atomic
replacement after a complete synced temporary file. Cancellation before commit
leaves the prior settings file intact; after commit it leaves the full new file.
The installer applies its insert/update/remove delta to locked current settings.
Touched records must still match the original snapshot (or the desired result);
a conflicting concurrent edit refuses without writing. Unrelated records, wallet
settings and preferences remain intact.
The replacement Home executes its startup task; fork discovery is read-only.
Unlock credentials are cleared and the Cube must
be reopened with the current session. Changing a backend likewise requires a new
authenticated context.

The narrow authenticated Connect capability uses the Vault-only App constructor.
Home offers BTCB2 only after the current authenticated account reports its flag
true; missing/unloaded/false flags refuse. Installer, PIN and Loader recheck the
flag, then exact-chain anchor admission validates the backend before writes.
Generic runtime support stays dormant for migration, duress and unmanaged node
paths. Mainnet and developer-only testnet4 stay separate. Existing Bitcoin paths
keep their legacy constructors and SDK behavior. Full synthetic create/reopen/
failure validation and independent final-head review remain release gates.

## Operational acceptance

The selected Connect API must expose the authenticated BTCB2 status/anchor and
Esplora routes for the exact mainnet or testnet4 identity. No JWT is persisted in
fork daemon configuration. The server's Bitcoin Esplora port3000 must never serve
these routes; BTCB2 loopback3002 is not a Mac/droplet endpoint until infrastructure
finishes indexing and provides accepted tailnet access. Testnet4 remains separately
configured and fail-closed. Live connectivity, indexing stability, and correct
endpoint acceptance remain external gates; mocks do not satisfy them.

Rollback is to keep or restore BTCB2 feature exposure off and close its session.
No Bitcoin directory migration, service restart, deployment, or existing-wallet
rewrite is part of this change. Robert retains deployment and flag control.
