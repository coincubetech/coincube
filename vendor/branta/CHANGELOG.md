# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
