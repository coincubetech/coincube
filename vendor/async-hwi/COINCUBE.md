This is the published `async-hwi` 0.0.32 crate (BSD-3-Clause), copied from
crates.io. The only change to its published source is an OP_RETURN compatibility
test in `src/bitbox.rs`. Its Cargo manifest raises `bitbox-api` from 0.9.0 to
0.11.0, the first Rust release that recognizes OP_RETURN PSBT outputs. This
allows BitBox02 firmware 9.24.0 or newer to sign the Bitcoin claim marker.

Keep this patch until upstream `async-hwi` supports `bitbox-api` 0.11 or newer.
