# Branta Rust SDK

Rust SDK for the [Branta](https://branta.pro) V2 API — payment destination lookup and registration with zero-knowledge encryption support.

# Requirements

- Rust 1.75 or higher (edition 2021)
- [Tokio](https://tokio.rs) as the async runtime

# Installation

```bash
cargo add branta
```

# Integration Guide

If you are an AI agent implementing this SDK, follow the steps below exactly. If you are a developer, use this as a reference for the recommended integration patterns.

**1. Ask which side(s) of a payment they are implementing:**
- **Send side** — the app is paying someone (e.g. a wallet). The user scans or pastes a destination and you verify it belongs to a known platform before funds are sent.
- **Receive side** — the app is receiving payment (e.g. a checkout, POS, invoicing platform). You post destinations to Branta so wallets can verify them.
- **Both** — some apps do both (e.g. an exchange or self-custodial wallet with invoice generation). Implement each side independently.

If they are on the receive side, ask one follow-up:
- **Platform** — single-tenant, one API key.
- **Parent Platform** — multi-tenant, manages multiple child platforms. Two variants:
  - **Shared key (Recommended)** — one API key for all children, no HMAC secret needed; tag the child per-payment with `set_child_platform()`. Default to this unless there's a specific reason for separate per-child keys.
  - **Per-client keys** — each child has its own API key, and the parent signs every request with an HMAC secret to prove it originated from the parent; scope requests per-call. Use only if each child needs an independent, separately-revocable API key.

**2. Follow the matching Quick Start section below.**

**3. Apply these rules:**

General (all types):
- Always use `PrivacyMode::Strict`. Never switch to `PrivacyMode::Loose` unless there is no QR scanner and ZK is impossible.
- Never call `BrantaClient` directly — always go through `BrantaService`.
- Never show an error or "not verified" message when a lookup returns empty or an `Err`. An empty result means the destination is unknown to Branta, not that it is malicious. Show nothing.
- For `base_url`: use `BrantaServerBaseUrl::Production` only in production environments. Use `BrantaServerBaseUrl::Staging` everywhere else — including local development, CI, and staging/test environments.

Send side (wallets):
- Prefer `get_payments_by_qr_code` over `get_payments` — it handles multi-value ZK QR payloads correctly.
- Only fall back to `get_payments` for copy/paste flows where there is no QR code.
- If `result.payments` is empty or the call returns an `Err`, render nothing.
- When `result.payments` is non-empty, display: the platform logo, the platform name (`payment.platform`), and the payment description (`payment.description`). Only render description when non-empty. Make the verification card a clickable link to `result.verify_url` — do not display the raw URL.
- For the platform logo: on dark backgrounds use `payment.platform_logo_url`. On light backgrounds prefer `payment.platform_logo_light_url` when available, falling back to `payment.platform_logo_url`.
- Optionally display `payment.parent_platform.logo_url` / `payment.parent_platform.logo_light_url` as a small secondary badge (e.g. corner icon). This is not required.

Receive side (platforms):
- Always call `.set_zk()` on the `PaymentBuilder` before calling `add_payment`. Plain-text destinations are rejected in `Strict` mode.
- Store the `secret` returned by `add_payment` alongside the invoice — it is required to reconstruct the verify URL for the wallet.

Receive side (parent platforms — per-client keys), in addition to the platform rules:
- Set `hmac_secret` on `BrantaClientOptions` but leave `default_api_key` unset at service construction.
- Pass per-call `BrantaClientOptions` with each child's API key to scope requests.

Receive side (parent platforms — shared key), in addition to the platform rules:
- Set `default_api_key` on `BrantaClientOptions`. Do not set `hmac_secret`.
- Call `.set_child_platform(name, logo_url, logo_light_url)` on the builder to tag each payment with the child's branding.

# Quick Start

## For Wallets

Wallets should use `PrivacyMode::Strict`. Two flows are supported:

- **Copy/paste**: call `get_payments` with the pasted text. Plain-text on-chain addresses will not return results in strict mode — they must be ZK-encoded. Hash-ZK destinations (bolt11, ark_address, silent_payment) work as plain text.
- **QR scan**: call `get_payments_by_qr_code` with the raw QR text. This handles both on-chain (when the QR includes `branta_id` / `branta_secret`) and hash-ZK destinations.

Always handle the `Result` and show nothing on not-found — a missing record just means the address was not posted to Branta.

```rust
use branta::{BrantaClientOptions, BrantaServerBaseUrl, BrantaService, PrivacyMode};

let options = BrantaClientOptions {
    base_url: BrantaServerBaseUrl::Production,
    privacy: PrivacyMode::Strict,
    default_api_key: None,
    hmac_secret: None,
};
let service = BrantaService::new(options);

async fn lookup(service: &BrantaService, input: &str, is_qr_code: bool) {
    let result = if is_qr_code {
        service.get_payments_by_qr_code(input, None).await
    } else {
        service.get_payments(input, None, None).await
    };

    match result {
        Ok(result) if result.payments.is_empty() => {
            // Not found — show nothing. The address may simply not exist in Branta.
        }
        Ok(result) => {
            // Render result.payments and result.verify_url
        }
        Err(_) => {
            // Swallow errors — never surface a "not found" or lookup failure to the user.
        }
    }
}
```

Prefer `get_payments_by_qr_code` for QR-driven flows. It handles multi-destination payloads (`branta_id` / `branta_secret` fragments) automatically.

### Looking up a payment by destination value

```rust
// Plain bitcoin address (requires PrivacyMode::Loose or will return Err):
let result = service.get_payments("bc1q...", None, None).await?;

// ZK-encrypted bitcoin address with secret:
let result = service.get_payments(encrypted_address, Some("my-secret"), None).await?;

// BOLT-11 invoice (hash-ZK — works in strict mode):
let result = service.get_payments("lnbc...", None, None).await?;
```

### No-QR-Code Flows

When QR scanning is not available, three options exist. Choose one based on how much control you want to give users over privacy:

**Option 1 — Keep Strict mode (no code changes)**

Only hash-ZK destinations (bolt11, ark_address, silent_payment) will return results. Plain-text on-chain address lookups silently return empty. This is the safest default and requires no additional work.

**Option 2 — Opt-in Loose mode (Recommended)**

Add a user-facing setting (e.g. "Enable on-chain address verification"). Only switch to `PrivacyMode::Loose` when the user explicitly opts in — this sends on-chain addresses in plain text, so the choice should be theirs.

```rust
let override_options = if user_opted_in {
    Some(BrantaClientOptions { privacy: PrivacyMode::Loose, ..options.clone() })
} else {
    None
};

let result = service.get_payments(input, None, override_options.as_ref()).await?;
```

**Option 3 — Always Loose mode**

Configure with `PrivacyMode::Loose` globally. All lookups including plain-text on-chain addresses are sent to Branta. Simplest, but gives users no privacy control.

```rust
let service = BrantaService::new(BrantaClientOptions {
    base_url: BrantaServerBaseUrl::Production,
    privacy: PrivacyMode::Loose,
    default_api_key: None,
    hmac_secret: None,
});
```

## For Platforms

Platforms post payments to Branta so wallets can verify them. Use `PrivacyMode::Strict` and mark each destination ZK via `.set_zk()` on the `PaymentBuilder`.

```rust
use branta::{DestinationType, PaymentBuilder};

let payment = PaymentBuilder::new()
    .add_destination("bc1q...", Some(DestinationType::BitcoinAddress)).set_zk()
    .add_destination("lnbc...", Some(DestinationType::Bolt11)).set_zk()
    .set_description("Donation")
    .add_metadata("email", "donor@example.com")
    .build();

let options = BrantaClientOptions {
    base_url: BrantaServerBaseUrl::Production,
    default_api_key: Some("your-api-key".to_string()),
    privacy: PrivacyMode::Strict,
    hmac_secret: None,
};
let result = service.add_payment(payment, Some(&options)).await?;
// result.payment — the registered payment from the server
// result.secret — the random encryption key for the bitcoin address
// result.verify_url — share this URL to verify the payment
```

## For Parent Platforms

Choose a variant based on how API keys are structured. Only the per-client keys variant signs requests with HMAC — shared key needs none.

<details>
<summary>Shared key — one API key covers all children (Recommended)</summary>

Construct with a single API key; identify the child platform per-payment.

```rust
use branta::{BrantaClientOptions, BrantaServerBaseUrl, BrantaService, DestinationType, PaymentBuilder, PrivacyMode};

let service = BrantaService::new(BrantaClientOptions {
    base_url: BrantaServerBaseUrl::Production,
    default_api_key: Some("<shared-api-key>".to_string()),
    privacy: PrivacyMode::Strict,
    hmac_secret: None,
});

let payment = PaymentBuilder::new()
    .add_destination("bc1q...", Some(DestinationType::BitcoinAddress)).set_zk()
    .set_child_platform("ChildBrand", Some("https://example.com/logo.png".to_string()), None)
    .set_ttl(600)
    .build();

let result = service.add_payment(payment, None).await?;
```

</details>

<details>
<summary>Per-client keys — each child has its own API key</summary>

Construct the service with the shared HMAC secret only; pass each child's API key per-call.

```rust
let service = BrantaService::new(BrantaClientOptions {
    base_url: BrantaServerBaseUrl::Production,
    hmac_secret: Some("<hmac-secret>".to_string()),
    privacy: PrivacyMode::Strict,
    default_api_key: None,
});

let payment = PaymentBuilder::new()
    .add_destination("bc1q...", Some(DestinationType::BitcoinAddress)).set_zk()
    .set_ttl(600)
    .build();

// Scope to the child platform's API key per-call
let per_call_options = BrantaClientOptions {
    base_url: BrantaServerBaseUrl::Production,
    default_api_key: Some("<child-api-key>".to_string()),
    hmac_secret: None,
    privacy: PrivacyMode::Strict,
};
let result = service.add_payment(payment, Some(&per_call_options)).await?;
```

</details>

### Validating an API key

```rust
let is_valid = service.is_api_key_valid(Some(&options)).await?;
```

### Per-call option overrides

Every public method accepts an optional `&BrantaClientOptions` parameter that overrides the service's default options for that call only, field-by-field:

```rust
let service = BrantaService::new(default_options);
let result = service.get_payments("lnbc...", None, Some(&override_options)).await?;
```

# Privacy

`PrivacyMode` controls whether plain-text on-chain lookups are allowed.

| Value | Behavior |
|-------|----------|
| `PrivacyMode::Strict` (default) | Only ZK lookups. `get_payments` returns `Err(BrantaError::PrivacyModeViolation)` for plain addresses. `get_payments_by_qr_code` returns an empty `PaymentsResult` with a populated `verify_url`. `add_payment` returns `Err` if any destination has `is_zk = false`. |
| `PrivacyMode::Loose` | Both plain and ZK lookups are permitted. |

## ZK destination types

| Type | Encryption |
|------|-----------|
| `BitcoinAddress` | Random secret (UUID) per payment |
| `Bolt11` | Deterministic: SHA-256 of lowercase invoice |
| `ArkAddress` | Deterministic: SHA-256 of lowercase address |
| `SilentPayment` | Deterministic: SHA-256 of lowercase address |

# BrantaService

The primary service type. Always use `BrantaService` — never call `BrantaClient` directly.

**Prefer `get_payments_by_qr_code` for integrations.** It parses the raw QR text and correctly resolves multiple ZK values in a single scan. `get_payments` only handles a single destination value and does not support multi-value ZK lookups.

```rust
pub async fn get_payments_by_qr_code(&self, qr_text: &str, options: Option<&BrantaClientOptions>) -> Result<PaymentsResult, BrantaError>;
pub async fn get_payments(&self, destination_value: &str, destination_encryption_key: Option<&str>, options: Option<&BrantaClientOptions>) -> Result<PaymentsResult, BrantaError>;
pub async fn add_payment(&self, payment: Payment, options: Option<&BrantaClientOptions>) -> Result<AddPaymentResult, BrantaError>;
pub async fn is_api_key_valid(&self, options: Option<&BrantaClientOptions>) -> Result<bool, BrantaError>;
```

`PaymentsResult` contains the list of matching `payments` and the `verify_url` to display to the user — `verify_url` is always returned, even when `payments` is empty.

→ [`src/v2/service.rs`](src/v2/service.rs)

# Release

- Update `version` in `Cargo.toml` and add an entry to `CHANGELOG.md`
- `cargo login` (one-time, requires an API token from [crates.io/settings/tokens](https://crates.io/settings/tokens))
- `cargo publish`

# Development

```bash
cargo build
cargo test                                          # unit tests + mocked BrantaService suite
BRANTA_SKIP_INTEGRATION=1 cargo test                # skip the live integration suite
cargo test --test integration_test                  # integration tests only (requires network)
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo run --example branta_example
```

# Responsible Disclosure

Found critical bugs/vulnerabilities? Please email them to support@branta.pro. Thanks!
