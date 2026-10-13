# BTCB2 split migration: chain-specific inputs

Status: implementation plan. The current Claim transaction still uses the
90-byte OP_RETURN output; Split's transaction path is not yet wired into its
desktop wizard. Do not use this document as evidence that either new flow is
live.

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
   retained proof and `claim_observation/http/ancestry` for Bitcoin. Generalize
   the authenticated observation collector to BTCB2 without weakening its
   post-fork, BIP34 height, coinbase maturity, canonical block, tip-bracketing
   or current-UTXO checks. At the dependency's root height, read the canonical
   coinbase on **both** chains and require distinct txids; the same-height
   coinbase could otherwise have been copied across the fork. The currently displayed Split
   `bitcoin_only_post_fork` candidates are hints only. Add symmetrical
   BTCB2-only candidates and promote neither to proof without the ancestry
   collector. Bind both proofs to one account/session, source descriptor,
   source UTXO set and current two-chain tips.
2. **Owned Claim transactions.** `create_ancestry_self_transfer` already makes
   a one-output Bitcoin self-transfer with `B`, but production discovery and
   signer dispatch are gated in `claim_ancestry_gate`. Extend
   `create_ancestry_fork_sweep` to spend `S + F` instead of `S` alone, with a
   separately typed BTCB2 proof. Preserve exact unsigned-PSBT and finalized
   witness checks. Version the Claim journal to retain both *structural* paths
   and marker outpoints, never cached spend authority. On restart, reconstruct
   both transactions and recollect live proofs before sign, broadcast or
   completion. Remove the OP_RETURN constructor and finalizer only when this
   replacement passes the full two-chain suite.
3. **Foreign Split transactions.** The existing `foreign_split::SplitStep1`
   always writes OP_RETURN output 0 and treats every input as a shared claim.
   Replace it with a typed ancestry construction containing `S + B`, one
   ordinary destination output, and an explicit `claimed_prevouts = S`.
   Extend step two to consume `S + F`, authenticating `F` against the foreign
   source descriptor even though its destination is the target Cube. Rebuild
   finalized PSBTs from fresh coins after restart and reject any changed
   script, amount, input set, destination or fee. Update coordinator transport,
   preflight and journal identity to carry both typed proofs. Keep the unified
   signature fallback separate and labeled with its actual replay properties.
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
5. **Remove the old route atomically.** Remove `split_poison` payload building,
   `Poison::OpReturn` production selection, RDTS expiry checks that exist only
   for the 90-byte output, OP_RETURN review details and the transport path for
   newly created claims. Migrate or explicitly refuse any persisted,
   unbroadcast OP_RETURN intent; a possibly broadcast old intent must remain
   recoverable and reconcilable by exact txid. No historical journal is
   silently reinterpreted as ancestry. Update the in-repo workflow and safety
   docs, which currently describe the ancestry path as unsupported.

## Acceptance checks before enabling signing

- Core unit tests prove exact `S + B` and `S + F` input sets, one owned output,
  no OP_RETURN, descriptor ownership, fee bounds and ordinary hardware PSBT
  metadata across all supported source shapes. Missing or duplicate markers,
  unproven paths and mixed-chain inputs must refuse.
- A two-chain regtest uses distinct post-fork coinbase ancestors, signs both
  transactions with ordinary ECDSA, accepts each on its intended chain and
  rejects the exact signed bytes on the other chain. Repeat after restart,
  stale observations, marker spend, shallow/deep reorg and step-order changes.
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
