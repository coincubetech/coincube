//! App-owned recovery context, consent and off-page result routing.
use super::*;

impl App {
    pub(super) fn route_recovery_kit(&mut self, msg: view::RecoveryKitMessage) -> Task<Message> {
        let seed_source = if self.cube_settings.is_passkey_cube() {
            crate::app::state::settings::recovery_kit::SeedSource::Passkey
        } else {
            crate::app::state::settings::recovery_kit::SeedSource::Mnemonic
        };
        let client = self.authenticated_coincube_client();
        let server_cube_id = self.panels.connect.cube.server_cube_id;
        let wallet = self
            .wallet
            .clone()
            .filter(|wallet| wallet.supports_inheritance());
        let local_cube_id = self.cube_settings.id.clone();
        let imported = self
            .cube_settings
            .bitcoin_import
            .as_deref()
            .and_then(|import| {
                let attachment = import.active_attachment()?;
                let daemon = self.panels.imported_bitcoin.as_ref()?.backup_daemon()?;
                Some(
                    crate::app::state::settings::recovery_kit::ImportedBackupSource {
                        import_operation_id: import.operation_id,
                        imported_claim_source: self.cube_settings.imported_claim_source.clone(),
                        signer_metadata: Some(
                            crate::services::recovery::imported::encrypted_signer_metadata(
                                &self.datadir.network_directory(self.cube_settings.network),
                                &attachment.wallet_id,
                            )
                            .ok()?,
                        ),
                        descriptor: attachment.descriptor.clone(),
                        name: self.cube_settings.name.clone(),
                        birthday: attachment.birthday,
                        initial_lookahead: attachment.initial_lookahead,
                        chain: self.cube_settings.network,
                        daemon,
                    },
                )
            });
        crate::app::state::settings::recovery_kit::update_with_imported(
            &mut self.panels.global_settings.recovery_kit,
            msg,
            &self.cache,
            &local_cube_id,
            seed_source,
            client,
            server_cube_id,
            wallet,
            imported,
        )
    }

    pub(super) fn route_recovery_alerts(
        &mut self,
        msg: view::RecoveryAlertsMessage,
    ) -> Task<Message> {
        let client = self.authenticated_coincube_client();
        let server_cube_id = self.panels.connect.cube.server_cube_id;
        let wallet = self.wallet.clone();
        let entitled = self.panels.connect.account.is_recovery_alerts_entitled();
        let escrow_entitled = self.panels.connect.account.is_inheritance_escrow_entitled();
        let session_generation = self.panels.connect.account.session_generation();
        let local_cube_id = self.cube_settings.id.clone();
        // A *successful, current-session* status load is the only message
        // that reveals an eligible Vault for the one-time prompt; a
        // *successful user change* means the owner has engaged with the
        // alerts decision, so the prompt must never re-fire for this Cube
        // (else turning alerts off would immediately re-nudge — the
        // residual nudge is the banner, not the modal). Classify before
        // `msg` is moved into `update`.
        //
        // Gate the load on `Ok` + a matching generation: a failed load
        // leaves the monitoring state unknown (or resets it to no-vault),
        // and `update` drops a stale-generation load without hydrating
        // anything (see its guard), so neither reveals a fresh eligible
        // state to prompt off.
        let is_status_load = matches!(
            msg,
            view::RecoveryAlertsMessage::StatusLoaded(Ok(_), gen)
                if gen == session_generation
        );
        // Only an *accepted* change counts as engagement. `update` drops
        // a `ChangeResult` whose captured generation no longer matches the
        // active session (see its stale-result guard), so gate on the same
        // match here — otherwise a stale success that never touched state
        // would still permanently mark the prompt answered.
        let is_ok_change = matches!(
            msg,
            view::RecoveryAlertsMessage::ChangeResult(Ok(_), gen, _)
                if gen == session_generation
        );
        let task = crate::app::state::settings::recovery_alerts::update(
            &mut self.panels.global_settings.recovery_alerts,
            msg,
            client,
            server_cube_id,
            wallet,
            entitled,
            escrow_entitled,
            session_generation,
            &self.cache,
            &local_cube_id,
        );
        // The owner engaged with the alerts decision via the card → mark
        // the prompt answered (durably) so the modal never fires for this
        // Cube again.
        let engaged = if is_ok_change && !self.cube_settings.recovery_alerts_prompt_answered {
            self.cube_settings.recovery_alerts_prompt_answered = true;
            self.persist_recovery_alerts_answered()
        } else {
            Task::none()
        };
        // Existing-Vault trigger (PR 3): a just-resolved `StatusLoaded`
        // may have revealed an eligible (keyholders, monitoring off,
        // unanswered) Vault. Offer the one-time consent prompt now.
        let prompt = if is_status_load {
            self.maybe_show_recovery_alerts_prompt()
        } else {
            Task::none()
        };
        Task::batch([task, engaged, prompt])
    }
}
