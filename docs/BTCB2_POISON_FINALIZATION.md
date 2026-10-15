# Legacy poison finalization and ordinary ancestry pairs

`claim_finalize::finalize_poison_transfer` accepts the opaque
`PoisonSelfTransfer` from the owned native-P2WSH builder and a signed PSBT. It
returns `VerifiedPoisonTransfer`, with private fields and read-only transaction,
construction txid, chain label, descriptor, fee, final vsize and retained signature
counts. It has no broadcast, Claim authorization or deserialization entry point.
It remains for existing OP_RETURN records; new Claim transactions use
`finalize_ancestry_claim_pair`, and foreign Split uses
`foreign_split::finalize_ancestry_split_pair`. Both pair finalizers require
ordinary outputs, the same shared inputs plus one different chain-specific
funding input per chain, and valid standard signatures on both exact PSBTs.

This boundary deliberately accepts only partial-signature PSBTs. Signers may
add standard Bitcoin `SIGHASH_ALL` partial signatures and optionally request
`SIGHASH_ALL`; every other unsigned transaction byte and PSBT metadata field must
match the construction. Full previous transactions, witness scripts, derivation
origins and output metadata cannot be removed or replaced. Prefinalized PSBTs,
unified proprietary signatures, unknown additions and other sighash types refuse.
A signer that strips metadata must have its signatures merged into the exact
original construction before calling this API; this function does not guess or
repair ownership metadata.

Each supplied signature, including surplus signatures not selected for the final
witness, must belong to an original derivation key and verify over the standard
BIP143 SIGHASH_ALL digest with authenticated previous-output amounts. Existing
spend economic checks run before finalization. The pinned Miniscript
`PsbtExt::finalize_mut` constructs the satisfaction and `extract` runs its
interpreter checks. The final transaction is then checked against the original
unsigned transaction and walked with `Interpreter::iter` using authenticated
prevouts. Every actual signature constraint must be owned-key ECDSA SIGHASH_ALL;
empty, malformed, tampered and unified witnesses fail. No new script interpreter
is implemented here.

The legacy result proves cryptographic construction/final-witness integrity only. The
chain field is the bound construction's requested Bitcoin/mainnet or testnet4
identity, not independent chain inclusion evidence. Timelocks are checked against
transaction fields by Miniscript, not against live coin maturity. When the
legacy route was active, its 90-byte poison also required authenticated RDTS
activity, expiry margin, mempool acceptance and correct-chain routing before
broadcast. Historical attempts are now reconciled by their exact transaction
identity and fresh chain reads. Bitcoin confirmation depth and reorg checks
remain mandatory before a recorded step 2. A missing other-chain
transaction or post-fork timestamp is never proof of exclusivity. The new
ordinary pair must instead pass fresh, bounded two-chain ancestry and current
UTXO checks, wallet reservations, paired policy preflight, explicit review and
durable attempt journaling. Production signing remains gated.

Synthetic full-core tests cover primary and recovery satisfactions on both
Bitcoin chain labels, exact construction/metadata mutations, missing quorum,
invalid surplus signatures, non-ALL/unified requests, prefinalized input refusal
and tampered actual final witnesses. Run full `cargo test -p coincube-core`,
`cargo clippy -p coincube-core --all-targets -- -D warnings`, and formatting under
the repository pin. No seed persistence, host keyring or live requests occur in
these tests. Subsequent integration and live acceptance remain separate gates.
