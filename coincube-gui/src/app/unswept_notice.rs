//! Ephemeral display evidence for Bitcoin recovery. No signing authority.
use super::{cache::Cache, wallet::Wallet};
use crate::services::foreign_scan::known::{KnownOutput, KnownUnspent};
use coincube_core::{
    chain::ChainId,
    descriptors::CoincubeDescriptor,
    miniscript::bitcoin::{self, secp256k1},
};
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct Notice {
    pub proof: KnownUnspent,
    pub generation: u64,
    pub descriptor: CoincubeDescriptor,
}

pub fn owned(cache: &Cache, descriptor: &CoincubeDescriptor) -> Vec<KnownOutput> {
    let secp = secp256k1::Secp256k1::verification_only();
    cache
        .coins()
        .iter()
        .filter_map(|coin| {
            let height = coin.block_height?;
            if coin.spend_info.is_some()
                || coin.is_immature
                || height <= 0
                || height >= 961_640
                || height > cache.blockheight()
                || coin.derivation_index.is_hardened()
            {
                return None;
            }
            let script = if coin.is_change {
                descriptor.change_descriptor()
            } else {
                descriptor.receive_descriptor()
            }
            .derive(coin.derivation_index, &secp)
            .script_pubkey();
            if script != coin.address.script_pubkey() {
                return None;
            }
            Some(KnownOutput {
                outpoint: coin.outpoint,
                output: bitcoin::TxOut {
                    value: coin.amount,
                    script_pubkey: script,
                },
                bitcoin_height: height as u32,
            })
        })
        .collect()
}

impl Notice {
    pub fn new(proof: KnownUnspent, generation: u64, wallet: &Wallet) -> Self {
        Self {
            proof,
            generation,
            descriptor: wallet.main_descriptor.clone(),
        }
    }
    pub fn visible(&self, cache: &Cache, now: Instant) -> bool {
        if cache.chain() != ChainId::Bitcoin
            || !cache.connect_authenticated
            || !cache.btcb2_server_enabled
            || !self.proof.is_current(self.generation, now)
        {
            return false;
        }
        let current = owned(cache, &self.descriptor);
        !self.proof.outputs().is_empty()
            && self
                .proof
                .outputs()
                .iter()
                .all(|output| current.contains(output))
    }
}

impl Cache {
    pub fn unswept_recovery_notice(&self) -> Option<&'static str> {
        self.unswept_notice
            .as_ref()
            .filter(|notice| notice.visible(self, Instant::now()))
            .map(|_| "Some of this Cube’s coins also exist on Bitcoin Blake2b until swept there.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::vault::test_support::unified::fixture;
    use crate::daemon::model::Coin;
    use bitcoin::{Address, Amount, Network};

    fn setup() -> (Cache, tokio::sync::watch::Sender<u64>) {
        let f = fixture();
        let script = f
            .descriptor
            .receive_descriptor()
            .derive(3.into(), &secp256k1::Secp256k1::verification_only())
            .script_pubkey();
        let mut cache = Cache {
            connect_authenticated: true,
            btcb2_server_enabled: true,
            ..Cache::default()
        };
        cache.daemon_cache.blockheight = 1_000_000;
        cache.daemon_cache.coins.push(Coin {
            amount: Amount::from_sat(50_000),
            outpoint: f.psbt.unsigned_tx.input[0].previous_output,
            address: Address::from_script(&script, Network::Bitcoin).unwrap(),
            block_height: Some(900_000),
            derivation_index: 3.into(),
            spend_info: None,
            is_immature: false,
            is_change: false,
            is_from_self: false,
        });
        let (tx, rx) = tokio::sync::watch::channel(7);
        let proof =
            KnownUnspent::for_notice_test(owned(&cache, &f.descriptor), 7, rx, Instant::now());
        cache.unswept_notice = Some(Notice {
            proof,
            generation: 7,
            descriptor: f.descriptor,
        });
        (cache, tx)
    }
    #[test]
    fn unswept_notice_withdraws_for_changed_owned_coins() {
        let (cache, _live) = setup();
        assert!(cache.unswept_recovery_notice().is_some());
        for change in [0, 1, 2, 3, 4, 5, 6] {
            let mut altered = cache.clone();
            match change {
                0 => altered.daemon_cache.coins.clear(),
                1 => altered.daemon_cache.coins[0].block_height = None,
                2 => altered.daemon_cache.coins[0].amount += Amount::from_sat(1),
                3 => altered.daemon_cache.coins[0].derivation_index = 4.into(),
                4 => altered.daemon_cache.coins[0].is_change = true,
                5 => altered.daemon_cache.blockheight = 899_999,
                _ => {
                    altered.daemon_cache.coins[0].spend_info =
                        Some(coincubed::commands::LCSpendInfo {
                            txid: altered.daemon_cache.coins[0].outpoint.txid,
                            height: None,
                        })
                }
            }
            assert!(
                altered.unswept_recovery_notice().is_none(),
                "mutation {}",
                change
            );
        }
    }
    #[test]
    fn unswept_notice_is_session_chain_feature_and_age_bound() {
        let (cache, live) = setup();
        let mut altered = cache.clone();
        altered.connect_authenticated = false;
        assert!(altered.unswept_recovery_notice().is_none());
        altered = cache.clone();
        altered.btcb2_server_enabled = false;
        assert!(altered.unswept_recovery_notice().is_none());
        altered = cache.clone();
        altered.fiat_chain = ChainId::BitcoinBlake2b;
        assert!(altered.unswept_recovery_notice().is_none());
        assert!(!cache
            .unswept_notice
            .as_ref()
            .unwrap()
            .visible(&cache, Instant::now() + std::time::Duration::from_secs(61)));
        live.send_replace(8);
        assert!(cache.unswept_recovery_notice().is_none());
    }
    #[tokio::test]
    async fn unswept_notice_app_ignores_old_callbacks_and_revokes_live_evidence() {
        use crate::app::{cache::AppGeneration, claim_step1_tests::bitcoin_app, message::Message};
        let root = std::env::temp_dir().join(format!("coincube-notice-{}", uuid::Uuid::new_v4()));
        let (mut app, _) = bitcoin_app(&root);
        let generation = *app.panels.claim_generation.borrow();
        app.unswept_in_flight = Some(generation);
        let _ = app.update(Message::UnsweptNotice {
            app: AppGeneration::next(),
            generation,
            result: Ok(None),
        });
        assert_eq!(app.unswept_in_flight, Some(generation));
        let _ = app.update(Message::UnsweptNotice {
            app: app.cache.app_generation,
            generation: generation + 1,
            result: Ok(None),
        });
        assert_eq!(app.unswept_in_flight, Some(generation));
        let _ = app.update(Message::UnsweptNotice {
            app: app.cache.app_generation,
            generation,
            result: Ok(None),
        });
        assert!(app.unswept_in_flight.is_none());
        let (cache, _live) = setup();
        app.cache.unswept_notice = cache.unswept_notice;
        app.unswept_in_flight = Some(generation);
        app.revoke_claim();
        assert!(app.cache.unswept_notice.is_none());
        assert!(app.unswept_in_flight.is_none());
        assert_ne!(*app.panels.claim_generation.borrow(), generation);
        let _ = app.update(Message::UnsweptNotice {
            app: app.cache.app_generation,
            generation,
            result: Ok(None),
        });
        assert!(app.cache.unswept_notice.is_none());
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn unswept_notice_account_and_provider_replacement_without_claim_panel_revokes() {
        use crate::app::{claim_step1_tests::bitcoin_app, message::Message, view};
        let _guard = crate::app::session::test_guard();
        let root =
            std::env::temp_dir().join(format!("coincube-notice-session-{}", uuid::Uuid::new_v4()));
        for provider_change in [false, true] {
            let (mut app, _) = bitcoin_app(&root);
            app.panels.claim = None;
            app.panels.fork_claim = None;
            app.panels.connect.account.user = Some(crate::services::coincube::User {
                id: 7,
                email: "fixture@example.invalid".into(),
                email_verified: Some(true),
            });
            app.panels.connect.account.step =
                crate::app::state::connect::account::ConnectFlowStep::Dashboard;
            app.panels.connect.account.client = crate::services::coincube::CoincubeClient::for_test(
                "https://first.example.invalid",
            );
            app.panels.connect.account.client.set_token("fixture-token");
            app.panels.connect.account.features = Some(
                serde_json::from_value(
                    serde_json::json!({"plans": [], "bitcoinBlake2bEnabled": true}),
                )
                .unwrap(),
            );
            app.unswept_session = app.claim_connect_session();
            assert!(app.unswept_session.is_some());
            let generation = *app.panels.claim_generation.borrow();
            app.unswept_in_flight = Some(generation);
            drop(app.update(Message::View(view::Message::ConnectAccount(
                view::ConnectAccountMessage::PlanLoaded(None, 0),
            ))));
            assert!(app.cache.btcb2_server_enabled);
            assert_eq!(
                app.unswept_in_flight,
                Some(generation),
                "unchanged session must preserve request"
            );
            if provider_change {
                app.panels.connect.account.client =
                    crate::services::coincube::CoincubeClient::for_test(
                        "https://second.example.invalid",
                    );
                app.panels.connect.account.client.set_token("fixture-token");
            } else {
                app.panels.connect.account.user.as_mut().unwrap().id = 8;
            }
            drop(app.update(Message::View(view::Message::ConnectAccount(
                view::ConnectAccountMessage::PlanLoaded(None, 0),
            ))));
            assert!(app.unswept_in_flight.is_none());
            assert!(app.unswept_session.is_none());
            assert_ne!(*app.panels.claim_generation.borrow(), generation);
        }
        let _ = std::fs::remove_dir_all(root);
    }
    #[tokio::test]
    async fn unswept_notice_positive_callback_displays_until_local_spend() {
        use crate::app::{claim_step1_tests::bitcoin_app, message::Message};
        let root =
            std::env::temp_dir().join(format!("coincube-notice-positive-{}", uuid::Uuid::new_v4()));
        let (mut app, wallet) = {
            let _guard = crate::app::session::test_guard();
            bitcoin_app(&root)
        };
        let (cache, _live) = setup();
        app.cache.daemon_cache = cache.daemon_cache;
        let script = wallet
            .main_descriptor
            .receive_descriptor()
            .derive(3.into(), &secp256k1::Secp256k1::verification_only())
            .script_pubkey();
        app.cache.daemon_cache.coins[0].address =
            Address::from_script(&script, Network::Bitcoin).unwrap();
        app.cache.connect_authenticated = true;
        app.panels.connect.account.user = Some(crate::services::coincube::User {
            id: 7,
            email: "fixture@example.invalid".into(),
            email_verified: Some(true),
        });
        app.panels.connect.account.step =
            crate::app::state::connect::account::ConnectFlowStep::Dashboard;
        app.panels.connect.account.client =
            crate::services::coincube::CoincubeClient::for_test("https://notice.example.invalid");
        app.panels.connect.account.client.set_token("fixture-token");
        app.unswept_session = app.claim_connect_session();
        let generation = *app.panels.claim_generation.borrow();
        app.unswept_in_flight = Some(generation);
        let proof = KnownUnspent::for_notice_test(
            owned(&app.cache, &wallet.main_descriptor),
            generation,
            app.panels.claim_generation.subscribe(),
            Instant::now(),
        );
        assert!(app.cache.unswept_recovery_notice().is_none());
        drop(app.update(Message::UnsweptNotice {
            app: app.cache.app_generation,
            generation,
            result: Ok(Some(proof)),
        }));
        assert!(app.cache.unswept_recovery_notice().is_some());
        let coin = &mut app.cache.daemon_cache.coins[0];
        coin.spend_info = Some(coincubed::commands::LCSpendInfo {
            txid: coin.outpoint.txid,
            height: None,
        });
        assert!(app.cache.unswept_recovery_notice().is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
