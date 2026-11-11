# Branta 4.0.0 provenance and compatibility adjustment

Upstream: https://github.com/BrantaOps/branta-rust
Reviewed commit: 6292bbd0c6a720e5c9273a4f4e8b827097db6e5e (tag v4.0.0)
Tree: 93d1941261080250c6f39a3ded0b7deac52e23d3
Published crate checksum: 7a8a135dbf6e1966c9debb9d619edd56d4eb27ebf57bed896c21d36ac7ed4b74.

The vendored files are the published 4.0.0 crate's CHANGELOG.md, LICENSE, README.md,
examples/ and src/. Every one is byte-identical to that crate except
`src/v2/client.rs` (see "Source patch" below); SHA-256 hashes are
recorded in UPSTREAM_SHA256.json and enforced by coincube-gui's `vendor_integrity`
test, which also fails on any unpinned file anywhere in vendor/branta (other than
this file, UPSTREAM_SHA256.json, Cargo.toml, and a transient Cargo.lock or target/
from running the SDK's own suite). The crate's tests/,
Cargo.lock and CI files are not vendored. Do not format or edit these source files
with wallet changes.

The only upstream manifest change is:

```diff
-reqwest = { version = "0.13.4", default-features = false, features = ["json", "rustls"] }
+reqwest = { version = "0.12.18", default-features = false, features = ["json", "rustls-tls"] }
```

Upstream reqwest 0.13 selects rustls-platform-verifier, which requires Apple's
security-framework-sys >=2.12. The existing wallet's Breez dependency pins
security-framework-sys =2.10.0 in its iOS-only build dependencies. Cargo resolves
those constraints across targets even for desktop builds, so it cannot resolve both.
The adapter reuses the wallet's existing reqwest 0.12.18 and webpki roots without
altering Liquid or reimplementing the SDK protocol. The 4.0.0 client only uses
reqwest APIs that exist in 0.12. Other SDK dependency requirements are unchanged.
The GUI dependency is pinned to this local version =4.0.0. This is an adapted
vendored official SDK, not an unmodified crates.io dependency.

Remove this manifest adjustment when an upstream client/TLS configuration option
or wallet dependency resolution permits the unmodified release. SDK examples are
reference only: do not run them (they use live services).

TLS deliberately uses the existing wallet reqwest webpki/Mozilla root set, rather
than the newer upstream platform trust store. Corporate/private installed roots
will not be accepted; such lookups remain silent and non-blocking. Roots update
with dependencies. The logo client uses the same reqwest feature configuration.

## Source patch: `src/v2/client.rs`

This is the only source file Tenshu changes. Upstream's lookup client follows redirects
and reads unbounded response bodies, so a redirect target could receive the
encrypted lookup token. 4.0.0 keeps its HTTP client private, so this cannot be
configured from outside the crate. The patch:

- builds the client with `reqwest::redirect::Policy::none()`, so a 3xx falls through
  the existing non-success branch as `BrantaError::Incomplete`;
- reads lookup bodies chunk by chunk up to `MAX_LOOKUP_RESPONSE_BYTES` (256 KiB),
  rejecting a larger declared `Content-Length` before reading; an oversized body is
  `BrantaError::Incomplete`;
- adds four offline tests (redirect not followed, oversized declared body rejected,
  oversized chunked body without Content-Length rejected, body exactly at the cap
  accepted), each marked "Tenshu patch test".

`post_payment` and `is_api_key_valid` are untouched; Tenshu never calls them.
UPSTREAM_SHA256.json records both the upstream hash (`b4a16f8d…`) and the patched
hash of this file under `patched_files`, and `vendor_integrity` enforces the patched
hash. The production hunks:

```diff
+pub(crate) const MAX_LOOKUP_RESPONSE_BYTES: usize = 256 * 1024;
 ...
-            http: reqwest::Client::new(),
+            http: reqwest::Client::builder()
+                .redirect(reqwest::redirect::Policy::none())
+                .build()
+                .expect("HTTP client"),
 ...
-        let bytes = match response.bytes().await {
-            Ok(bytes) => bytes,
-            Err(_) => return Err(BrantaError::Incomplete),
-        };
+        if response
+            .content_length()
+            .is_some_and(|len| len > MAX_LOOKUP_RESPONSE_BYTES as u64)
+        {
+            return Err(BrantaError::Incomplete);
+        }
+        let mut bytes = Vec::new();
+        loop {
+            match response.chunk().await {
+                Ok(Some(chunk)) => {
+                    if bytes.len() + chunk.len() > MAX_LOOKUP_RESPONSE_BYTES {
+                        return Err(BrantaError::Incomplete);
+                    }
+                    bytes.extend_from_slice(&chunk);
+                }
+                Ok(None) => break,
+                Err(_) => return Err(BrantaError::Incomplete),
+            }
+        }
```

To upgrade the SDK, re-apply this patch to the new `client.rs` and re-review it, or
drop it if upstream adds equivalent configuration. Offering it upstream is a
tracked follow-up. Logos keep their own independent no-redirect, same-origin,
response-size and image-decode limits in the wrapper.

## Testing limits

4.0.0 makes its HTTP client trait, AES helpers and secret generator crate-private,
so Tenshu can no longer run the real `BrantaService` over a fake client. The
wrapper's tests cover the status mapping through its own `LookupBackend` seam and
run the real `verify` only on paths that finish before any request: conflicting
on-chain addresses (`tampered`), and plain or unbindable destinations (`unknown`).
Decrypt-and-bind behavior relies on upstream's own unit suite (`src/v2/service.rs`,
`src/v2/client.rs`), which runs offline against local mock servers:
`cargo test --manifest-path vendor/branta/Cargo.toml --lib`. CI runs it in the
`branta_vendor_tests` job, seeded with the root Cargo.lock so shared dependencies
use the reviewed versions. Delete the `vendor/branta/Cargo.lock` a local run
creates (it is not vendored), or point `CARGO_TARGET_DIR` elsewhere to avoid a
`vendor/branta/target/`; both are ignored by git and by `vendor_integrity`.
