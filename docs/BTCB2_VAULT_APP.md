# Vault-only BTCB2 app boundary

`App::new_for_chain` accepts the exact-chain cache/wallet/Cube settings, an
optional Cube encryption key derived directly during unlock, the current
Connect client, and an already admitted embedded daemon. It has no Breez or
Spark argument. Chain mismatch, absent authentication, a generic provider,
fallback endpoint or local node configuration is refused.

The fork app constructs no Liquid backend or Liquid panels, no Spark client,
and no marketplace panel. Its wallet registry has no Lightning route. The
sidebar receives ChainId and hides unsupported fork routes; direct events
cannot select those routes. Bitcoin constructors retain their existing clients.
Storage, rescan markers and device transport keys use the Cube's ChainId.
A watch-only Cube may lack a Cube encryption key; encrypted signing remains
unavailable until the necessary signer key exists.

The current recovery-alert heartbeat contract only accepts Bitcoin-family IDs,
so fork heartbeats are withheld. This is an explicit unsupported feature, not
a mainnet projection.

App session replacement/logout revokes the current authority, drops its retained
client and encryption key, and clears fiat data. On a Connect-served Cube,
global Home authentication changes must invoke the same invalidation through
every open tab and pending unlock/Loader. A local Cube's tab follows the
account as a Bitcoin tab does (below). Reopening uses a new authenticated context. Generic backend
switches cannot reuse fork authority and instruct the user to reopen instead.
Persisted daemon configuration is sanitized: no bearer token belongs in it.
The Connect session itself is saved as a Bitcoin Cube saves its own, in the
chain folder's `connect.json` (`bitcoin-blake2b/` or
`bitcoin-blake2b-testnet4/`, never `bitcoin/`); see "Connect signer
bootstrap" below.

Anchor freshness is measured on this desktop's clock. When Connect sends
`served_at`, the client shifts `observed_at` by the skew between that stamp
and the time the request was sent; a response without `served_at` still
requires synchronized clocks. A translated stamp in the future, or older than
90 seconds, is stale. There is no added skew allowance, and the shift does not
extend freshness. The stale-evidence message still tells the operator to check
both clocks and retry. The status/anchor service's shared per-IP rate budget
remains authoritative; rate limits do not trigger unbounded retries.

This boundary alone does not activate the runtime gate. Authenticated unlock,
create/reopen workflow evidence, exact-head review and dedicated live indexer
acceptance are separate launch gates. Claim/Split and unsupported external
signers remain gated. Wi-Fi (LAN) pairing and signing follow the chain-bound v3
pairing rules (`docs/BTCB2_SIGNING_IDENTITY.md`), as on Bitcoin.

The admitted client is installed into both Connect account and Cube consumers
before the initialization task runs. Its bootstrap loads user information from
that exact endpoint/token and never restores a saved keyring session. When it
saves its session to the chain folder it reads the saved sign-in only to
reuse its refresh token, and only when the saved sign-in is that same session. Late user
responses are generation-checked. Both generic App constructors return a typed
refusal for fork Cube settings; a seed-only fork App cannot be constructed.

A failed initial user bootstrap revokes authority immediately, even before the
account reaches its dashboard. Explicit transient retries retain the admitted
endpoint/token. Revocation also clears account/Cube consumers, not only the
backend's authority. The admitted in-memory session never owns a separate saved
keyring session, so its refusal does not delete unrelated saved credentials.
Unit-test account secrets use thread-local memory; direct legacy test keyring
consumers use a separate unit-test namespace.

## Connect signer bootstrap

The signer pipeline is the Bitcoin one, carrying the Cube's `ChainId` and its
Connect endpoint: it registers this computer as a signer device with the
chain folder's transport key, opens the realtime stream, and hands the Mobile
signing window its gRPC address, tokens and device id. What differs is the
kind of Cube, recorded once when the App is built (`Cache::connect_served`).

**Connect-served** (a Vault on a Connect backend; the daemon is bound to the
session it was opened with). Once its admitted panel reaches its dashboard,
the App writes that session to the chain folder's `connect.json` with the
guarded writer a keyring restore uses (`persist_restored_tokens`): exactly
the Cube's access token, the saved sign-in's refresh token when that is the
same session and otherwise an empty one, which this Cube never sends. It
then runs the Bitcoin login completion (`connect_login_completed`). The
admitted panel is never sent the login messages. A write the guarded writer
refuses leaves the Cube open; the signer list says Mobile signing could not
start and offers to try again.

The file is shared with any other Cube of the account and can move on while
this one is open. So every pipeline entry (`ensure_connect_ready`,
`trigger_connect_stream_ready`, `connect_stream_ready`) takes the access
token from the Cube's own client and refuses any other, compared by
`connect_session::same_session`; nothing adopts or overwrites a different
token found later. The device id is the one stored for the account; if none
is stored and the registration's write is refused, the id registration
returned is used for this session only and dropped with it. Such an id
cannot be renewed in place (renewal claims the id stored in the file), so if
Connect later stops recognising it the Cube shows "Couldn't reconnect this
computer" until it is reopened. A sign-in started
inside the Cube stays refused: `InAppConnectAuthReady`,
`InAppConnectAuthPersisted` and `InAppConnectLoginCompleted` are refused
whatever sends them, and a deferred commit is answered with
`AuthCommitRejected`. When the stream reports the session ended, the Cube is
torn down with the reopen notice instead of retrying; one promoted to its
managed node loses its Connect services only. A Cube opened Connect-served
keeps that kind for as long as it is open.

**Local** (no Vault, or a managed local node). It runs the Bitcoin path,
unchanged: its panel's sign-in commits through the App into the chain
folder's `connect.json` (deferred commit, `InAppConnectAuthReady`), the
commit check finds it there, and `connect_login_completed` registers the
signer device and starts the stream. Nothing is refused for it. With a Vault
it restores the saved session at open as a Bitcoin Cube does
(`gui::tab::saved_connect_session`, shared with `finish_loader_wallets`);
without one it restores nothing from the file and becomes ready through the
session check, the keyring refresh and the login commit, as a Vault-less
Bitcoin Cube does. One opened signed in keeps that session in its panel and
brings the pipeline up from it at open, as a Connect-served Cube does but
without the pin; once that session ends, its panel signs in through the
Bitcoin path. As on Bitcoin, signing out does not erase `connect.json`, and a
sign-in or sign-out in another tab does not sign it out or close it
(`App::on_global_auth_change`, as on a Bitcoin tab); a revoked feature still
closes it. Home's session is not adopted (`can_adopt_shared_connect_session`
still excludes Bitcoin Blake2b); the Cube runs its own keyring refresh.

A Cube opened Connect-served keeps that kind when promoted to its managed
node: it keeps its admitted panel, and loses its Connect services with the
reopen notice when its session ends or the account changes.

## Lost authorization

Once an admitted Cube loses authorization (401, logout, or account/token change),
its account panel requires closing and reopening the Cube. It shows no OTP/login
form and ignores delayed session results. Replacement login messages are refused
before saving credentials; only a new authenticated startup can install another
admitted client. Bitcoin's ordinary account login flow is unchanged.
