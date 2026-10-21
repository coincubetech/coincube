//! Public source selection for an imported-wallet claim. This is never scan
//! evidence or signing authority; the shared Split flow must obtain both.
use coincubed::config::Config;
use sha2::{Digest, Sha256};

use crate::{
    app::settings, chain::ChainId, dir::CoincubeDirectory, services::coincube::CoincubeClient,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedClaimSource {
    pub cube_id: String,
    pub name: String,
    pub external: String,
    pub internal: String,
    /// Full attested history/allocation window, exclusive, on each branch.
    pub address_window: u32,
}

#[derive(Debug, Clone)]
pub struct ImportedClaimRequest {
    pub source: ImportedClaimSource,
    session_binding: [u8; 32],
}

impl ImportedClaimRequest {
    pub fn new(source: ImportedClaimSource, client: &CoincubeClient) -> Result<Self, String> {
        if source.cube_id.is_empty() || !(1..=10_000).contains(&source.address_window) {
            return Err("Refresh this imported wallet before starting a claim.".into());
        }
        Ok(Self {
            source,
            session_binding: bind(client)?,
        })
    }

    /// Recheck the saved attachment and accepted Connect session on resume,
    /// scan and handoff. A replaced/deleted source cannot reuse this selection.
    pub fn validate(
        &self,
        root: &CoincubeDirectory,
        client: &CoincubeClient,
    ) -> Result<(), String> {
        if self.session_binding != bind(client)? {
            return Err(
                "The Connect session changed. Open Claim BTCB2 from your imported wallet again."
                    .into(),
            );
        }
        let saved = settings::Settings::from_file(&root.network_directory(ChainId::Bitcoin))
            .map_err(|_| {
                "The imported wallet is no longer available. Reopen it before claiming.".to_string()
            })?;
        let cube = saved
            .cubes
            .iter()
            .find(|cube| cube.id == self.source.cube_id)
            .ok_or_else(|| {
                "The imported wallet is no longer available. Reopen it before claiming.".to_string()
            })?;
        self.validate_cube(cube)?;
        let attachment = cube
            .bitcoin_import
            .as_deref()
            .and_then(settings::BitcoinImport::active_attachment)
            .ok_or_else(|| {
                "The imported wallet changed. Open Claim BTCB2 from that wallet again.".to_string()
            })?;
        let data = root
            .network_directory(ChainId::Bitcoin)
            .coincubed_data_directory(&attachment.wallet_id);
        let config = Config::from_file(Some(data.path().join("daemon.toml"))).map_err(|_| {
            "The imported wallet's saved scan is unavailable. Reopen it before claiming."
                .to_string()
        })?;
        if config.bitcoin_config.chain != ChainId::Bitcoin
            || config.main_descriptor.as_imported() != Some(&attachment.descriptor)
            || config.import_birthday != Some(attachment.birthday)
            || config.import_initial_lookahead != Some(attachment.initial_lookahead)
            || config.data_directory().as_ref().map(|dir| dir.path()) != Some(data.path())
        {
            return Err("The imported wallet's saved scan does not match this Cube. Reopen it before claiming.".into());
        }
        let state = coincubed::read_imported_claim_state(&config, &data)
            .map_err(|_| "The imported wallet's saved scan is unavailable or changed. Reopen it before claiming.".to_string())?;
        let required = state
            .receive_index
            .checked_add(1)
            .and_then(|receive| {
                state
                    .change_index
                    .checked_add(1)
                    .map(|change| receive.max(change))
            })
            .ok_or_else(|| "The imported wallet's address range is out of bounds.".to_string())?
            .max(state.discovery_target)
            .max(state.receive_scanned)
            .max(state.change_scanned);
        if !state.history_complete
            || state.receive_scanned < state.discovery_target
            || state.change_scanned < state.discovery_target
            || required > self.source.address_window
        {
            return Err("The imported wallet's saved address window changed. Open Claim BTCB2 from that wallet again.".into());
        }
        Ok(())
    }

    fn validate_cube(&self, cube: &settings::CubeSettings) -> Result<(), String> {
        let changed =
            || "The imported wallet changed. Open Claim BTCB2 from that wallet again.".to_string();
        let import = cube.bitcoin_import.as_ref().ok_or_else(changed)?;
        let attachment = import.active_attachment().ok_or_else(changed)?;
        attachment.validate(ChainId::Bitcoin)?;
        if cube.id != self.source.cube_id
            || cube.network != ChainId::Bitcoin
            || !import.is_ready()
            || self.source.external != attachment.descriptor.receive_descriptor().to_string()
            || self.source.internal != attachment.descriptor.change_descriptor().to_string()
            || !(attachment.initial_lookahead..=10_000).contains(&self.source.address_window)
        {
            return Err(changed());
        }
        Ok(())
    }
}

fn bind(client: &CoincubeClient) -> Result<[u8; 32], String> {
    let token = client
        .token()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "Sign in to Connect before starting a claim.".to_string())?;
    let mut digest = Sha256::new();
    digest.update((client.base_url.len() as u64).to_be_bytes());
    digest.update(client.base_url.as_bytes());
    digest.update((token.len() as u64).to_be_bytes());
    digest.update(token.as_bytes());
    Ok(digest.finalize().into())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use coincube_core::wallet_descriptor::{ImportedWalletDescriptor, WalletDescriptor};
    use coincubed::config::{BitcoinBackend, BitcoinConfig, EsploraConfig};
    use coincubed::datadir::DataDirectory;
    use std::time::Duration;

    /// Create only synthetic, fully scanned imported storage under a test root.
    /// Callers save their Cube's settings separately.
    pub(crate) fn prepare_storage(root: &CoincubeDirectory, cube: &settings::CubeSettings) {
        let wallet = cube
            .bitcoin_import
            .as_ref()
            .unwrap()
            .active_attachment()
            .unwrap();
        let data = root
            .network_directory(ChainId::Bitcoin)
            .coincubed_data_directory(&wallet.wallet_id);
        let staged = DataDirectory::new(data.path().with_extension("test-staging"));
        std::fs::create_dir_all(data.path().parent().unwrap()).unwrap();
        let mut config = Config::new(
            BitcoinConfig::new(ChainId::Bitcoin, Duration::from_secs(30)),
            Some(BitcoinBackend::Esplora(EsploraConfig {
                addr: "http://localhost:18184".into(),
                token: None,
                fallback_addr: None,
                fallback_token: None,
                secondary_fallback_addr: None,
                secondary_fallback_token: None,
            })),
            log::LevelFilter::Info,
            WalletDescriptor::Imported(wallet.descriptor.clone()),
            DataDirectory::new(data.path().to_path_buf()),
        );
        config.import_birthday = Some(wallet.birthday);
        config.import_initial_lookahead = Some(wallet.initial_lookahead);
        coincubed::create_imported_storage(
            &config,
            &staged,
            wallet.birthday,
            wallet.initial_lookahead,
        )
        .unwrap();
        std::fs::rename(staged.path(), data.path()).unwrap();
        coincubed::complete_imported_history_for_test(
            &config,
            &data,
            wallet.initial_lookahead,
            wallet.initial_lookahead,
        )
        .unwrap();
    }

    pub(crate) fn fixture() -> (settings::CubeSettings, ImportedClaimRequest, CoincubeClient) {
        let descriptor = ImportedWalletDescriptor::parse(
            "wpkh([6cb116eb/84h/0h/0h]xpub6C6c6ShFaUMjohRwYGnrDr5kbrnUjBeGh5U8BPAjtZnZ2D7WpRidu2SUNpBVfmMaw8TpDA7quYNW4iNwGYaq9iAwp5fzHLCXJwTqe76WpmF/<0;1>/*)",
            None, ChainId::Bitcoin,
        ).unwrap();
        let wallet =
            settings::ImportedWalletAttachment::new(descriptor.clone(), ChainId::Bitcoin, 0, 200)
                .unwrap();
        let mut cube = settings::CubeSettings::new("Imported wallet".into(), ChainId::Bitcoin);
        cube.bitcoin_import = Some(Box::new(settings::BitcoinImport {
            operation_id: uuid::Uuid::new_v4(),
            stage: settings::BitcoinImportStage::Ready { wallet },
            last_error: None,
        }));
        let mut client = CoincubeClient::new();
        client.set_token("synthetic-imported-claim-session");
        let source = ImportedClaimSource {
            cube_id: cube.id.clone(),
            name: cube.name.clone(),
            external: descriptor.receive_descriptor().to_string(),
            internal: descriptor.change_descriptor().to_string(),
            address_window: 200,
        };
        let request = ImportedClaimRequest::new(source, &client).unwrap();
        (cube, request, client)
    }

    #[test]
    fn source_binding_rejects_changed_wallet_and_partial_window() {
        let (mut cube, mut request, _) = fixture();
        assert!(request.validate_cube(&cube).is_ok());
        request.source.address_window = 100;
        assert!(request.validate_cube(&cube).is_err());
        request.source.address_window = 200;
        request.source.internal = request.source.external.clone();
        assert!(request.validate_cube(&cube).is_err());
        let (_, request, _) = fixture();
        cube.id = request.source.cube_id.clone();
        cube.network = ChainId::BitcoinBlake2b;
        assert!(request.validate_cube(&cube).is_err());
    }

    #[test]
    fn request_requires_same_nonempty_connect_session() {
        let (_, request, mut client) = fixture();
        assert_eq!(request.session_binding, bind(&client).unwrap());
        client.set_token("different-synthetic-session");
        assert_ne!(request.session_binding, bind(&client).unwrap());
        assert!(ImportedClaimRequest::new(request.source.clone(), &CoincubeClient::new()).is_err());
        client.base_url.push_str("/different-provider");
        assert_ne!(request.session_binding, bind(&client).unwrap());
    }

    #[test]
    fn claim_rechecks_durable_source_window_and_identity() {
        let root = CoincubeDirectory::new(
            std::env::temp_dir().join(format!("imported-claim-{}", uuid::Uuid::new_v4())),
        );
        let (cube, request, client) = fixture();
        let network = root.network_directory(ChainId::Bitcoin);
        std::fs::create_dir_all(network.path()).unwrap();
        let settings = settings::Settings {
            cubes: vec![cube.clone()],
            ..Default::default()
        };
        std::fs::write(
            network.path().join(settings::SETTINGS_FILE_NAME),
            serde_json::to_vec(&settings).unwrap(),
        )
        .unwrap();
        assert!(request.validate(&root, &client).is_err()); // missing database
        prepare_storage(&root, &cube);
        assert!(request.validate(&root, &client).is_ok());

        let wallet = cube
            .bitcoin_import
            .as_ref()
            .unwrap()
            .active_attachment()
            .unwrap();
        let data = network.coincubed_data_directory(&wallet.wallet_id);
        let config = Config::from_file(Some(data.path().join("daemon.toml"))).unwrap();
        coincubed::expand_imported_discovery(&config, &data, 201).unwrap();
        assert!(request.validate(&root, &client).is_err()); // pending larger scan
        coincubed::complete_imported_history_for_test(&config, &data, 201, 201).unwrap();
        assert!(request.validate(&root, &client).is_err()); // old completed coverage is stale
        let mut expanded = request.source.clone();
        expanded.address_window = 201;
        let request = ImportedClaimRequest::new(expanded, &client).unwrap();
        assert!(request.validate(&root, &client).is_ok());

        // A different config identity also fails even if the saved Cube is unchanged.
        let mut wrong = config;
        wrong.import_birthday = Some(1);
        std::fs::write(
            data.path().join("daemon.toml"),
            toml::to_string_pretty(&wrong).unwrap(),
        )
        .unwrap();
        assert!(request.validate(&root, &client).is_err());
        std::fs::remove_dir_all(root.path()).unwrap();
    }
}
