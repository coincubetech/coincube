//! End-to-end usage demo. Run with:
//!
//! ```sh
//! BRANTA_API_KEY=<staging-api-key> cargo run --example branta_example
//! ```
//!
//! Without `BRANTA_API_KEY` set, the read-only lookups still run (against staging), but the
//! `add_payment` call at the end will fail with `BrantaError::Unauthorized`.

use branta::{
    BrantaClientOptions, BrantaServerBaseUrl, BrantaService, PaymentBuilder, PrivacyMode,
};

#[tokio::main]
async fn main() {
    let api_key = std::env::var("BRANTA_API_KEY").ok();

    let options = BrantaClientOptions {
        base_url: BrantaServerBaseUrl::Staging,
        default_api_key: api_key,
        hmac_secret: None,
        privacy: PrivacyMode::Loose,
    };

    let service = BrantaService::new(options);

    println!("Get Payments ----------------------------");
    match service.get_payments("address1", None, None).await {
        Ok(result) => {
            for payment in &result.payments {
                println!("Payment: {payment:#?}");
            }
            println!("Verify URL: {}", result.verify_url);
        }
        Err(e) => println!("Lookup failed: {e}"),
    }

    println!("Get ZK Payments -------------------------");
    let zk_address =
        "pQerSFV+fievHP+guYoGJjx1CzFFrYWHAgWrLhn5473Z19M6+WMScLd1hsk808AEF/x+GpZKmNacFBf5BbQ==";
    match service.get_payments(zk_address, Some("1234"), None).await {
        Ok(result) => {
            for payment in &result.payments {
                println!("Payment: {payment:#?}");
            }
        }
        Err(e) => println!("ZK lookup failed: {e}"),
    }

    println!("Get Payments By QR Code ------------------");
    match service
        .get_payments_by_qr_code("bitcoin:address1", None)
        .await
    {
        Ok(result) => println!("Verify URL: {}", result.verify_url),
        Err(e) => println!("QR lookup failed: {e}"),
    }

    println!("Add Payment ------------------------------");
    let payment = PaymentBuilder::new()
        .set_description("Test description")
        .add_metadata("test_key", "test value")
        .set_ttl(4000)
        .add_destination("address2", None)
        .build();

    match service.add_payment(payment, None).await {
        Ok(result) => {
            println!("Payment: {:#?}", result.payment);
            println!("Secret: {}", result.secret);
            println!("Verify URL: {}", result.verify_url);
        }
        Err(e) => println!("Add payment failed: {e} (needs BRANTA_API_KEY to succeed)"),
    }
}
