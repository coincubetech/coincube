# AI access: before and after the design pass

Content-only captures of Cube › Settings › AI access, drawn by the production
presenter with the software renderer (no app window, sidebar or page header).
`before/` is `redesign` at `b16083b64`; `after/` is this branch. Every state is
built from a disposable public-key test wallet and a synthetic pairing key.
Client names are synthetic labels; no configuration, credential or locator is
shown or written.

Regenerate (the gallery test also runs without the variable, as layout,
overflow and Spanish checks):

```sh
R06A_CAPTURE_DIR=/tmp/ai-access BREEZ_API_KEY=DUMMY_BREEZ_API_KEY \
  COINCUBE_ENABLE_PASSKEY=1 COINCUBE_PASSKEY_RP_ID=coincube.io \
  cargo test -p coincube-gui --lib design_gallery
```

The test writes 1500 px tall frames; these copies are trimmed to their content.

| File | State |
|---|---|
| `01-empty` | Nothing paired, AI access off |
| `02-unsupported` | No supported Vault loaded (1024 px window) |
| `04-pair-form-read` | Pairing form with read access and two parts chosen |
| `05-pair-form-transactions` | Pairing form with transaction preparation chosen (light) |
| `06-paired-available` | Two clients, one open after copying its configuration (1024 px) |
| `07-paired-paused-off` | Clients paused because AI access is off |
| `09-revocation-unconfirmed` | Durable revocation could not be confirmed |
| `12-transaction-stale` | An unsigned prepared transaction that is out of date |
| `13-empty-cube-setup` | Empty Cube: pairing a wallet-preparation client |

The matching design boards are on the Tenshu canvas, page "AI access · design
pass (impeccable, Nov 2026)".
