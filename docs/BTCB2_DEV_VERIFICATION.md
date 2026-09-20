# BTCB2 development verification and infrastructure handoff

This is a verification procedure, not evidence that a deployment or a user flow
has passed. Record the exact integrated API and desktop commits, test results,
and independent review before release. Component tests do not accept a changed
integration head. Robert retains merges, deployment, environment changes and
production flags. Use synthetic wallets and new datadirs only.

## Configuration boundary

Tenshu uses `COINCUBE_API_URL` for the authenticated Connect account, features,
network status, anchor, pricing and chain-specific Esplora backend. Set the
accepted development API base, not a node/indexer URL. The GUI build reads `.env`
and release builds require this value at compile time; rebuild after changes.
The RPC credentials and dedicated upstream URLs belong to the API deployment,
never a desktop Cube, backup or committed `.env`.

The API requires distinct `BTCB2_ESPLORA_URL` and optional
`BTCB2_NODE_RPC_URL` with either local cookie-file authentication or both
`BTCB2_NODE_RPC_USER` and `BTCB2_NODE_RPC_PASSWORD`. Missing optional RPC does
not prevent API startup; it prevents authenticated backend admission. Separate
`BTCB2_TESTNET4_*` variables must remain unconfigured until that infrastructure
exists. `BITCOIN_BLAKE2B_ENABLED` and account overrides belong to the operator;
this procedure does not change them. A missing/unloaded/false account flag must
not offer a usable fork Cube.

Follow the API [infrastructure runbook](https://github.com/coincubetech/coincube-api/blob/test/btcb2-api-integration-20260919/docs/btcb2-node-ops.md)
for the exact environment, authenticated read-only commands and HTTP/state
matrix. Pin that document to the accepted API commit in the release record.
Never put bearer tokens in command arguments, logs or Buzz. Use a private
0600 header file for operator curl checks; do not follow redirects with auth.

The September 19 reported BTCB2 RPC `127.0.0.1:39332` and REST
`127.0.0.1:3002` are **server loopback** on `coincube-bitcoin-service`
(`100.125.72.49`). They are not endpoints for a Mac or another droplet.
Bitcoin's `100.125.72.49:3000` must never be substituted. Infrastructure owns
index completion/stability, reachable tailnet endpoints, ACLs, and live RPC
acceptance. Agents must not restart or reconfigure those services.

## Reproducible local checks

Run from the task worktree, with repository-pinned tooling and an isolated
build target. A Rust version printed by the installed binary is the evidence;
a toolchain file alone does not establish which compiler ran. These commands
require the repository's normal native dependencies and `protoc`.

```sh
set -eu
umask 077
TESTED_HEAD=$(git rev-parse HEAD)
rustc --version
cargo --version
export CARGO_TARGET_DIR="$PWD/target-btcb2-verification"
export BREEZ_API_KEY=synthetic-test-key
export COINCUBE_API_URL=https://synthetic-api.example.invalid
cargo fmt --all -- --check
cargo test --locked -p coincube-core
cargo test --locked -p coincubed
cargo test --locked -p coincube-gui --all-targets
cargo clippy --locked --all-targets -- -D warnings
test "$(git rev-parse HEAD)" = "$TESTED_HEAD"
```

Do not add the GUI `integration-tests` feature: it uses a live development API.
Connect session unit tests must use the in-memory test secret store introduced
by `510fda9e2e9b4908066aec18d2fffceff7e3d5e1`. Other legacy keyring tests require
a disposable OS account/keyring; a temporary datadir alone does not isolate
OS credentials. CI's ephemeral runner is an appropriate environment. Keep
Bitcoin controls in the whole-package runs. A sandbox socket/keyring failure
is a failed local run, not evidence that the corresponding tests passed.

The pinned two-chain harness is
[`.github/workflows/btcb2-regtest.yml`](../.github/workflows/btcb2-regtest.yml).
It runs on a PR with the `btcb2` label, or an explicit workflow dispatch. Require
`BTCB2_HARNESS_REQUIRED=1`, zero skipped tests, the pinned indexer commit and
actual test names in the log. CI normally checks out the PR merge ref: record
its commit and compare its tree with the reviewed content before attributing
results to a head SHA. Genesis compatibility does not establish fork identity,
chain-exclusive funds, or the complete desktop create/sign/reopen workflow.

## Enabled-flow acceptance with fixtures, then development infrastructure

The authenticated Connect runtime is narrower than generic runtime support.
It must admit only the exact selected fork chain and native P2WSH Vault through
the current authenticated client. Generic local-node, external socket, migration,
passkey, duress and unsupported signer paths must not become enabled merely
because this entry path exists. No Breez/Spark client should be constructed.

Exercise the following with a synthetic Connect server and regtest or fixture
Esplora before using accepted development infrastructure:

1. Log in with an account whose loaded features allow BTCB2. Create a fresh
   Vault using the local CubeKey, confirm its seed backup, and choose a PIN.
   No admission failure may write a Cube, seed or daemon database. Test disabled,
   missing auth, wrong chain, inactive fork, malformed anchor and indexer lag.
2. Open the admitted Vault, close it, and reopen it from Home using its PIN.
   Verify the exact `bitcoin-blake2b` directory, Cube network, daemon chain and
   authenticated Esplora path; repeat the identity checks for testnet4 fixtures.
   No lookup, seed or cache may use the Bitcoin encoding twin's directory.
3. Delay each async startup/save response, then log out or replace the account.
   A late response must not restore a signer, PIN, bearer context or daemon.
   Retry must obey the startup backoff and start with new authenticated evidence.
4. Scan a synthetic funded wallet, construct/sign/finalize a spend and verify
   protective signatures retained in the final witness. Cross-check the full
   two-chain acceptance/rejection behavior. A capability flag or PSBT record
   alone is not replay protection; unknown evidence stays unknown.
5. Advance blocks during a long scan, then separately reorg the observed anchor.
   Ordinary growth must finish; changed or unavailable evidence must refuse
   persistence and recover safely on retry. Exercise restart, stale evidence,
   throttling and indexer catch-up without selecting a Bitcoin fallback.
6. Return fresh, stale, malformed and unavailable pricing. Only valid BTCB2
   quotes permit fiat toggling. Native units are expected until verified
   NonKYC/Neoxa adapters exist; Bitcoin's asset price is never a substitute.
7. Test backup/export and wrong-chain restore refusal against same-ID Bitcoin
   controls. Repeat Bitcoin creation/open/sign behavior with the fork flag off.

For the eventual manual dev check, use a disposable OS account/keyring and
new datadir. Build with the accepted dev API URL and normal repository build
instructions. The executable accepts `--datadir`; it has no BTCB2 CLI network
switch. Select the chain through the authenticated Home UI after its gates pass.

```sh
umask 077
BTCB2_TEST_DATADIR=$(mktemp -d "${TMPDIR:-/tmp}/tenshu-btcb2-dev.XXXXXX")
/path/to/reviewed/coincube --datadir "$BTCB2_TEST_DATADIR"
```

Do not import an existing wallet, use live funds, or broadcast on mainnet for
agent validation. Live outage/restart exercises remain infrastructure-owned.
No command in this document starts or changes a server service.

## Limits, rollback and launch record

Future server timestamps are rejected; the current anchor contract allows
90 seconds of past age and no positive clock skew. Check client/server clocks
when admission reports stale. Do not fix it by locally restamping observations.
Status and anchor share a per-IP API rate-limit bucket; multiple clients behind
one NAT can throttle. Authority throttling can also put the daemon provider in
its existing cooldown. Record these availability limits in dev acceptance.

Claim/Split remains hidden until authenticated observation collection, ownership
and inclusion checks, restart-safe state, both transaction steps and pre-broadcast
validation are implemented and independently tested. The
[core assessment](BTCB2_CLAIM_SAFETY.md) is only a prerequisite; its eligible
result is not signing or broadcast permission. Dynamic RDTS expiry uses chain
MTP, never hard-coded expiry or the machine clock. Six confirmations must be
rechecked with reorg handling. Missing transactions and post-fork timestamps
never prove permanent chain exclusivity. Keychain LAN remains gated until
chain-bound pairing and signing prerequisites are accepted.

Rollback is operator-controlled feature disable plus reverting the reviewed app
release as needed. Do not remap fork data into Bitcoin directories, rewrite
wallets or remove databases. Disabling the API feature is not revocation of all
existing recovery/decrypt interactions: the signing admission/submission gate
has a narrower scope. Log out/stop the fork context to revoke its in-memory
backend authority. Preserve isolated data for a subsequent safe release.

The launch record must distinguish completed source/test gates from pending
infrastructure acceptance, verified exchange schemas/conversion semantics,
Claim/Split implementation, Keychain/hardware matrices and owner release
approval. Report missing code as missing code, not as an external outage.
