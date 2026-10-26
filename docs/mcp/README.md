# Experimental local AI access

This `redesign` integration ports the read-only Vault MCP runtime from `mcp`
commit `dfb2737082b940bb1e2a8d1a05a2eb4d07870d2a`. It is development
software. Packaged-client interoperability, signed-artifact smoke, and
independent security review are release gates, not claims made by this document.

## What works in this build

An unlocked Cube with one active Bitcoin Vault served by Tenshu's embedded daemon
can grant a local MCP client selected read permissions: balances, confirmed
history (at most 50 transactions), and spending-policy summary. The companion
`tenshu-mcp` process uses STDIO with the host, then authenticated device-local
IPC to Tenshu. It does not open a TCP listener. Public `tenshu_get_capabilities`
reports build support only; it does not report live consent or wallet inventory.

Liquid, Spark, imported non-Vault policies, Bitcoin Blake2b, external daemons,
and multiple/future Vault selections are unavailable. The ordinary build offers
no setup or transaction preparation tools, no keys or descriptors, no signing,
no broadcast, and no assistant-driven Cube unlock. Direct ChatGPT STDIO pairing,
a one-time-code host wizard, and edit-in-place permissions are planned work.

## Pairing a client

1. Unlock the Cube and open **Cube → Settings → AI access**.
2. Read the provider disclosure: your AI provider may receive financial data.
3. Enable access, enter a local label, select the current Vault, and select each
   read permission. The label is for your reference; it does not authenticate a
   host application's identity.
4. Choose **Pair client until revoked**. This creates a separate random bearer
   credential in a device-keystore-protected encrypted per-Cube registry.
5. Copy the private client configuration into a trusted host's local config,
   then clear the clipboard. It contains a credential. Keep it out of chats,
   screenshots, logs, tickets, and shared documents. Restart the host client.

A client stays paired until you explicitly revoke it. Access pauses on Cube
lock/close and resumes after normal unlock only for the same Vault descriptor
and storage identity. Disabling AI access pauses the clients. Revoke each client
individually; to change scope, revoke and pair again. The list says whether the
runtime is available, not whether a host is continuously connected. A failed
durable revoke is shown as an error and does not claim success. Cube deletion
removes its pairing material before wallet deletion.

## Build and verify

Use the pinned Rust toolchain. `make build` places `tenshu-mcp` beside the GUI;
`make test` includes the MCP protocol suite. The release workflow stages the
companion with Linux, macOS and Windows artifacts. The Unix packaged-binary
smoke fixture is `python3 contrib/check_mcp_binary.py /path/to/tenshu-mcp`.
CI and signed-artifact results must be checked against the integrated commit.

The read protocol and security tests live in `coincube-mcp/tests/security.rs` and
`coincube-mcp/tests/pairing.rs`; trusted UI/keystore tests live in
`coincube-gui/src/app/local_mcp.rs`. Fixtures use synthetic wallet data.
