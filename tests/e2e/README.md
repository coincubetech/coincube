# Testnet4 acceptance, without an agent in the loop

```sh
tests/e2e/run --qa-config /private/qa/config.json --qa-run-dir /private/qa/run-001
```

Repeat **the same command and run directory** after a delivery timeout, app
update, or interruption. No model, screenshot interpretation, camera pairing,
or browser automation is used. Pytest owns sequencing and results; Appium's
XCUITest driver controls the real iPhone through a USB-only WebDriverAgent
tunnel. The native helper reuses Tenshu's production Connect, encryption,
descriptor, PSBT merge, and signature-verification implementations.

This is an API/physical-signer integration suite, not desktop visual acceptance.
It exercises Cube creation through the production Rust state machine. The live
spend uses a **prepared disposable Testnet4 vault and locally signed draft**.
It does not recreate an account, enroll a passkey, provision a vault, obtain
faucet funds, or exercise the desktop's mouse-driven vault builder on each run.
Those are fixture setup, not silently passing test cases. The original
agent-driven run remains incomplete until delivery/signing/broadcast succeeds.

## What one run does

1. Require a synchronized local Bitcoin Core on Testnet4, checking its chain
   name and BIP94 genesis. Testnet3 address prefixes alone are insufficient.
2. Run the signed, disposable Cube create → recovery verification → save →
   reopen → unlock regression. A successful result is cached by source revision.
3. Read the dedicated profile's existing Connect account/device/transport key.
   Require production REST `https://api.coincube.io` and gRPC
   `https://grpc.coincube.io:443`. Authenticate the Cube/vault and require
   Testnet4, the expected descriptor, a primary 2-of-3 policy, exactly one valid
   existing primary signature, and the enrolled phone signer.
4. Attach Appium to WDA over USB. Check installed Keychain build, Testnet4 in
   Settings, and the named Cube in the phone's Connect-backed Cubes list.
5. Check the exact funded inputs, recipient, change, unsigned transaction ID
   and fee. An absent/spent input blocks instead of silently re-funding.
6. Create or recover the same encrypted Connect request using a stable,
   checkpointed `request_id`. Select its unique note on the phone; never approve
   the first arbitrary pending request.
7. Verify the phone displays the intended network, both output addresses,
   amounts and fee, then use its ordinary approval flow. An existing disposable
   signer PIN can come from a private file. A real Face ID/passkey/OS prompt
   that cannot be completed automatically reports BLOCKED; it is not bypassed.
8. Fetch the exact session, decrypt the signature response, reject unexpected
   transaction changes, merge only permitted signature material, and verify
   the quorum with production native validation.
9. Finalize with Core, recheck outputs/fee, run `testmempoolaccept`, record the
   exact signed bytes before sending, and broadcast on Testnet4.
10. Verify transaction acceptance independently with Mempool and Blockstream.
    Add `--qa-confirm` to require mining; normal acceptance includes mempool.

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
checkpoint. A crash or timeout after the Approve tap never causes an automatic
second approval or PIN attempt; rerunning skips the phone and keeps polling
Connect for the signature, reporting `SIGNATURE_DELIVERY` if it still has not
arrived.

No raw Appium server logs, page source, screenshots, RPC payloads or native
errors are saved. Failures use stable codes such as `REQUEST_DELIVERY`,
`NATIVE_CONNECT_AUTH_EXPIRED`, `KEYCHAIN_BUILD`, `MEMPOOL_REJECTED` and
`PUBLIC_PROPAGATION`. These are what routine automation should report; an agent
is only needed to investigate an unfamiliar failure or update changed selectors.

The read-only standalone diagnostics remain available:
`diagnostics/ios_usb_wda.py` and `diagnostics/testnet4.py [--txid TXID]`.

References: [Appium attach to running WDA](https://appium.github.io/appium-xcuitest-driver/latest/guides/attach-to-running-wda/),
[real-device prerequisites](https://appium.github.io/appium-xcuitest-driver/latest/preparation/real-device-config/).
