# Recipient identity checks

Tenshu performs optional, send-only Branta Strict lookups during preparation of
external Spark BOLT11 payments and VAULT Bitcoin/BIP21 requests containing both
`branta_id` and `branta_secret`. No lookup runs on typing, internal-transfer paths,
Liquid, Spark-native addresses/invoices, stablecoin/cross-chain sends, plain
Bitcoin addresses, BOLT12, LNURL or Lightning addresses. Ark and silent-payment
support in the SDK does not enable those payment routes in Tenshu.

The setting is local `global_settings.json:recipient_identity_checks`, exposed
in Cube Settings / General. New and older installations default on. Turning it
off invalidates outstanding privacy tickets, cancels their asynchronous work and
hides old identity results across tabs. No lookup or logo client starts while
disabled. Turning it on enables future preparation only. No account synchronization,
check/result/preference telemetry, API key, HMAC secret or payment registration.

Strict mode does not send plaintext destinations or payment amounts. The SDK
processes the original payload locally and sends an encrypted lookup token over
HTTPS. BOLT11 tokens are deterministic and repeated lookups are linkable. Branta
can observe lookup timing and the connecting network address. Only an explicit
"View with Branta" click opens the SDK verification URL; the secret-bearing URL
is neither displayed nor logged. Opening Branta's privacy link is also an explicit
user action, not a lookup or prefetch.

## Payment correctness

Spark joins its existing payment preparation with a bounded identity lookup;
responses carry a generation, and identity is attached to the exact prepared
handle. Edits, cancellation and navigation invalidate the prior generation.

VAULT retains BIP21 input separately from the parsed/network-checked address used
for transaction construction. URI amounts populate an empty amount field;
disagreements invalidate the form. Unknown required fields and ambiguous amounts
are rejected. Each original recipient row is matched to a distinct script/amount
output in the actual prepared PSBT. If optional association cannot be made (for
example, recovery/MAX changes after redraft), the valid PSBT proceeds without
identity metadata. Explicit BIP21 requested amounts still constrain the final output
regardless of the privacy preference. Association handles response
reordering. The existing duplicate-address form restriction remains; association
logic itself never collapses equal addresses. Descriptor-owned deposit/change
outputs are excluded before lookup. Existing output and fee checks remain in the
signing/broadcast path. Identity appears beside each output in review and again
inside the final broadcast modal. Identity is not persisted in saved PSBTs.

The three-second lookup deadline includes logos. Logo variants load concurrently;
exhausting their remaining budget preserves the already-bound identity. A VAULT batch shares one deadline
and has at most three outstanding lookups. Unsupported input, empty response,
transport/parsing/decryption error or timeout shows nothing and does not block
sending. Positive identity requires a successfully decrypted ZK destination equal
to the prepared destination. Identity attribution makes no claim of payment safety.

Only `BrantaError::Tampered` blocks sending and requires replacing the request.
In SDK 3.2.1 it is reachable only for authenticated on-chain BIP21 address mismatch,
not BOLT11. Wrong secrets and ordinary failures are silent, so absence of a block
is not a safety claim. The known bad destination stays blocked for the current form despite preference
changes, metadata/encoding changes, or edits to another recipient. Replace or
remove the affected destination to continue. Uppercase testnet bech32 URIs are skipped
for lookup because the SDK normalizes case only for mainnet bech32; wallet address
validation continues normally. Non-ASCII or ambiguous SDK payloads are skipped.

## SDK provenance and dependencies

Official Branta 3.2.1 source is vendored from reviewed commit
`ba5e510a3189f32d11cb57e10267284b7109cb39` (same tree as published source commit
`dff65e0f65c1970273a7cdc198ece63019692c76`), with version requirement `=3.2.1`.
The sole upstream manifest adjustment reuses reqwest `0.12.18` with `rustls-tls`.
No SDK Rust source was changed. See [provenance](../vendor/branta/TENSHU_PROVENANCE.md)
and enforced source hashes in `vendor/branta/UPSTREAM_SHA256.json`.

Upstream reqwest 0.13 uses platform-verifier, whose Apple dependency conflicts
with Breez's exact iOS build-dependency pin even on desktop because Cargo resolves
across targets. No Liquid file or dependency pin was changed. The adjustment uses
the wallet's webpki/Mozilla roots (including logos), not OS-added private roots.

Compared with the original lockfile:

| Dependency | Before | After |
|---|---|---|
| reqwest | 0.11.27 / 0.12.12 / 0.12.18 | unchanged |
| rustls | 0.20.9 / 0.21.12 / 0.23.37 | unchanged |
| aes-gcm | 0.10.3 | + 0.11.1 |
| base64 | 0.13.1 / 0.21.7 / 0.22.1 | + 0.23.1 |
| hmac | 0.12.1 | + 0.13.0 |
| digest, sha2 | existing multiple versions | unchanged |
| serde | 1.0.228 | 1.0.229 |
| serde_json | 1.0.149 | 1.0.151 |
| async-trait | 0.1.89 | 0.1.92 |
| tokio | 1.50.0 | 1.53.1 |
| uuid | 1.22.0 | 1.26.1 |
| thiserror 2 | 2.0.18 | 2.0.20 |

The lookup client's upstream redirect policy and unlimited response-body allocation
are retained to preserve reviewed source. The wrapper bounds elapsed time. A future
upstream client configuration option or separately reviewed source patch can disable
redirects/cap bodies. Logo fetching already rejects redirects, validates HTTPS and
exact origin for both variants, limits responses to 256 KiB, accepts PNG/JPEG only,
checks decode format and bounds dimensions/allocation, and caches 32 decoded images
in memory. All logo errors are silent. SVG and other image formats are not loaded.

## Validation

All integration tests use fakes or SDK dependencies injected with a fake HTTP
client. The SDK unit suite is run with `--lib`; its live integration examples are
never run. Implementation starts at base `66d65369554c4abbdc40610430bcf009c246a32c`.
Validation commands/results are recorded in the task handoff. Local Homebrew Rust
is 1.94.0, whereas the repository pin is 1.97.1; cross-platform/pinned-toolchain
validation cannot be claimed from that local run.

References: [SDK](https://github.com/BrantaOps/branta-rust),
[send-side pricing](https://branta.pro/pricing),
[privacy details](https://branta.pro/your-data),
[privacy policy](https://branta.pro/privacy#automatic-collection).
