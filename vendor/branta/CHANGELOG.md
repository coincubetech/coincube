# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [4.0.0] - 2026-11-02

### Changed
- **Breaking:** `verify` replaces `get_payments` and `get_payments_by_qr_code`. It returns `match`, `unknown`, `tampered`, or `incomplete`. A match includes payments and a verify URL. The other statuses include only the verify URL. A swapped address is `tampered`. A 404 is `unknown`. A 5xx, a 429, or no network is `incomplete`.
- **Breaking:** `DestinationType` is `bitcoin_address`, `bolt11`, `ark_address`, and `silent_payment`. `bolt12`, LNURL, lightning addresses, and tether are not destination types. A BIP21 extra this SDK cannot encrypt makes `verify` return `unknown`.
- **Breaking:** `PrivacyMode`, `BrantaClientOptions`, `BrantaServerBaseUrl`, `PaymentsResult`, and `PaymentBuilder::set_zk` are removed. Configure `BrantaApiOptions` with `environment`, `api_key`, and `hmac_secret`. A missing environment uses staging.
- **Breaking:** `add_payment` encrypts a copy of every destination and leaves the caller's `Payment` plaintext. Bitcoin addresses use a caller-supplied UUID secret. bolt11, Ark, and silent payments use a key derived from the normalized value. Description is stored inside encrypted metadata. An unset TTL is omitted so Guardrail applies the 7-day default.
- The public crate root exports `BrantaService`, `PaymentBuilder`, `BrantaApiOptions`, `BrantaEnvironment`, `DestinationType`, `VerifyStatus`, `VerifyResult`, and `AddPaymentResult`.

## [3.2.4] - 2026-10-29

### Security
- `get_payments_by_qr_code` now requires every payment instruction in a QR code to match a decrypted destination before returning a payment. A swapped address with a rewritten `branta_secret`, an added `lightning`/`ark`/`sp` leg, or a conflicting `bc=`/`tb=` no longer comes back as verified. Conflicting on-chain addresses (path, `bc=`, `tb=`) return `Err(BrantaError::Tampered)`. `bolt12`, `lno=`, `pay=`, and LNURL instructions cannot be bound and return no payments without a lookup. `QrParser` gains `has_unverifiable_instruction` and now parses `bc`, `tb`, `sp`, and `lno` params (ported from `branta-js` #78).

## [3.2.3] - 2026-10-28

### Added
- Recognize signet and regtest Ark addresses (`tark1`) as `ark_address`, alongside mainnet `ark1`. Strict-mode hash-ZK lookup and registration treat them the same way as `ark1`.

## [3.2.2] - 2026-09-30

### Changed
- **Breaking:** `BrantaError::LogoUrlDomainMismatch` changed from a unit variant to `LogoUrlDomainMismatch(String)`, carrying the name of the field that failed the origin check (e.g. `platform_logo_url`, `parent_platform.logo_url`). Code matching `BrantaError::LogoUrlDomainMismatch` must change to `BrantaError::LogoUrlDomainMismatch(_)` (or bind the field name). Released as a patch version alongside the rest of the sextet; despite being source-breaking for exhaustive matches, treat this as a breaking change when upgrading.

### Fixed
- Fixed a same-origin validation gap: if the first payment in a `get_payments` response had no `platform_logo_url`, validation silently stopped checking every later payment's logo URLs entirely. `BrantaClient::verify_logo_urls` now runs independently per payment, and now also checks `platform_logo_light_url` and `parent_platform`/`child_platform`'s `logo_url`/`logo_light_url` against the configured base URL's origin, not just `platform_logo_url`.

## [3.2.1] - 2026-08-29

### Fixed
- `get_payments_by_qr_code` now verifies that the plaintext Bitcoin address parsed from a scanned QR code matches the address decrypted via `branta_id`/`branta_secret`, returning `Err(BrantaError::Tampered)` on mismatch. Closes a gap where an attacker could swap the visible address in a `bitcoin:` URI while leaving a legitimate, verified `branta_id`/`branta_secret` pair untouched (ported from `branta-js` 3.2.1). This is the one deliberate exception to `decrypt_destinations`'s otherwise-total swallow-all-decrypt-errors rule.

## [3.2.0] - 2026-07-25

### Added
- Initial release of the Branta Rust SDK.
- Feature-parity port of `branta-dotnet` 3.2.0 (and `branta-js`, `branta-dart`, `branta-python`, `branta-kotlin`).
- `BrantaService` with `get_payments`, `get_payments_by_qr_code`, `add_payment`, and `is_api_key_valid`.
- `PaymentBuilder` fluent builder with ZK support, metadata encryption, and child platform tagging.
- `QrParser` handles `bitcoin:`/`lightning:` URIs and plain-text values, with full query-string decoding.
- AES-256-GCM encryption with deterministic and random nonce modes.
- Zero-knowledge (ZK) destination support for Bitcoin addresses, BOLT-11, Ark, and silent payments.
- Metadata DEK-envelope encryption.
- `PrivacyMode::Strict` (default) and `PrivacyMode::Loose` enforcement.
- HMAC-SHA256 request signing support for parent platform flows.
- Full unit test coverage via mocked client/AES/secret-generator dependencies (`mockall`, `wiremock`).
- Integration tests against staging and production, reusing the same example QR-code fixtures as `branta-python`.
