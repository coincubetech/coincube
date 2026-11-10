# Testnet4 acceptance, without an agent in the loop

```sh
tests/e2e/run --qa-fresh --qa-run-dir /private/qa/run-001
```

The fresh flow generates its Cube, recovery material, 2-of-3 vault, phone signer,
funding request and transaction. It does not take an existing vault, descriptor,
PSBT, address, PIN or recovery phrase as input. Repeat the **same command and
run directory** to resume; choose a new directory for a separate disposable
vault. Pytest sequences the work and produces JUnit results. Appium/XCUITest
controls Keychain over USB. No model, camera, iPhone Mirroring, screenshots or
browser automation is involved in reruns.

## Fresh flow

1. Verify both public explorers' Testnet4 genesis and production Connect
   endpoints. Verify the configured QA account against the normal desktop
   Keychain session, then register the run profile through the production
   device-registration client. Persist its transport key before registration.
   Existing private auth caches can also be copied without changing their source.
2. Attach to the wired phone and verify the account. For a fresh signed-in QA
   keychain, select **Use a PIN**, generate and save its PIN, capture all twelve
   indexed recovery words, complete the real recovery challenge, and select
   Testnet4. Later runs reuse this account/device-bound credential.
3. Create a persistent Cube through the production Home state machine, complete
   the actual backup challenge, reopen its encrypted seed and unlock it.
   Register its stable identity and encryption key with production Connect.
4. Create a run-specific signer through Keychain's ordinary Add signer key
   flow. Reconcile the exact new key through Connect before creating a vault.
5. Generate and reconstruct a Border Wallet grid phrase, pattern and checksum.
   Install a primary **2-of-3 Master / Keychain / Border Wallet** vault with a
   delayed Border Wallet recovery path through the production installer. Verify
   its Cube appears on the phone through Connect.
6. Request Testnet4 faucet funding once, then require two independent explorers
   to agree on the funded outpoint and raw previous transaction. No prepared
   funding transaction or local Bitcoin Core synchronization is required.
7. Generate a recoverable test recipient and change address, construct a full
   PSBT, and add the Master signature. Pin its exact inputs, outputs and fee.
8. Create/recover one encrypted Connect request with a stable request ID. Verify
   the network, destinations, amounts and fee shown on the phone before using
   the generated PIN to approve it. Before entering the PIN, background/resume
   the app and record three seconds of challenge survival. This is lifecycle
   evidence; it does not claim to prove a Home-refresh event occurred.
9. Collect and verify the phone signature using production native crypto.
   Finalize against the saved spend intent, persist the signed bytes, and send
   exactly that transaction to the Testnet4 Esplora broadcast endpoint.
10. Require independent Mempool and Blockstream transaction acceptance. Add
    `--qa-confirm` to require a mined confirmation.

The fixed default spend is 10,000 sats with a 1,000-sat fee. Configured spend and
fee are bounded at 100,000 and 10,000 sats respectively. The native helper
verifies the funded script, previous transaction, primary quorum and actual
signatures. Unlike the legacy Core-backed mode below, fresh mode does not run a
separate local `testmempoolaccept`: the receiving Testnet4 node validates the
broadcast, and two independent explorers verify its acceptance.

## Workstation authentication and USB prerequisites

The fresh flow needs a signed-in disposable QA account shared by Tenshu and
Keychain. Account signup/email OTP, Apple trust/unlock/Developer Mode and
certificate provisioning remain workstation prerequisites. They are not
reported as successful test stages. The runner never logs out or replaces an
existing master. An existing passkey master cannot become an unattended PIN
signer merely by creating another key: it reports `PHONE_MASTER_NOT_OWNED`.
A fresh QA account at **Use a PIN** is supported; once the suite has provisioned
it, subsequent fresh-vault runs reuse its encrypted credentials automatically.

Store nonsecret workstation settings in private, gitignored
`tests/e2e/.local/workstation.json`, using `workstation.example.json` as its
schema. `account_email` is the exact disposable QA account; native bootstrap
refuses any different authenticated user before registering a device.
`profile_dir` names the normal signed-in desktop profile, not a prepared Cube
or vault. A legacy cache is optional: when absent, the suite reads the normal
desktop session from macOS Keychain and performs supported device registration.
`desktop_session_namespace` is `development` for a debug Tenshu build and
`production` for the release app; this selects only its existing Keychain
namespace. Both API endpoint values always remain pinned to production. `ios.minimum_build` must identify the installed
reviewed Keychain build. Fresh onboarding also requires accessibility forwarding
through Keychain's screenshot-protection wrapper: build 36 hides the recovery
controls from XCTest, and cannot complete this flow. Build 37 is the candidate
for that fix; its number alone is not source or acceptance evidence.
`ios.xctestrun` can identify an already-signed WebDriverAgent
runner so the suite starts it automatically when needed.

Install Appium 3.9.0 and XCUITest 12.22.8, plus Python 3.11+, Xcode and the pinned
Rust toolchain. The launcher installs pinned Python dependencies once. Fresh
mode builds both Rust helpers and signs the provisioning helper with the normal
COINCUBE Developer ID/profile automatically. No TestFlight upload is required
for a correctly signed build already installed over USB.

```sh
# Synthetic tests only: no phone, network, credentials or funds.
tests/e2e/run --self-test

# Read-only environment checks; no onboarding, Cube, funding or signing.
tests/e2e/run --qa-fresh --qa-run-dir /private/qa/run-001 --qa-preflight

# Full live flow. Repeat this exact command after an interruption.
tests/e2e/run --qa-fresh --qa-run-dir /private/qa/run-001 --qa-deadline 180
```

Use `--qa-config /absolute/private/workstation.json` to override the default
workstation configuration. Only one physical acceptance process may own the
phone at a time; the suite holds a USB lease and exclusive run/account locks.

## Credentials and recovery after interruption

Generated Cube PIN, Master and recipient recovery phrases, Border Wallet phrase,
pattern/checksum, and phone PIN/recovery words are encrypted in macOS Data
Protection Keychain. The signed native helper verifies readback before use.
Phone updates use compare-and-swap revisions; a stale process cannot overwrite
newer recovery state. Nonsecret locator files and private checkpoints stay in
the run directory. Keep both the Mac's Keychain and the run directory for
recovery; copying the run directory alone does not copy its credentials.

No secrets are placed in argv, Git, JUnit, screenshots or raw Appium logs.
Authentication caches, descriptors, PSBTs and signed transactions remain private
files under the run directory. The generated Cube uses production encrypted
seed storage. Secret-get responses travel only over a captured child-process
pipe; do not invoke them with stdout forwarded to a terminal or log.

A faucet timeout leaves a durable claim intent. A rerun checks that address
instead of requesting another payment. Explicit empty/rate-limit responses
retain the service's retry window. The public faucet can be unavailable; that
is `BLOCKED`, not successful funding. Its developer API uses `btc-testnet` for
Testnet4 ([official API](https://www.cypherfaucet.com/api),
[service source](https://github.com/Tech1k/cypherfaucet.com)).

A PIN attempt is recorded before entry and blocks further attempts across
process restarts and different run directories until the exact key/signature
is verified through Connect. A delayed signature is polled without reopening
approval. Interrupted initial PIN entry fails closed if partial input cannot
be determined. One completed PIN setup whose recovery words were never captured
and whose challenge was never submitted may restart at the full fresh-setup
chooser after an app restart. The runner archives the previous entry markers
before acting and retains the same encrypted PIN. It never repeats an unlock,
resets an existing master, or discards a saved phrase/submission. A second such
restart is blocked. Keychain's normal local-blob and server-key guards must pass
again before a new recovery phrase appears, and Settings identity is verified
before Cube creation or funding. Lost broadcast responses reconcile the same transaction ID;
retries can send only the previously pinned signed bytes.

Exit **0** means selected stages passed, **1** an assertion failed, **2** a
selected live run was blocked. Preflight success is only preflight success.
`checkpoint.json` records stage outcomes, and `results.xml` is sanitized JUnit.
Ordinary repository collection skips live tests; these skips are not acceptance
evidence. This flow is opt-in and is not added to GitHub CI.

## Legacy prepared-vault replay

Schema version 1 remains available for the original saved draft. It uses a
prepared disposable vault, full locally signed PSBT, exact spend intent and a
synchronized local Testnet4 Core. It requires its own explicit configuration:

```sh
tests/e2e/run --qa-config /private/qa/config.json --qa-run-dir /private/qa/legacy-run
```

The following setup details apply only to that legacy mode.

## One-time workstation and fixture setup

Use a dedicated QA account and disposable Testnet4 signer/profile. The suite
never resets Keychain or signs the user out. Keep the phone signed in, trusted,
unlocked, in Developer Mode, with XCTest UI automation authorized. Install a
Keychain build containing #194 and set its real build number as the minimum
(the default minimum is 35; a build number alone is not source provenance).

Install the pinned Appium tools in the test user's normal environment:

```sh
npm install -g appium@3.9.0
appium driver install xcuitest@12.22.8
```

Python 3.11+ is required. `run` creates `tests/e2e/.venv` and installs the pinned
Python dependencies once. It also finds a versioned Homebrew Python when macOS
`python3` is older. Later runs reuse that environment.

Start an already-signed WebDriverAgent on the phone, or set `ios.xctestrun` to
its existing signed `.xctestrun` file; the suite can launch that runner with
`xcodebuild test-without-building`. This does not provision certificates or
change trust settings. The WDA tunnel is created and removed automatically;
no `iproxy`, Wi-Fi device address or iPhone Mirroring is needed. Only runner
processes started by this invocation are stopped.

Build the native helper once from the same checkout using the pinned Rust:

```sh
PATH=/opt/homebrew/opt/rustup/bin:$PATH \
  BREEZ_API_KEY=testnet4-fixture-placeholder \
  cargo build --locked -p coincube-gui --features integration-tests \
  --example testnet4_connect
```

The signed Cube test also requires the COINCUBE Developer ID and the repository's
normal macOS signing profile. Its runner uses the current checkout's own Cargo
target directory. It generates its own PIN/recovery material, verifies it and
removes the disposable Cube and device secret. It does not supply the persistent
live-vault fixture.

Copy `config.example.json` outside Git and fill in the **existing QA** fixture:

- `profile_dir`: isolated Tenshu root; `testnet4/connect.json` must hold the
  existing active account/device, and `testnet4/connect_transport_key` its
  existing transport key. The helper never creates/replaces one. An expired
  cached account session reports `connect_auth_expired`; refresh through the
  ordinary application before resuming.
- `psbt_file`: base64 **full** saved draft, with the valid local Master Signer
  signature, authenticated previous transactions and complete key origins.
  Keep the original immutable. Do not provide the phone-pruned wire payload.
- `descriptor_file`: the fixture vault's private descriptor text.
- `cube_id`, `vault_id`, `target_key_id`, `target_fingerprint`: the existing
  production Connect records for that disposable Testnet4 vault/signer.
- `intent`: exact input outpoints and two output addresses/amounts, expected
  unsigned transaction ID, and fee cap. This suite supports native P2WSH primary
  spends with one recipient and one change output. It refuses other policies.
- `core`: synchronized Testnet4 Core on a literal loopback address with its
  cookie file. No credential-bearing remote RPC URL is accepted.
- `ios.cube_label`: exact visible Cube label. `ios.pin_file` is optional for
  signers using the ordinary app PIN flow; never put a real credential in JSON.

Set private fixture files, including the cached auth/transport files, to mode
600 and their containing fixture/run directories to 700. No secrets, raw PSBTs,
descriptors, private identifiers, PINs, or real run reports belong in Git.
`config.example.json` contains placeholders and cannot accidentally run live.
The `run_id` is stable for one immutable spend; a different funded transaction
requires a new run ID and directory.

The historical GUI request had no QA note and cannot be adopted blindly.
This runner reuses its prepared draft but creates its own deterministic request
on a live invocation. It does not cancel any historical session. Do not run the
GUI's signing flow concurrently for the same draft.

## Commands and results

```sh
# Local, synthetic regression tests: no network, phone, credentials or funds.
tests/e2e/run --self-test

# Existing fixture/device checks without creating a session or broadcasting.
tests/e2e/run --qa-config /private/qa/config.json \
  --qa-run-dir /private/qa/run-001 --qa-preflight

# Live acceptance, automatically resuming the exact request/transaction.
tests/e2e/run --qa-config /private/qa/config.json \
  --qa-run-dir /private/qa/run-001 --qa-deadline 120
```

Exit **0** means all selected stages passed, **1** means an assertion failed,
and **2** means a selected live run was blocked. A preflight exit 0 is only
preflight success. Ordinary repository pytest collection skips live tests
unless configured; those skips are not acceptance evidence. No live tests are
added to GitHub CI, and xdist is refused for the physical-device scenario.

`checkpoint.json` records stage outcomes and the stable session/transaction
identity; `results.xml` is a sanitized JUnit report. `signed.psbt` and
`signed.hex`, if produced, remain private in the run directory. The process
holds an exclusive run lock and a shared USB automation lease. File writes use
atomic rename and fsync. Changed configuration or draft bytes cannot reuse a
checkpoint. A crash during approval never causes an automatic second PIN
attempt; the runner first queries Connect and otherwise reports the uncertain
approval for reconciliation.

No raw Appium server logs, page source, screenshots, RPC payloads or native
errors are saved. Failures use stable codes such as `REQUEST_DELIVERY`,
`NATIVE_CONNECT_AUTH_EXPIRED`, `KEYCHAIN_BUILD`, `MEMPOOL_REJECTED` and
`PUBLIC_PROPAGATION`. These are what routine automation should report; an agent
is only needed to investigate an unfamiliar failure or update changed selectors.

The read-only standalone diagnostics remain available:
`diagnostics/ios_usb_wda.py` and `diagnostics/testnet4.py [--txid TXID]`.

References: [Appium attach to running WDA](https://appium.github.io/appium-xcuitest-driver/latest/guides/attach-to-running-wda/),
[real-device prerequisites](https://appium.github.io/appium-xcuitest-driver/latest/preparation/real-device-config/).
