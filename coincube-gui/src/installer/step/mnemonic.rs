use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use coincube_core::{bip39, signer::MasterSigner};
use iced::Task;

use coincube_ui::widget::Element;

use crate::{
    hw::HardwareWallets,
    installer::{context::Context, message::Message, step::Step, view},
    signer::Signer,
};

pub struct BackupMnemonic {
    signer: Arc<Mutex<Signer>>,
    fresh_fork_cube: bool,
    confirmed: bool,
}

impl BackupMnemonic {
    pub fn new(signer: Arc<Mutex<Signer>>) -> Self {
        Self {
            signer,
            fresh_fork_cube: false,
            confirmed: false,
        }
    }
}

impl From<BackupMnemonic> for Box<dyn Step> {
    fn from(s: BackupMnemonic) -> Box<dyn Step> {
        Box::new(s)
    }
}

impl Step for BackupMnemonic {
    fn load_context(&mut self, ctx: &Context) {
        self.fresh_fork_cube = ctx.fresh_fork_cube;
    }
    fn apply(&mut self, ctx: &mut Context) -> bool {
        if ctx.fresh_fork_cube {
            ctx.fresh_fork_seed_backed_up = self.confirmed;
            return self.confirmed;
        }
        true
    }
    fn update(&mut self, _hws: &mut HardwareWallets, message: Message) -> Task<Message> {
        if let Message::ConfirmFreshSeedBackup(confirmed) = message {
            self.confirmed = confirmed;
        }
        Task::none()
    }
    fn skip(&self, ctx: &Context) -> bool {
        if ctx.fresh_fork_cube {
            return false;
        }
        if let Some(descriptor) = &ctx.descriptor {
            !descriptor
                .to_string()
                .contains(&self.signer.lock().unwrap().fingerprint().to_string())
        } else {
            false
        }
    }
    fn view<'a>(
        &'a self,
        _hws: &'a HardwareWallets,
        progress: (usize, usize),
        email: Option<&'a str>,
    ) -> Element<'a, Message> {
        if self.fresh_fork_cube {
            let words = self
                .signer
                .lock()
                .unwrap()
                .mnemonic()
                .iter()
                .enumerate()
                .map(|(i, word)| format!("{}. {}", i + 1, word))
                .collect::<Vec<_>>()
                .join("   ");
            return view::backup_fresh_fork_mnemonic(progress, email, words, self.confirmed);
        }
        view::backup_mnemonic(progress, email)
    }
}

pub struct RecoverMnemonic {
    language: bip39::Language,
    words: [(String, bool); 12],
    current: usize,
    suggestions: Vec<String>,
    error: Option<String>,
    skip: bool,
    recover: bool,
}

impl Default for RecoverMnemonic {
    fn default() -> Self {
        Self {
            language: bip39::Language::English,
            words: Default::default(),
            current: 0,
            suggestions: Vec::new(),
            error: None,
            skip: false,
            recover: false,
        }
    }
}

impl From<RecoverMnemonic> for Box<dyn Step> {
    fn from(s: RecoverMnemonic) -> Box<dyn Step> {
        Box::new(s)
    }
}

impl Step for RecoverMnemonic {
    fn update(&mut self, _hws: &mut HardwareWallets, message: Message) -> Task<Message> {
        match message {
            Message::MnemonicWord(index, value) => {
                if let Some((word, valid)) = self.words.get_mut(index) {
                    if value.len() >= 3 {
                        let suggestions = self.language.words_by_prefix(&value);
                        if suggestions.contains(&value.as_ref()) {
                            *valid = true;
                            self.suggestions = Vec::new();
                        } else {
                            self.suggestions = suggestions.iter().map(|s| s.to_string()).collect();
                            *valid = false;
                        }
                    } else {
                        self.suggestions = Vec::new();
                        *valid = false;
                    }
                    self.current = index;
                    *word = value;
                }
            }
            Message::ImportMnemonic(recover) => self.recover = recover,
            Message::Skip => {
                self.skip = true;
                return Task::perform(async {}, |_| Message::Next);
            }
            _ => {}
        }
        Task::none()
    }

    fn apply(&mut self, ctx: &mut Context) -> bool {
        if self.skip {
            // If the user click previous, we dont want the skip to be set to true.
            self.skip = false;
            return true;
        }

        let words: Vec<String> = self
            .words
            .iter()
            .filter_map(|(s, valid)| if *valid { Some(s.clone()) } else { None })
            .collect();

        let seed = match MasterSigner::from_str(ctx.bitcoin_config.network, &words.join(" ")) {
            Ok(seed) => seed,
            Err(e) => {
                self.error = Some(e.to_string());
                return false;
            }
        };

        let signer = Signer::new(seed);
        let fingerprint = signer.fingerprint();

        if let Some(descriptor) = &ctx.descriptor {
            let info = descriptor.policy();
            let mut descriptor_keys = HashSet::new();
            for fingerprint in info.primary_path().thresh_origins().1.keys() {
                descriptor_keys.insert(*fingerprint);
            }
            for path in info.recovery_paths().values() {
                for fingerprint in path.thresh_origins().1.keys() {
                    descriptor_keys.insert(*fingerprint);
                }
            }
            if !descriptor_keys.contains(&fingerprint) {
                self.error =
                    Some("The descriptor does not use a key derived from this seed".to_string());
                return false;
            }
        }

        ctx.recovered_signer = Some(Arc::new(signer));
        true
    }
    fn view<'a>(
        &'a self,
        _hws: &'a HardwareWallets,
        progress: (usize, usize),
        email: Option<&'a str>,
    ) -> Element<'a, Message> {
        view::recover_mnemonic(
            progress,
            email,
            &self.words,
            self.current,
            &self.suggestions,
            self.recover,
            self.error.as_ref(),
        )
    }
}

#[cfg(test)]
mod fork_backup_tests {
    use super::*;
    #[test]
    fn fresh_fork_master_requires_explicit_backup_confirmation() {
        let dir = crate::dir::CoincubeDirectory::new(Default::default());
        let mut ctx = Context::new_for_chain(
            crate::chain::ChainId::BitcoinBlake2b,
            dir.clone(),
            crate::installer::context::RemoteBackend::None,
            None,
            None,
        );
        ctx.fresh_fork_cube = true;
        let signer = Arc::new(Mutex::new(Signer::generate(ctx.network).unwrap()));
        let mut step = BackupMnemonic::new(signer);
        step.load_context(&ctx);
        assert!(!step.skip(&ctx));
        assert!(!step.apply(&mut ctx));
        assert!(!ctx.fresh_fork_seed_backed_up);
        let mut hws = HardwareWallets::new(dir, ctx.network);
        let _ = step.update(&mut hws, Message::ConfirmFreshSeedBackup(true));
        assert!(step.apply(&mut ctx));
        assert!(ctx.fresh_fork_seed_backed_up);
    }
}
