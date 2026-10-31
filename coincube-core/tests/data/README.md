# Unified sighash test vectors

`unified_sighash.json` is copied unchanged from Bitcoin Knots tag
`v29.4.1.knots20260508`, commit
`8c85b1585dac23f964e2dd32045624de7f02aa58`, at
`src/test/data/unified_sighash.json`.

Upstream file SHA-256:
`5c5e95fc1ab8ef9ce6b3cb6e76b8c74a987d182ebd01d8e2b98b6d0fbb26f630`.

Bitcoin Knots distributes the fixture under the MIT License. The exact upstream
copyright and permission notice is preserved beside it in
`LICENSE-BITCOIN-KNOTS` (SHA-256
`7c4a87f43afaf667b4c2187af92ebdd27310a24cec113f973e058e3300a76002`).

The Coincube test suite executes the 142 vectors for script types 0 and 1. The
12 taproot key-path and 12 tapscript vectors are intentionally skipped because
those script types are outside this implementation slice.

# Bitcoin Blake2b block fixture

`btcb2_block_979904.raw` is Bitcoin Blake2b mainnet block 979,904
(`000000000000000065508ea018e19120795fb5af170a89853ad3b477343fd252`, 49
transactions, 20,820 bytes), fetched unchanged on 2026-10-31 from Connect's
`/api/v1/esplora/bitcoin-blake2b/mainnet/block/<hash>/raw`. Public chain data;
`btcb2_block.rs` checks it hashes to that id, so a corrupted copy fails loudly.
