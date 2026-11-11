//! End-to-end usage demo. Run with:
//!
//! ```sh
//! BRANTA_API_KEY=<staging-api-key> cargo run --example branta_example
//! ```
//!
//! Without `BRANTA_API_KEY`, `verify` still runs. `add_payment` returns `Unauthorized`.

use branta::{BrantaApiOptions, BrantaEnvironment, BrantaService, DestinationType, PaymentBuilder};

#[tokio::main]
async fn main() {
    let mut options = BrantaApiOptions::new(BrantaEnvironment::Staging);
    options.api_key = std::env::var("BRANTA_API_KEY").ok();
    let service = BrantaService::new(options);

    println!("Verify ----------------------------------");
    match service.verify("bc1qexample", None).await {
        Ok(result) => {
            println!("Status: {}", result.status.as_str());
            println!("Verify URL: {}", result.verify_url);
        }
        Err(err) => println!("Verify failed: {err}"),
    }

    println!("Add Payment -----------------------------");
    let payment = PaymentBuilder::new()
        .set_description("Test description")
        .add_metadata("test_key", "test value")
        .set_ttl(4000)
        .add_destination("bc1qexample", Some(DestinationType::BitcoinAddress))
        .build();

    match service.add_payment(&payment, None).await {
        Ok(result) => {
            println!("Secret: {}", result.secret);
            println!("Verify URL: {}", result.verify_url);
        }
        Err(err) => println!("Add payment failed: {err} (needs BRANTA_API_KEY to succeed)"),
    }
}
