# BTCB2 ancestry split: release acceptance record

The production gate in `coincube-gui/src/services/claim_ancestry_gate.rs` stays
closed until every row below has a dated result for the exact release commit.
The disposable two-chain CI harness proves software behavior; it is not a
substitute for the deployed indexer, Connect, physical devices or desktop UI.
Record versions, commands, elapsed times and redacted logs with the review.
Never include account tokens, wallet seeds, private keys or wallet descriptors
in the evidence bundle.

| Check | Required evidence | Pass condition |
| --- | --- | --- |
| Pinned two-chain software suite | GitHub Actions run for the release commit, including `btcb2-harness`, platform unit tests and coverage | All required jobs pass, with no unexpected skips. The harness accepts each signed `S+B`/`S+F` transaction only on its own chain and covers restart, spent funding inputs and reorgs. |
| Deployed indexer recovery | Staging run using the deployed node/indexer binaries, service configuration and **same index database** across a deep reorg; record old and replacement tips and service logs | The supervisor recovers the indexer without deleting or rebuilding its database, reaches the replacement canonical tip, and the paired collector refuses the obsolete ancestry proof. Never induce this reorg on production mainnet. |
| Authenticated Connect scan | Timed Claim and Split deposit checks through the production Connect proxy with a representative Vault history; capture redacted per-chain request counts, response bytes and final status | Both chains' block, transaction, UTXO and tip reads use one account session. Claim ancestry discovery has one shared 90-second search deadline, 4,096-request ceiling and 16 MiB response ceiling across both chains and candidates. Final collection keeps its separate 30-second deadline and at most 256 remaining requests on the same counters; standalone collection and requalification retain their existing 30-second, 256-request bounds. A timeout or changed session refuses Sign without a one-sided result. Loopback measurements do not count. |
| Physical signing | BitBox and each supported Ledger model/Bitcoin-app version sign both ordinary PSBTs from a dedicated low-value wallet; retain device/firmware versions and redacted PSBT fingerprints | Each device displays the full destination and amount, approves every required input on both chains, and produces signatures the paired finalizer accepts. Rejection, partial support or a changed PSBT blocks launch. Do not use a production Vault for this check. |
| Desktop visual and accessibility review | Captures and reviewer notes for both Claim and Split deposits, coin review, signing, broadcast and recovery states, in supported themes/window sizes | Chain and address labels, input equations, amounts, fees, confirmation states and clipboard feedback remain readable. Keyboard focus and screen-reader access reach every action and warning; no clipping or hidden confirmation control. |
| Independent safety review | Reviewer identity, commit SHA, findings and disposition | A reviewer independent of implementation verifies chain-specific ancestry, ownership, reservations, exact PSBT/final-witness binding, journal recovery and one-use send gates. All blocking findings are resolved. |

After all rows pass, make one reviewed change that enables discovery, signing
and both broadcast stages together. Repeat the pinned suite and a final
low-value end-to-end Claim and Split check on that gate-enabled build before
shipping. A status-only recovery screen or a successful Bitcoin send does not
imply that the BTCB2 side is safe to send.
