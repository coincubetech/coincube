//! Driver for disposable regtest fixtures in tests/test_wallet_import.py.
//! Call only while the fixture daemon is stopped. Production chains are refused.
use std::{error::Error, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        return Err(
            "Usage: import-wallet-state CONFIG expand TARGET | CONFIG restore RECEIVE CHANGE"
                .into(),
        );
    }
    let config = coincubed::config::Config::from_file(Some(PathBuf::from(&args[0])))?;
    if config.bitcoin_config.chain != coincube_core::chain::ChainId::Regtest {
        return Err("This test driver only operates on explicit regtest fixtures".into());
    }
    let data = config
        .data_directory()
        .ok_or("Fixture config requires a data directory")?;
    match args[1].as_str() {
        "expand" if args.len() == 3 => {
            coincubed::expand_imported_discovery(&config, &data, args[2].parse()?)?;
        }
        "restore" if args.len() == 4 => {
            coincubed::restore_imported_address_state(
                &config,
                &data,
                args[2].parse()?,
                args[3].parse()?,
            )?;
        }
        _ => return Err("Invalid fixture operation".into()),
    }
    Ok(())
}
