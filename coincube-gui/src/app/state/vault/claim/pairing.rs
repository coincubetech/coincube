//! Read-only routing for a paired Claim. These identities select a journal;
//! only the coordinator can authenticate its contents and authorize actions.
use std::path::PathBuf;

use coincube_core::chain::ChainId;

use crate::{
    app::{
        settings::{CubeSettings, Settings, WalletId},
        wallet::Wallet,
    },
    dir::CoincubeDirectory,
};

#[derive(Debug)]
pub struct Pairing {
    bitcoin_cube: String,
    fork_cube: String,
    bitcoin_wallet: WalletId,
}

impl Pairing {
    /// Resolve from both chain-local settings files on every entry. Never use
    /// the fork wallet's installation timestamp to locate the Bitcoin journal.
    pub fn read(
        root: &CoincubeDirectory,
        bitcoin_cube: &str,
        fork_cube: &str,
        wallet: &Wallet,
    ) -> Result<Self, String> {
        if wallet.chain != ChainId::BitcoinBlake2b {
            return Err("Open the Bitcoin Blake2b Cube to continue this Claim.".into());
        }
        if wallet.descriptor_checksum
            != WalletId::generate(&wallet.main_descriptor).descriptor_checksum
        {
            return Err("The open Vault's storage identity does not match its descriptor.".into());
        }
        let bitcoin = Settings::from_file(&root.network_directory(ChainId::Bitcoin))
            .map_err(|e| format!("Couldn't read the Bitcoin Cube settings: {e}"))?;
        let fork = Settings::from_file(&root.network_directory(ChainId::BitcoinBlake2b))
            .map_err(|e| format!("Couldn't read the Bitcoin Blake2b Cube settings: {e}"))?;
        Self::bind(
            &bitcoin.cubes,
            &fork.cubes,
            bitcoin_cube,
            fork_cube,
            &wallet.id(),
            &wallet.id_fingerprint().to_string(),
        )
    }

    fn bind(
        bitcoin: &[CubeSettings],
        fork: &[CubeSettings],
        bitcoin_cube: &str,
        fork_cube: &str,
        fork_wallet: &WalletId,
        fingerprint: &str,
    ) -> Result<Self, String> {
        if bitcoin_cube.is_empty() || fork_cube.is_empty() || bitcoin_cube == fork_cube {
            return Err("The Claim must name two distinct Cubes.".into());
        }
        let source = matching_cube(bitcoin, bitcoin_cube, ChainId::Bitcoin)?;
        let target = matching_cube(fork, fork_cube, ChainId::BitcoinBlake2b)?;
        let source_wallet = source
            .vault_wallet_id
            .as_ref()
            .ok_or_else(|| "The Bitcoin Cube has no Vault.".to_string())?;
        if target.vault_wallet_id.as_ref() != Some(fork_wallet)
            || source_wallet.descriptor_checksum != fork_wallet.descriptor_checksum
            || source.vault_fingerprint.as_deref() != Some(fingerprint)
            || target.vault_fingerprint.as_deref() != Some(fingerprint)
        {
            return Err("The Claim's paired Vault identities changed. Reopen the Bitcoin Cube to check them.".into());
        }
        Ok(Self {
            bitcoin_cube: bitcoin_cube.into(),
            fork_cube: fork_cube.into(),
            bitcoin_wallet: source_wallet.clone(),
        })
    }

    pub fn bitcoin_cube(&self) -> &str {
        &self.bitcoin_cube
    }
    pub fn fork_cube(&self) -> &str {
        &self.fork_cube
    }
    pub fn journal_directory(&self, root: &CoincubeDirectory) -> PathBuf {
        root.network_directory(ChainId::Bitcoin)
            .coincubed_data_directory(&self.bitcoin_wallet)
            .path()
            .join("claim")
    }
}

fn matching_cube<'a>(
    cubes: &'a [CubeSettings],
    id: &str,
    chain: ChainId,
) -> Result<&'a CubeSettings, String> {
    let mut matches = cubes.iter().filter(|cube| cube.id == id);
    let cube = matches
        .next()
        .ok_or_else(|| "A paired Claim Cube is missing.".to_string())?;
    if matches.next().is_some() || cube.network != chain {
        return Err("A paired Claim Cube has an ambiguous or wrong-chain identity.".into());
    }
    Ok(cube)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::settings::VaultIdentity;

    fn cube(id: &str, chain: ChainId, timestamp: i64) -> CubeSettings {
        CubeSettings::new_with_raw_id(id.into(), id.into(), chain).with_vault(VaultIdentity {
            wallet_id: WalletId::new("checksum".into(), Some(timestamp)),
            fingerprint: Some("12345678".into()),
        })
    }

    #[test]
    fn pairing_uses_exact_source_storage_id_not_fork_timestamp_or_checksum_lookup() {
        let source = cube("source", ChainId::Bitcoin, 11);
        let other = cube("other", ChainId::Bitcoin, 99);
        let target = cube("target", ChainId::BitcoinBlake2b, 22);
        let pair = Pairing::bind(
            &[other, source.clone()],
            std::slice::from_ref(&target),
            "source",
            "target",
            target.vault_wallet_id.as_ref().unwrap(),
            "12345678",
        )
        .unwrap();
        let root = CoincubeDirectory::new(std::path::PathBuf::from("/synthetic-claim-test"));
        assert_eq!(
            pair.journal_directory(&root),
            root.network_directory(ChainId::Bitcoin)
                .coincubed_data_directory(source.vault_wallet_id.as_ref().unwrap())
                .path()
                .join("claim")
        );
        assert_ne!(
            pair.journal_directory(&root),
            root.network_directory(ChainId::Bitcoin)
                .coincubed_data_directory(target.vault_wallet_id.as_ref().unwrap())
                .path()
                .join("claim")
        );
        assert_eq!(pair.bitcoin_cube(), "source");
        assert_eq!(pair.fork_cube(), "target");
    }

    #[test]
    fn pairing_refuses_missing_duplicate_wrong_chain_and_replaced_vaults() {
        let source = cube("source", ChainId::Bitcoin, 11);
        let target = cube("target", ChainId::BitcoinBlake2b, 22);
        let wallet = target.vault_wallet_id.clone().unwrap();
        let bind = |sources: &[CubeSettings], targets: &[CubeSettings]| {
            Pairing::bind(sources, targets, "source", "target", &wallet, "12345678")
        };
        assert!(bind(&[], std::slice::from_ref(&target)).is_err());
        assert!(bind(
            &[source.clone(), source.clone()],
            std::slice::from_ref(&target)
        )
        .is_err());
        assert!(bind(
            std::slice::from_ref(&source),
            &[target.clone(), target.clone()]
        )
        .is_err());
        let mut bad = source.clone();
        bad.network = ChainId::BitcoinBlake2b;
        assert!(bind(&[bad], std::slice::from_ref(&target)).is_err());
        let mut bad = source.clone();
        bad.vault_fingerprint = Some("87654321".into());
        assert!(bind(&[bad], std::slice::from_ref(&target)).is_err());
        let mut bad = source.clone();
        bad.vault_wallet_id.as_mut().unwrap().descriptor_checksum = "other".into();
        assert!(bind(&[bad], std::slice::from_ref(&target)).is_err());
        let mut bad = target;
        bad.vault_wallet_id.as_mut().unwrap().timestamp = Some(23);
        assert!(bind(&[source], &[bad]).is_err());
    }
}
