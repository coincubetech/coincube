# BTCB2 split migration: chain-specific inputs

Status: migration in progress. New Claim and two-step Split transactions using
the 90-byte OP_RETURN are disabled. Existing records remain readable for
reconciliation. The replacement input-ancestry flow is not yet live.

## Decision and safety invariant

Replace the 90-byte OP_RETURN with ordinary Bitcoin and BTCB2 inputs whose
ancestry proves they exist on only one chain. A user deposits a small amount of
post-fork Bitcoin and a small amount of post-fork BTCB2 into fresh addresses of
the **source wallet being split**. These deposits pay the respective network
fees; they are not sent to COINCUBE. The app must positively verify each
deposit's chain-specific ancestry, ownership, confirmation depth and current
unspent status. A post-fork timestamp, a missing transaction on the other
indexer or an exchange withdrawal label is not proof.

For every shared pre-fork outpoint `S`, construct two ordinary transactions:

1. On Bitcoin, spend **all** selected shared outpoints `S` together with a
   verified Bitcoin-only input `B` into a fresh address of the same source
   wallet. BTCB2 cannot validate this transaction because `B` is absent there.
2. On BTCB2, spend the **same** shared outpoints `S` together with a verified
   BTCB2-only input `F`. Claim returns the proceeds to a fresh address of its
   paired BTCB2 Vault; Split sends them to a fresh address of the target BTCB2
   Cube. Bitcoin cannot validate this transaction because `F` is absent there.

Both transactions use normal outputs and ordinary `SIGHASH_ALL` PSBTs. Every
selected input must be signed by the wallet's normal threshold. Neither
transaction may be offered for signing until *both* chain-specific inputs are
verified and reserved. This avoids relying on the six-confirmation Bitcoin
step-one observation as the only protection for the BTCB2 spend if Bitcoin
later reorganizes. Keep six confirmations and reorg monitoring for the
user-visible sequence and accounting. A reorg or spent marker invalidates
review and requires rebuilding; it must never silently switch to OP_RETURN or
a replayable spend.

## Existing pieces and required work

1. **Proof and inventory.** Reuse `claim_ancestry`, its bounded coinbase path,
   retained proof and `claim_observation/http/ancestry` for Bitcoin. The
   canonical two-chain coinbase comparison now accepts either chain as the
   dependency root, with an explicit root-chain label. Bounded BTCB2 path
   discovery and proof refresh are wired, but the BTCB2 candidate inventory
   and live spend admission still need to be wired. Generalize
   the authenticated observation collector to BTCB2 without weakening its
   post-fork, BIP34 height, coinbase maturity, canonical block, tip-bracketing
   or current-UTXO checks. At the dependency's root height, read the canonical
   coinbase on **both** chains and require distinct txids; the same-height
   coinbase could otherwise have been copied across the fork. The currently displayed Split
   `bitcoin_only_post_fork` candidates are hints only. Add symmetrical
   BTCB2-only candidates for display (now present) and promote neither to
   proof without the ancestry collector. A paired read-only funding collector
   now refreshes both root paths, selected-transaction confirmations,
   canonical block mappings, maturity, unspent status and two-chain tips under
   one session; it is not yet bound to the source wallet or signing intent.
   The foreign inventory now retains previous transactions for covered,
   post-fork funding candidates. Review-only validators compare those bytes,
   retained paths, session, source constructions and two current tips for
   Split and Claim; Split also binds the candidates' block assignments.
   A further read-only collection now checks every shared input's confirmed
   transaction against the same canonical pre-fork block on both chains and
   requires an explicit current unspent response on each, under the paired
   funding session and stable tips. It also authenticates the shared output
   bytes by txid and checks their scripts and amounts against both PSBTs' UTXO
   metadata. Claim and Split review validators bind
   that exact shared set to both unsigned transactions and their version-10
   Claim or version-11 Split journal. Signer admission still needs to consume
   this fresh evidence with wallet ownership, reservations and a new preflight;
   no candidate or validator result is cached spend authority.
   A bounded paired search now uses one session, deadline and request budget
   for Bitcoin and BTCB2 candidate ancestry plus the shared-input refresh;
   it returns no one-sided result. For foreign Split, scan-derived funding and
   shared coins are rechecked against the source descriptor before the search.
   Selected coins are matched to live paths and confirmation blocks. This is
   still read-only and is not yet invoked by the production Split panel.
2. **Owned Claim transactions.** `create_ancestry_self_transfer` makes a
   one-output Bitcoin self-transfer with `B`.
   `create_ancestry_fork_sweep_with_marker` now constructs and exactly
   reconstructs the ordinary `S + F` fork transaction, requiring the
   designated `F` input. These builders establish structure and wallet
   ownership, not live chain exclusivity. Production discovery and signer
   dispatch remain gated in `claim_ancestry_gate`. Add a separately typed
   BTCB2 live proof and preserve exact unsigned-PSBT and finalized witness
   checks. The version-10 Claim journal now retains both *structural* paths,
   marker outpoints and exact unsigned transactions; paired restore rebuilds
   both against current owned coins while keeping signing and submission
   unavailable. Next bind the live two-chain collector and reservations to
   this intent. On restart, recollect live proofs before sign, broadcast or
   completion. Keep legacy reconstruction and verification only for recorded
   intents and possibly broadcast transactions. Remove them after journal
   migration and reconciliation are complete.
3. **Foreign Split transactions.** The existing `foreign_split::SplitStep1`
   always writes OP_RETURN output 0 and treats every input as a shared claim.
   A separately typed ancestry construction now builds `S + B` with one
   ordinary destination output and an explicit `claimed_prevouts = S`.
   Its BTCB2 counterpart builds `S + F` and authenticates `F` against the
   foreign source descriptor even though its destination is the target Cube.
   Both rebuild exact PSBTs from fresh coins and the persisted fee rate,
   rejecting changed scripts, amounts, input sets, destinations and fees.
   These are structural builders only; the old OP_RETURN types remain for
   recorded legacy transactions. A version-11 Split journal now retains both
   unsigned constructions, funding paths, fee rates and target identity, and
   restart can rebuild both from current source coins. It remains unchecked;
   reconstruction does not restore chain proof, reservations or signer access.
   A read-only paired construction now takes the qualified funding result and
   builds both ordinary PSBTs together, then validates them against the paired
   observation. It requires the exact source receive index and target script
   supplied by their owners, and refuses if scan tips have changed.
   Next update coordinator transport and preflight to carry both typed live
   proofs. Keep the unified signature fallback separate and labeled with its
   actual replay properties.
4. **Both desktop wizards.** Lead with the two deposits and show separate
   Bitcoin and BTCB2 receive addresses, exact chain labels, expected minimum
   funding for each fee and independent confirmation/proof progress. Clearly
   say to send only Bitcoin to the Bitcoin address and only BTCB2 to the BTCB2
   address. Do not reuse an address across chains as evidence of exclusivity.
   After both checks pass, show each step's shared coins, marker input, fee,
   full destination address and signing threshold before the usual signer
   screen. The final review must show the exact destination and broadcast
   action. If a deposit is too small, unconfirmed, spent, entangled, outside
   scanner coverage or cannot be traced to a chain-specific coinbase, explain
   that state without enabling Sign. Do not call the deposit itself a split.
5. **Retire the old route now.** New builds, signing, first submission and
   resubmission of the 90-byte OP_RETURN transaction are disabled before the
   replacement goes live. Next remove `split_poison` payload building,
   `Poison::OpReturn` production selection, the remaining RDTS expiry checks
   that exist only for the 90-byte output, OP_RETURN review details and the transport path for
   newly created claims. Migrate or explicitly refuse any persisted,
   unbroadcast OP_RETURN intent; a possibly broadcast old intent must remain
   recoverable and reconcilable by exact txid. No historical journal is
   silently reinterpreted as ancestry. Update the in-repo workflow and safety
   docs, which currently describe the ancestry path as unsupported.

## Wizard coin review

Use the Send screen's coin-row hierarchy as a visual reference: a compact
label, status badge and right-aligned amount, with the full outpoint available
on expansion. Do **not** reuse its coin-selection checkbox or its "Expires in"
badge. The split builder determines the required inputs; the review is
read-only and must be generated from the exact PSBT inputs being signed.

- Show the shared pre-fork coins together, with a subtotal and a clear
  "Before fork · present on both chains" label. Every shared coin in this
  attempt appears in both transaction reviews.
- Show a Bitcoin funding coin under the Bitcoin transaction and a BTCB2
  funding coin under the BTCB2 transaction. Before proof, label each
  "Post-fork candidate · checking"; only a fresh, complete two-chain proof
  earns "Bitcoin-only · verified" or "BTCB2-only · verified". Amount,
  confirmations and outpoint remain visible. A missing or invalid marker
  keeps Sign unavailable.
- At each signing review, display a compact equation such as "2 shared coins
  + 1 Bitcoin-only coin → your source wallet" (and the corresponding BTCB2
  version with its actual destination), followed by the full address and fee.
  State that these inputs must be spent together to separate the chains.
  There is no "select all" action, unchecked row, or way to omit the marker.
- Keep unrelated post-fork coins outside the transaction review. If useful,
  show a separate "Not part of this split" count, without implying that all
  post-fork coins are automatically safe or that the split covers unrelated
  wallet activity.

The reusable read-only `CoinReview`/`CoinPairReview` model now derives shared
and funding rows, subtotals, destination and fees from the exact unsigned
PSBTs. It refuses missing amounts, duplicate or omitted inputs, mismatched
shared coins across chains (including different scripts at the same amount)
and OP_RETURN outputs. The recorded Claim step-one
review uses its Bitcoin side and labels the marker as an unverified candidate.
The paired Claim and Split wizard stages still need to consume the model only
after fresh two-chain proof, source ownership and reservations are bound; the
model alone cannot enable signing.

Build this as a reusable *read-only* coin summary for Claim and foreign Split,
using the existing amount formatting and badge components. Keep the normal
Send selector unchanged. Test the row mapping against the actual unsigned PSBT
and re-render it after any fee, tip or proof change so stale input labels cannot
survive a rebuilt transaction.

## Acceptance checks before enabling signing

- Core unit tests prove exact `S + B` and `S + F` input sets, one owned output,
  no OP_RETURN, descriptor ownership, fee bounds and ordinary hardware PSBT
  metadata across all supported source shapes. Missing or duplicate markers,
  unproven paths and mixed-chain inputs must refuse.
- A two-chain regtest uses distinct post-fork coinbase ancestors, signs both
  transactions with ordinary ECDSA, accepts each on its intended chain and
  rejects the exact signed bytes on the other chain. Repeat after restart,
  stale observations, marker spend, shallow/deep reorg and step-order changes.

The paired Claim consensus vector passes against the pinned Knots and electrs
two-chain harness, including BTCB2-first broadcast and one-block reorgs of
either confirmation. Coordinator restart, stale observation, marker spend and
deeper reorg cases remain.
The foreign Split bridge now builds and finalizes an ordinary `S+B`/`S+F`
pair from distinct coinbase dependencies. All five supported foreign source
shapes pass the offline signed-PSBT vectors and the pinned two-chain regtest.
The latter mines separate post-fork coinbases into the foreign source, tests
both signed transactions against each node, confirms BTCB2 first, and checks
that the BTCB2 transaction cannot replay on Bitcoin after a Bitcoin
confirmation is reorganized out.
The Split-only fixture needs the two pinned Knots binaries, not electrs.
Coordinator restart, stale observation, marker spend and deeper reorg cases
remain.
- Exercise Claim and Split through their actual GUI intents and signed-PSBT
  round trips. Include BitBox and Ledger integration smoke checks on real
  devices before a mainnet release; software-only tests cannot prove a device
  will display and approve the PSBT.
- Keep the production gate closed until these checks and an independent review
  pass. Then enable discovery, signing and durable completion together. Confirm
  Connect serves the block, transaction, UTXO and tip endpoints needed for
  both chain-specific proof collectors under one account session.

This is a wallet migration, not just a copy change. A user's existing shared
coins should remain untouched until both independent marker proofs are ready.
