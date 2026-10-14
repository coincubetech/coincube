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
   The version-10 Claim journal now requires its recorded fee rates to match
   the exact unsigned PSBT fees, and the version-11 Split observation check
   compares the funding coins' branch and index to the saved derivations.
   A bounded paired search now uses one session, deadline and request budget
   for Bitcoin and BTCB2 candidate ancestry plus the shared-input refresh;
   it returns no one-sided result. For foreign Split, scan-derived funding and
   shared coins are rechecked against the source descriptor before the search.
   Selected coins are matched to live paths and confirmation blocks. A saved
   Split deposit reservation now restricts the search to the two exact
   external receive indices shown to the user. The search reloads and compares
   the released reservation under the current Connect identity before
   returning a read-only result from a completed paired scan. The production
   Split panel calls it for a read-only deposit check; it cannot authorize signing.
   Claim now has an equivalent read-only paired search that binds the selected
   Bitcoin and BTCB2 funding outputs and every shared output to its Vault
   descriptor. A Cube-specific entry point to the bounded two-chain scanner
   now walks its known receive/change descriptors, joins scan-time shared and
   chain-specific UTXOs, authenticates every candidate against the Vault and
   feeds that inventory to the paired search. Its history-gap result is not a
   complete wallet accounting: the production route must reconcile the full
   wallet UTXO set and refresh spendability before admission. A review-only
   Claim pair constructor builds the exact ordinary Bitcoin and BTCB2 PSBTs
   from that paired result, current shared wallet coins, previous transactions
   and separately reserved change indices. It refuses a changed shared set or
   output before returning the pair. `ClaimInventory::construct_pair_for_review`
   now supplies previous transactions from the same paired scan, rejecting a
   missing or conflicting transaction before delegating to that constructor.
   The production Claim deposit check invokes paired discovery for read-only
   status, but cannot construct or display the pair until both change
   destinations are durably reserved; it does not admit it to signers.
   The Claim funding journal now has a version-3 record for two distinct
   change indices. A reservation service asks the Bitcoin and BTCB2 wallet
   backends to advance their own change branches, checks their explicit chain
   identities and descriptor, then writes both indices under the Claim lock.
   A partial failure burns any already allocated index. Version-10 Claim
   creation and restart require the exact saved pair. The production wizard
   does not yet invoke this service because it has no authenticated BTCB2
   wallet daemon handle, and the current Connect wallet backend cannot
   atomically reserve change indices; both conditions must be resolved before
   the preview or signing path is enabled.
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
   Both ordinary Split builders now refuse a marker whose value is smaller
   than that chain's computed worst-case transaction fee. Claim's ordinary
   Bitcoin and BTCB2 builders apply the same fee-cover check to their selected
   chain-specific inputs, including when reconstructing recorded transactions.
   A review-only size estimator matches the exact constructed sizes across all
   supported foreign source shapes. The Split deposit screen now rescans the
   saved source and reads current fees on both chains before showing each
   minimum; it refuses the instructions when either quote is unavailable.
   This estimate must be recalculated from actual inputs at review. Claim's
   deposit screen now uses the Vault descriptor's maximum signed size for all
   scanned shared inputs plus one funding input, reads fresh fees on each
   chain, and shows separate minimum deposits. It refuses to release an
   address reservation when either quote is unavailable. The actual fee must
   still be recalculated from the constructed pair at review.
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
   supplied by their owners, and refuses if scan tips have changed. Journal
   creation now also requires the saved pre-deposit record and checks that the
   Bitcoin funding index, BTCB2 funding index and Bitcoin return index match
   its three distinct source addresses and Connect identity. The journal also
   requires the exact BTCB2 target descriptor, receive index and script from
   the saved target reservation; a different destination is refused. It still does
   not grant live proof or signer access.
   The Connect preflight reader now offers a paired operation for finalized
   ordinary transactions. It requires one common input set with exactly one
   distinct funding input on each chain and positive, fresh policy acceptance
   for both exact signed transactions under one Connect generation; one-sided
   acceptance returns no paired evidence. The coordinator still must bind this
   read to both typed live ancestry proofs, wallet reservations and finalized
   signature checks before submission. Keep the unified signature fallback
   separate and labeled with its actual replay properties.
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
   The foreign scan now identifies three distinct receive indices proved unused
   within both chains' covered ranges: Bitcoin deposit, BTCB2 deposit, and
   Bitcoin self-transfer return. These are read-only hints, not reservations;
   the wizard must durably reserve the exact addresses and recheck their
   history before displaying them. A scan with room for only one fresh address
   cannot start the two-deposit flow. An immutable private pre-deposit record
   now persists the source descriptor, target Cube, Connect account/provider
   and those three indices under the Split journal lock. Restart reloads the
   exact same addresses and refuses a changed selection or identity. This
   record alone does not prove freshness or authorize a spend; the wizard still
   needs to recheck both chains' address history before showing it. A read-only
   pre-display check now reloads that record, binds it to the current Connect
   identity, and checks all three addresses on both chains under one timeout;
   any used address or failed read refuses the instructions. The production
   Split's production "Start split" now stores this reservation under the
   source digest, performs all six two-chain address-history reads off the UI
   thread, and shows distinct Bitcoin and BTCB2 deposit addresses only when
   the check succeeds. It then durably marks the same reservation released;
   a later retry with a matching source, target and Connect identity reuses
   those exact addresses after fresh authenticated reads on both chains even
   when deposits have created history, while a
   first display still refuses any already-used address. The old route picker
   is bypassed for fresh production
   splits and cannot open a retired signer path. The paired Split funding
   search now filters out unrelated post-fork outputs at other receive
   indices. The production Split deposit screen now offers a read-only
   "Check deposits" action: it rescans the saved source descriptors on both
   chains, reloads the exact released reservation, reports whether each
   reserved address has a confirmed post-fork candidate, and attempts the
   bounded paired ancestry proof. Once proof succeeds, it builds both ordinary
   PSBTs at current per-chain fee rates and derives a read-only coin,
   destination and fee preview from their exact inputs and outputs. It discards
   the proof and transactions after displaying the preview and cannot open
   Sign. Before the read-only proof search, the deposit check now excludes
   individual coins at either reserved address that are below that chain's
   current estimated fee. This lets a later sufficient deposit be considered
   without rotating the reserved address; the exact builder still enforces
   the fee bound. The check also distinguishes a shared ancestry root,
   reorganization and unavailable fresh reads in its retry guidance.
   Successful Split previews and Claim proof statuses expire after 30 seconds
   in the desktop, with older timers unable to clear newer checks. The user
   must check both deposits again for a current display. This display limit
   does not replace live preflight at signing and broadcast.
   Exact per-chain proof failure states, proof refresh and ordinary
   signer admission remain to be wired. Claim now independently reserves
   two distinct receive indices from its paired Cube scan in a private journal
   bound to both Cubes, the Vault descriptor and the Connect identity. Its
   pre-display check reloads that exact record and requires fresh unused
   address history on both chains for both addresses. A preparation step also
   checks the Bitcoin Vault descriptor and advances its receive index through
   both reserved addresses before returning the deposit instructions, so its
   daemon can discover incoming funds. The production Claim preconditions now
   offer a bounded two-chain Vault scan, persist that reservation and prepare
   the Bitcoin daemon's receive range before displaying the two copyable
   deposit addresses. Stale results and session revocations withdraw them;
   this stage cannot sign. A separate "Check deposits" action now reloads the
   same reservation, reconciles a new paired scan with the full Bitcoin Vault
   UTXO set, and runs the bounded two-chain ancestry collector; it displays
   only a transient status and does not retain spend authority. Its status
   distinguishes candidate presence at the Bitcoin and BTCB2 reserved addresses
   from a successful paired ancestry proof. The read-only Claim check now
   refreshes both fee quotes and filters each reserved address's candidates
   by that chain's current estimated fee, so a later sufficient deposit can
   be considered after a smaller one. An unreleased reservation cannot
   enter discovery. Ordinary transaction admission remains. Claim now durably marks
   its saved reservation released only after both addresses were checked
   unused and the Bitcoin daemon accepted their receive indices. Reopening
   that same Claim reservation checks identity and wallet state again, then
   displays the exact addresses after fresh authenticated history reads even
   if deposits have since made their
   history nonempty; it never chooses replacements on retry.
   Coincubed `getinfo` now reports an explicit chain ID. The desktop accepts
   an authenticated `chain_id` on a Connect wallet record, checks its address
   network, and keeps an omitted field unknown. The Claim deposit probe and
   read-only checks now admit a Connect-backed Bitcoin Vault when its wallet
   record positively identifies the Bitcoin chain, matches the Vault
   descriptor and reports a ready wallet state; they no longer require the old
   embedded-daemon submission adapter. The current remote Connect wallet API
   does not yet supply `chain_id`, so Connect-backed Claim address preparation
   still refuses it. The preparation path now advances a Connect-backed
   Bitcoin Vault's receive branch with durable one-at-a-time new-address
   requests, verifies every returned derivation, then reads back both exact
   reserved addresses and repeats the two-chain unused-history check before
   releasing the journal record. It refuses an
   advance of more than 256 indices in one attempt. Connect must return the
   positive identity
   before those deposits are usable. Creation of a version-10
   paired Claim intent now requires that saved record and checks both funding
   derivations against its exact external indices under the journal lock. The
   paired Claim discovery now searches only funding outputs at the two
   reserved receive indices, so a different post-fork Vault coin cannot
   silently replace a planned deposit. Before that search it reads the
   Bitcoin Vault daemon's unfiltered coin set and compares every current
   pre-fork coin, amount, derivation, address and confirmation height to the
   paired scan. It also requires the reserved Bitcoin funding output to be
   present and unspent in that wallet. A missing coin, incomplete address
   coverage, unsynced wallet or changed spend state refuses review. The Claim
   daemon comparison now uses the explicit `getinfo` chain identity, so an
   external Coincubed daemon can pass the same positive check as an embedded
   one; an older or unbound remote response cannot.
   The Cube launcher now discovers a funding-only Split record, which has no
   transaction intent yet. Reopening under Connect validates its source
   digest, target Cube and account, then repeats both-chain address checks
   before showing the same deposit addresses. A resumed deposit screen can
   run the same read-only "Check deposits" scan.
   New Split deposit records also bind the authenticated target BTCB2 Vault
   ID, descriptor and reserved receive script under the same journal lock.
   Reopening or checking deposits rejects a different loaded target Vault.
   The target daemon must positively identify as BTCB2 when reserving its
   destination; mainnet address encoding alone cannot distinguish the two
   chains. An older Connect wallet response without an authenticated chain
   ID is therefore refused until Connect supplies it.
   The Claim wizard calls paired funding discovery only for read-only deposit
   status; it does not yet call the transaction builder.
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
The Split deposit stage now consumes the paired model only after fresh
two-chain proof, source ownership, bound reservations and construction of
both ordinary PSBTs. Claim still needs its paired preview. The model alone
cannot enable signing.

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
two-chain harness, including BTCB2-first broadcast and one-block and six-block
reorgs of either confirmation. Coordinator restart, stale observation and
marker spend cases remain.
   The foreign Split bridge now builds and finalizes an ordinary `S+B`/`S+F`
   pair from distinct coinbase dependencies. All five supported foreign source
   shapes pass the offline signed-PSBT vectors and the pinned two-chain regtest.
   Core also has typed pair finalizers for both Claim and Split. They require
   the same shared input set, distinct chain-specific funding inputs, ordinary
   outputs and valid signatures on both exact PSBTs before returning a pair.
   The version-10 Claim and version-11 Split journals now expose a review-only
   path that first validates a fresh paired funding collection against the
   exact saved intent and then invokes the corresponding pair finalizer.
   Connect now has separate exact-byte, one-attempt Bitcoin and BTCB2 mainnet
   broadcast transports for either typed pair. Each side has its own
   transaction-bound, expiring gate and uncertain outcome. These transports
   are not called from the wizard: the paired coordinator still has to bind
   fresh two-chain proof, reservations, preflight and explicit approval to
   separate durable submission intents before either gate can be entered.
   Paired Connect preflight also accepts the typed, signature-verified Claim
   or Split pair directly, observes both exact witnesses under one session,
   and refuses non-mainnet pairs; it remains read-only evidence.
   Version-10 Claim and version-11 Split intents can now atomically retain the
   two finalized signed transactions, including each txid and wtxid, before
   any network attempt. Journal validation requires their unsigned forms to
   match both saved templates exactly. Restart restores the bytes for
   reconciliation but leaves the controller unchecked and does not permit
   signing or broadcast. Separate possible-submission records now bind each
   chain's exact txid, wtxid and short-lived Connect preflight before any
   transport call. The initial Bitcoin record requires both signed witnesses
   to pass paired preflight. The later fork record requires a new preflight of
   the exact BTCB2 witness; the Bitcoin transaction may be mined by then.
   Typed Claim and Split preflight readers now make only that BTCB2 request
   after the Bitcoin step and refuse a rejected policy result. They do not
   themselves establish Bitcoin confirmation, live ancestry or spend authority.
   These records cannot be cleared or repeated through the journal; the fork
   record is sequenced after the Bitcoin record. Neither record
   establishes upstream acceptance or unlocks a send. The paired coordinator
   must still require a fresh six-confirmation Bitcoin observation before
   recording the BTCB2 attempt. A read-only recovery collector now checks
   each possible attempt independently against fresh transaction status,
   exact signed witness bytes, canonical block mapping and stable chain tip.
   Absent, stale, mismatched or unavailable observations never permit an
   automatic retry. The Bitcoin Claim wizard now reopens a version-10 journal
   through a separate read-only recovery screen, checks each recorded possible
   send against its own chain, and shows exact transaction IDs and independent
   statuses. It does not re-enable signing or retry an absent transaction.
   The foreign Split wizard now also diverts version-11 journals away from
   the legacy step-one/step-two restart routes. Its read-only recovery screen
   checks the exact saved possible sends independently under the current
   Connect identity and offers no signer, resend or broadcast action. The
   fork-attempt journal now also requires an opaque,
   short-lived Bitcoin six-confirmation result bound to the exact signed
   witness and session; the coordinator must still recollect live two-chain
   ancestry and obtain explicit approval before either send. Acceptance on
   one chain never implies acceptance on the other.
   The initial collector requires the shared and Bitcoin-only inputs to be
   unspent on Bitcoin. That condition becomes false once the exact Bitcoin
   transaction confirms, so the fork stage needs a distinct collector that
   verifies the canonical Bitcoin spend and requires the shared and BTCB2-only
   inputs to remain unspent on BTCB2. Reusing the initial collector would make
   a successful second step impossible.
   A separate read-only collector now requalifies both ancestry roots, checks
   the exact Bitcoin witness in a canonical block at six confirmations,
   requires the Bitcoin funding and shared outputs spent there, and requires
   the BTCB2 funding and shared outputs unspent under stable two-chain tips.
   Its distinct result type cannot enter the initial signer validator. It
   still needs wallet ownership, reservation, finalized fork signature and
   fresh policy checks in the stage-two coordinator.
   The Claim and Split pair finalizers now issue an opaque, controller-bound
   initial proof only after validating the exact signed pair against the saved
   intent and fresh two-chain collection. Recording a possible Bitcoin send
   requires that proof, matching preflight tip hashes, context and witnesses,
   and the original observation-age limit (capped at 30 seconds). A separate
   post-Bitcoin proof is issued only after the stage-two collector agrees with
   the saved witnesses, exact canonical Bitcoin confirmation block, input set
   and current two-chain tips. The six-confirmation token now retains the
   Bitcoin tip under which its block was read. Recording the possible BTCB2
   send requires this new proof, fresh fork policy and the *same* confirmation
   block and Bitcoin tip token, so a changed chain view between collection and
   submission forces a new read.
   Neither proof survives restart or grants transport access by itself.
   These artifacts are cryptographic evidence only; the coordinator still must
   bind them to the saved intent, fresh two-chain proof, reservations and paired
   Connect preflight before either transaction can be submitted.
The latter mines separate post-fork coinbases into the foreign source, tests
both signed transactions against each node, confirms BTCB2 first, and checks
that the BTCB2 transaction cannot replay on Bitcoin after a Bitcoin
confirmation is reorganized out. All five supported source shapes also reject
the opposite transaction after a six-block reorg of either chain.
The Split-only fixture needs the two pinned Knots binaries, not electrs.
Coordinator restart, stale observation and marker spend cases remain.
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
