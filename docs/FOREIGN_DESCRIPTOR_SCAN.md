# Foreign descriptor discovery (scan only)

`services::foreign_scan::scan` accepts separately labeled external/internal public
output descriptors and bounded index ranges. This is an unused application
boundary: no Home action, import, signer, seed/hardware flow, Cube/datadir write,
Claim authorization or broadcast is installed.

Supported discovery descriptors: `pkh`, `wpkh`, `sh(wpkh)`, native `wsh`
(including sorted multisig) and `tr`, as parsed and sanity-checked by the pinned
Miniscript library. Private keys, hardened public derivation suffixes, multipath
expressions, unsupported descriptor forms and testnet xpubs refuse. Hardened
*origin annotations* are permitted because they require no private derivation.
Give external and internal branches separately; the scanner never guesses their
meaning. Taproot discovery explicitly reports no unified-signing capability;
all descriptor forms report no Claim authorization.

Only Bitcoin mainnet and Bitcoin Blake2b mainnet are currently accepted. The
Bitcoin testnet4 counterpart is absent from Connect; neither legacy testnet nor
mainnet may substitute for it. The authenticated client's fixed API base/token
is used only for the fork anchor. A separate anonymous client, without inherited
auth/device headers or redirects, sends derived address and transaction queries.
Descriptors/xpubs remain local. Address queries still disclose addresses to the
operator; this is not an anonymity guarantee.

API dependency: the fresh-address extension in coincube-api issue318. Dynamic
address statistics and UTXOs require explicit fresh/BYPASS/no-store response
markers; missing markers refuse. Stats use both chain and mempool transaction
counts: zero UTXOs does not mean an address was never used. Each address is read
twice to reject observed changes. Every returned UTXO is bound to the full
previous transaction's computed txid, output index, amount and locally derived
script. Immutable `/tx/{id}/hex` may use the normal API cache only because those
bytes are locally authenticated. Duplicate outpoints/scripts refuse.

A successful result means discovery completed within the specified history-gap
policy (or a single fixed address), not that no funds exist beyond the gap.
Insufficient range, address/body/time budgets, provider failures, malformed
responses, cancellation and changed tips return errors, never fabricated empty
wallets or partial "complete" data. The hard budgets are 200 total addresses,
1000 UTXOs, 2 MiB per response, 16 MiB aggregate Esplora response bytes, five
seconds per HTTP request and 30 seconds overall. Gap is 1–100. Callers must
choose conservative bounded ranges; there is no automatic retry loop.

Before and after discovery the fresh tip hashes must agree. On BTCB2 both must
also equal fresh authenticated operator anchor hashes. Indexer lag refuses.
This checks provider consistency, not proof of work, independent inclusion or an
atomic mempool snapshot. No post-fork timestamp or missing transaction proves
exclusive funds. Re-read before any later spend and follow the canonical #276
Claim expiry, confirmation-depth, poison and reorg gates separately.

The owner increments the supplied generation on logout, API/account changes or
cancel (dropping its sender also cancels); the scanner drops pending requests.
Recheck result generation at consumption because delivery can race revocation.
No result survives an incomplete collection as spend permission.

Verification: synthetic tests cover spent address history, bounded ranges,
changed tips, public descriptor capabilities/refusals, authenticated prevouts,
duplicate inputs, exact anonymous routing, missing freshness, service errors and
in-flight cancellation. Run the full `coincube-gui` test package on an ephemeral
pinned CI runner. Host GUI runtime tests remain prohibited until unrelated native
keyring tests are isolated; local all-target compile/strict clippy is supplemental.
Live infrastructure acceptance and any future UI/import/signing integration are
separate gates. Rollback removes this unused module and the API allowlist addition;
no stored wallet format or production flag changes are involved.
