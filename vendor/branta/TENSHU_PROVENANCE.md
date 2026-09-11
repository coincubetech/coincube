# Branta 3.2.1 provenance and compatibility adjustment

Upstream: https://github.com/BrantaOps/branta-rust
Reviewed commit: ba5e510a3189f32d11cb57e10267284b7109cb39
Tree: 578d9d2943ddab9f2ab52c06a886e3b9d5cd7781
Published 3.2.1 source commit: dff65e0f65c1970273a7cdc198ece63019692c76 (same tree).
Published crate checksum: 77720f45707c27d8c5e00b4428d72086711bb58362af5f1efc3dd2cf55f31b5a.

Every Rust source file under src/ is byte-identical to that commit; SHA-256 hashes
are recorded in UPSTREAM_SHA256.json. LICENSE is retained. Do not format or edit
these source files with wallet changes.

The only upstream manifest change is:

```diff
-reqwest = { version = "0.13.4", default-features = false, features = ["json", "rustls"] }
+reqwest = { version = "0.12.18", default-features = false, features = ["json", "rustls-tls"] }
```

Upstream reqwest 0.13 selects rustls-platform-verifier, which requires Apple's
security-framework-sys >=2.12. The existing wallet's Breez dependency pins
security-framework-sys =2.10.0 in its iOS-only build dependencies. Cargo resolves
those constraints across targets even for desktop builds. Cargo cannot resolve both. The adapter reuses the
wallet's existing reqwest 0.12.18 and webpki roots without altering Liquid or
reimplementing the SDK protocol. Other SDK dependency requirements are unchanged.
The GUI dependency is pinned to this local version =3.2.1. This is an adapted
vendored official SDK, not an unmodified crates.io dependency.

Remove this manifest adjustment when an upstream client/TLS configuration option
or wallet dependency resolution permits the unmodified release. SDK examples are
reference only: do not run them (they use live services and Loose mode).

TLS deliberately uses the existing wallet reqwest webpki/Mozilla root set, rather
than the newer upstream platform trust store. Corporate/private installed roots
will not be accepted; such lookups remain silent and non-blocking. Roots update
with dependencies. The logo client uses the same reqwest feature configuration.

The upstream lookup client still permits redirects and unbounded response bodies.
The wrapper bounds wall time but does not change those source semantics. An
upstream configuration option or separately reviewed vendor hardening is a
follow-up; redirect targets could receive the encrypted lookup token. Logos have
independent no-redirect, same-origin, response-size and image-decode limits.
