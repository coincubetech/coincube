//! Border Wallet creation wizard.
//!
//! Multi-step modal that guides the user through creating a Border Wallet
//! key derived from a visual grid pattern. Can be used in any spending path.
//!
//! Steps: Intro → RecoveryPhrase → Grid → Checksum → Confirm

use crate::app::settings::GridSeedSource;

use coincube_core::{
    border_wallet::{
        build_mnemonic, derive_enrollment, BorderWalletEnrollment, CellRef, GridRecoveryPhrase,
        OrderedPattern, WordGrid, PATTERN_LENGTH,
    },
    miniscript::{
        bitcoin::{bip32::DerivationPath, Network},
        descriptor::{DescriptorPublicKey, DescriptorXKey, Wildcard},
    },
};
use iced::{
    advanced::widget::Id,
    alignment::{Horizontal, Vertical},
    widget::{scrollable, Space},
    Length, Task,
};
use rand::{rngs::OsRng, seq::SliceRandom};
use zeroize::{Zeroize, Zeroizing};

use coincube_ui::{
    color,
    component::{
        button, form,
        modal::{self},
        text::{p1_bold, p1_regular, text},
    },
    icon, theme,
    widget::{action::Action, Column, Container, Element, Row, TextInput},
};

use std::sync::{Arc, Mutex};

use crate::{
    hw::HardwareWallets,
    installer::{
        descriptor::{Key, KeySource},
        message::{self, Message},
        step::descriptor::editor::key::SelectedKey,
    },
    signer::Signer,
};

/// The global Tab handler reveals focus here after the outer app scroll area.
pub(crate) const BORDER_WALLET_GRID_SCROLL_ID: &str = "tenshu.border-wallet.grid-scroll";

/// Wizard step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WizardStep {
    Intro,
    RecoveryPhrase,
    Grid,
    Checksum,
    Confirm,
}

/// Messages specific to the Border Wallet wizard.
#[derive(Clone)]
pub enum BorderWalletWizardMessage {
    Next,
    Previous,
    GeneratePhrase,
    PhraseWordEdited(usize, String),
    ToggleCellSelection(u16, u8),
    UndoLastCell,
    ClearPattern,
    ConfirmEnrollment,
}

// Manual Debug impl to redact recovery phrase words from logs.
impl std::fmt::Debug for BorderWalletWizardMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PhraseWordEdited(idx, _) => f
                .debug_tuple("PhraseWordEdited")
                .field(idx)
                .field(&"<redacted>")
                .finish(),
            Self::Next => write!(f, "Next"),
            Self::Previous => write!(f, "Previous"),
            Self::GeneratePhrase => write!(f, "GeneratePhrase"),
            // The ordered pattern is the key. Coordinates must not enter logs.
            Self::ToggleCellSelection(_, _) => write!(f, "ToggleCellSelection(<redacted>)"),
            Self::UndoLastCell => write!(f, "UndoLastCell"),
            Self::ClearPattern => write!(f, "ClearPattern"),
            Self::ConfirmEnrollment => write!(f, "ConfirmEnrollment"),
        }
    }
}

pub struct BorderWalletWizard {
    network: Network,
    coordinates: Vec<(usize, usize)>,
    step: WizardStep,

    // Recovery phrase (12 words) — zeroized on drop via custom Drop impl below.
    phrase_words: Vec<form::Value<String>>,
    phrase_valid: bool,

    // Grid + pattern
    grid: Option<WordGrid>,
    pattern: OrderedPattern,
    // Opaque, stable focus IDs. Their allocation order is shuffled so numeric
    // ID deltas do not encode grid coordinates if FocusAction is ever logged.
    grid_focus_ids: Vec<Id>,

    // Derived data — only the checksum word is retained, never the full mnemonic.
    checksum_word: Option<String>,
    enrollment: Option<BorderWalletEnrollment>,

    /// When true, allow random phrase generation. When false (default),
    /// the "Generate" button derives from the master signer via BIP-85.
    allow_random_grid_phrase: bool,
    /// Master signer for BIP-85 grid phrase derivation.
    signer: Option<Arc<Mutex<Signer>>>,
    /// Where [`Self::phrase_words`] currently comes from.
    ///
    /// Set by "Generate" — naming the signer it derived from — and reset to
    /// [`GridSeedSource::Independent`] by anything that makes the words
    /// something else: a random generate, or a keystroke in any of the twelve
    /// fields. It rides out on [`KeySource::BorderWallet`] so the signing flow
    /// can re-derive the phrase from that exact seed, or know to ask.
    phrase_source: GridSeedSource,

    error: Option<String>,
}

impl BorderWalletWizard {
    fn grid_focus_ids() -> Vec<Id> {
        let mut ids: Vec<_> = (0..WordGrid::TOTAL_CELLS).map(|_| Id::unique()).collect();
        ids.shuffle(&mut OsRng);
        ids
    }

    pub fn new(
        network: Network,
        coordinates: Vec<(usize, usize)>,
        signer: Arc<Mutex<Signer>>,
        allow_random_grid_phrase: bool,
    ) -> Self {
        Self {
            network,
            coordinates,
            step: WizardStep::Intro,
            phrase_words: vec![form::Value::default(); 12],
            phrase_valid: false,
            grid: None,
            pattern: OrderedPattern::new(),
            grid_focus_ids: Self::grid_focus_ids(),
            checksum_word: None,
            enrollment: None,
            allow_random_grid_phrase,
            signer: Some(signer),
            phrase_source: GridSeedSource::Independent,
            error: None,
        }
    }

    fn on_next(&mut self) -> Task<Message> {
        self.error = None;
        match self.step {
            WizardStep::Intro => {
                self.step = WizardStep::RecoveryPhrase;
            }
            WizardStep::RecoveryPhrase => {
                let phrase_str = Zeroizing::new(
                    self.phrase_words
                        .iter()
                        .map(|w| w.value.trim().to_lowercase())
                        .collect::<Vec<_>>()
                        .join(" "),
                );

                match GridRecoveryPhrase::from_phrase(&phrase_str) {
                    Ok(rp) => {
                        self.grid = Some(rp.generate_grid());
                        self.pattern = OrderedPattern::new();
                        self.step = WizardStep::Grid;
                    }
                    Err(_) => {
                        self.error = Some(
                            "Invalid recovery phrase. Please enter a valid 12-word BIP39 mnemonic."
                                .to_string(),
                        );
                    }
                }
            }
            WizardStep::Grid => {
                if !self.pattern.is_complete() {
                    self.error = Some(format!(
                        "Please select exactly {} cells. Currently selected: {}",
                        PATTERN_LENGTH,
                        self.pattern.len()
                    ));
                    return Task::none();
                }
                if let Some(grid) = &self.grid {
                    match build_mnemonic(grid, &self.pattern) {
                        Ok((mnemonic, checksum_word)) => {
                            // Keep only the checksum word; the full mnemonic is
                            // used solely for derive_enrollment and then dropped.
                            self.checksum_word = Some(checksum_word.to_string());

                            let secp =
                                coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::new();
                            match derive_enrollment(&mnemonic, self.network, &secp) {
                                Ok(enrollment) => {
                                    self.enrollment = Some(enrollment);
                                    self.step = WizardStep::Checksum;
                                }
                                Err(e) => {
                                    self.error = Some(format!("Key derivation failed: {:?}", e));
                                }
                            }
                            // `mnemonic` is dropped here — no full mnemonic retained.
                        }
                        Err(e) => {
                            self.error = Some(format!("Mnemonic construction failed: {:?}", e));
                        }
                    }
                }
            }
            WizardStep::Checksum => {
                self.step = WizardStep::Confirm;
            }
            WizardStep::Confirm => {
                return self.on_confirm();
            }
        }
        Task::none()
    }

    fn on_previous(&mut self) -> Task<Message> {
        self.error = None;
        match self.step {
            WizardStep::Intro => {}
            WizardStep::RecoveryPhrase => {
                self.step = WizardStep::Intro;
            }
            WizardStep::Grid => {
                self.step = WizardStep::RecoveryPhrase;
            }
            WizardStep::Checksum => {
                self.step = WizardStep::Grid;
            }
            WizardStep::Confirm => {
                self.step = WizardStep::Checksum;
            }
        }
        Task::none()
    }

    fn on_generate_phrase(&mut self) -> Task<Message> {
        // Default: derive from master signer via BIP-85.
        // Fallback to random if allow_random_grid_phrase is true or signer is unavailable.
        // Tracks which branch below produced the phrase, and which seed, so the
        // recorded source describes what is actually in the fields rather than
        // what the Cube was configured to prefer.
        let mut source = GridSeedSource::Independent;
        let result = if !self.allow_random_grid_phrase {
            if let Some(signer) = &self.signer {
                match signer.lock() {
                    Ok(signer) => {
                        source = GridSeedSource::MasterDerived {
                            fingerprint: signer.fingerprint(),
                        };
                        signer
                            .derive_grid_recovery_phrase()
                            .map_err(|e| format!("{:?}", e))
                    }
                    Err(e) => {
                        self.error = Some(format!(
                            "Failed to generate recovery phrase: signer lock poisoned: {}",
                            e
                        ));
                        return Task::none();
                    }
                }
            } else {
                GridRecoveryPhrase::generate().map_err(|e| format!("{:?}", e))
            }
        } else {
            GridRecoveryPhrase::generate().map_err(|e| format!("{:?}", e))
        };

        match result {
            Ok(rp) => {
                let words: Vec<&str> = rp.as_str().split_whitespace().collect();
                for (i, word) in words.iter().enumerate() {
                    if i < 12 {
                        self.phrase_words[i] = form::Value {
                            value: word.to_string(),
                            warning: None,
                            valid: true,
                        };
                    }
                }
                self.phrase_valid = true;
                self.phrase_source = source;
                self.error = None;
            }
            Err(e) => {
                self.phrase_source = GridSeedSource::Independent;
                self.error = Some(format!("Failed to generate recovery phrase: {}", e));
            }
        }
        Task::none()
    }

    fn on_phrase_word_edited(&mut self, index: usize, word: String) -> Task<Message> {
        if index < 12 {
            self.phrase_words[index].value = word;
            self.phrase_words[index].valid = true;
            self.phrase_words[index].warning = None;
        }
        self.phrase_valid = self.phrase_words.iter().all(|w| !w.value.trim().is_empty());
        // One keystroke is enough: the phrase is no longer the one that signer
        // derives, so it can no longer be re-derived at signing time.
        self.phrase_source = GridSeedSource::Independent;
        Task::none()
    }

    fn on_toggle_cell(&mut self, row: u16, col: u8) -> Task<Message> {
        let cell = CellRef::new(row, col);
        if let Some(pos) = self.pattern.cells().iter().position(|c| c == &cell) {
            self.pattern.remove_at(pos);
            self.error = None;
        } else {
            match self.pattern.add(cell) {
                Ok(()) => self.error = None,
                Err(e) => self.error = Some(crate::user_error::border_wallet_cell_message(&e)),
            }
        }
        Task::none()
    }

    fn on_confirm(&mut self) -> Task<Message> {
        if let Some(enrollment) = &self.enrollment {
            let fingerprint = enrollment.fingerprint;
            let derivation_path = enrollment.derivation_path.clone();
            let xpub = enrollment.xpub;

            let desc_xpub = DescriptorPublicKey::XPub(DescriptorXKey {
                origin: Some((fingerprint, derivation_path)),
                xkey: xpub,
                derivation_path: DerivationPath::master(),
                wildcard: Wildcard::Unhardened,
            });

            let key = Key {
                source: KeySource::BorderWallet {
                    grid_seed_source: self.phrase_source,
                },
                name: "Border Wallet".to_string(),
                fingerprint,
                key: desc_xpub,
                account: None,
            };

            return Task::done(Message::DefineDescriptor(
                message::DefineDescriptor::KeysEdited(
                    self.coordinates.clone(),
                    SelectedKey::New(Box::new(key)),
                ),
            ));
        }
        Task::none()
    }

    // --- View methods ---

    fn wizard_msg(&self, msg: BorderWalletWizardMessage) -> Message {
        Message::BorderWalletWizard(msg)
    }

    fn primary_control<'a>(
        id: &'static str,
        label: &'static str,
        message: Option<BorderWalletWizardMessage>,
    ) -> Action<'a, Message> {
        button::primary_action(id, None, label, Message::FocusAction)
            .on_press_maybe(message.map(Message::BorderWalletWizard))
    }

    fn secondary_control<'a>(
        id: &'static str,
        label: &'static str,
        message: Option<BorderWalletWizardMessage>,
    ) -> Action<'a, Message> {
        button::secondary_action(id, None, label, Message::FocusAction)
            .on_press_maybe(message.map(Message::BorderWalletWizard))
    }

    fn back_control<'a>(id: &'static str, message: Message) -> Action<'a, Message> {
        Action::new(
            id,
            Row::new()
                .spacing(8)
                .align_y(Vertical::Center)
                .push(icon::previous_icon())
                .push(text("Back")),
            Message::FocusAction,
        )
        .padding(8)
        .style(|theme, status| {
            let mut style = theme::button::transparent(theme, status);
            style.text_color = theme.colors.shell.ink;
            style
        })
        .on_press(message)
    }

    fn modal_style(theme: &theme::Theme) -> iced::widget::container::Style {
        iced::widget::container::Style {
            background: Some(iced::Background::Color(theme.colors.shell.panel)),
            border: iced::Border {
                color: theme.colors.shell.line,
                width: 1.0,
                radius: 12.0.into(),
            },
            text_color: Some(theme.colors.shell.ink),
            ..Default::default()
        }
    }

    fn grid_cell_style(
        theme: &theme::Theme,
        status: iced::widget::button::Status,
        selected: bool,
    ) -> iced::widget::button::Style {
        let shell = theme.colors.shell;
        iced::widget::button::Style {
            background: Some(iced::Background::Color(if selected {
                shell.accent
            } else {
                shell.panel
            })),
            text_color: if selected {
                iced::color!(0x201407)
            } else {
                shell.ink
            },
            border: iced::Border {
                color: if selected {
                    shell.accent
                } else if matches!(status, iced::widget::button::Status::Hovered) {
                    shell.muted
                } else {
                    shell.line
                },
                width: 1.0,
                radius: 4.0.into(),
            },
            ..Default::default()
        }
    }

    /// The intro explains the flow; the four numbered stages start with Phrase.
    fn progress(&self) -> Element<Message> {
        let current = match self.step {
            WizardStep::Intro | WizardStep::RecoveryPhrase => 0,
            WizardStep::Grid => 1,
            WizardStep::Checksum => 2,
            WizardStep::Confirm => 3,
        };
        let labels = ["Phrase", "Pattern", "Checksum", "Confirm"];
        let mut steps = Column::new().spacing(4).width(Length::Fill);
        for (index, label) in labels.iter().enumerate() {
            let number = Container::new(text(format!("{}", index + 1)).size(11))
                .center_x(Length::Fixed(20.0))
                .center_y(Length::Fixed(20.0))
                .style(move |theme: &theme::Theme| iced::widget::container::Style {
                    background: (index == current)
                        .then_some(iced::Background::Color(theme.colors.shell.selected)),
                    border: iced::Border {
                        color: if index < current {
                            theme.colors.shell.good
                        } else if index == current {
                            theme.colors.shell.accent
                        } else {
                            theme.colors.shell.line
                        },
                        width: 1.0,
                        radius: 10.0.into(),
                    },
                    text_color: Some(if index < current {
                        theme.colors.shell.good
                    } else if index == current {
                        theme.colors.shell.accent_ink
                    } else {
                        theme.colors.shell.muted
                    }),
                    ..Default::default()
                });
            let name = text(if index == current {
                format!("{} · Step {} of 4", label, index + 1)
            } else {
                label.to_string()
            })
            .size(12)
            .style(move |theme: &theme::Theme| iced::widget::text::Style {
                color: Some(if index == current {
                    theme.colors.shell.ink
                } else {
                    theme.colors.shell.muted
                }),
            });
            steps = steps.push(
                Row::new()
                    .spacing(6)
                    .align_y(Vertical::Center)
                    .push(number)
                    .push(name),
            );
        }
        steps.into()
    }

    fn view_intro(&self) -> Element<Message> {
        let header = modal::header(Some("Border Wallet".to_string()), Some(|| Message::Close));

        let description = Column::new()
            .spacing(8)
            .push(p1_bold("What is a Border Wallet?"))
            .push(p1_regular(
                "A Border Wallet key is derived from a visual pattern \
                 you select on a word grid. The key is never stored \u{2014} it exists only \
                 while you are using it.",
            ))
            .push(p1_regular("To create one, you will:"))
            .push(p1_regular(
                "1. Generate or enter a 12-word recovery phrase (the Entropy Grid seed)",
            ))
            .push(p1_regular(
                "2. Select 11 cells from the word grid (your Pattern)",
            ))
            .push(p1_regular(
                "3. Memorize the checksum word (the final derived word)",
            ))
            .push(p1_regular("4. Review and confirm the derived key"))
            .push(p1_regular(
                "Important: To reconstruct this key you need all three components: \
                 the recovery phrase, the exact pattern, and the checksum word. \
                 Write them down and store them securely.",
            ));

        let next_btn = Self::primary_control(
            "border-wallet.intro.start",
            "Get Started",
            Some(BorderWalletWizardMessage::Next),
        );

        // Footer: [Back] [spacer] [Get Started]. Intro is the first step
        // of the wizard, so "Back" returns the user to the
        // Select-key-source picker for this slot (via
        // `DefineDescriptor::ReopenKeyModal`) rather than dropping
        // them all the way back to the descriptor editor.
        let footer = Row::new()
            .push(Self::back_control(
                "border-wallet.intro.back",
                Message::DefineDescriptor(message::DefineDescriptor::ReopenKeyModal(
                    self.coordinates.clone(),
                )),
            ))
            .push(Space::new().width(Length::Fill))
            .push(next_btn)
            .align_y(Vertical::Center);

        let col = Column::new()
            .spacing(15)
            .push(header)
            .push(description)
            .push(footer)
            .width(500);

        Container::new(col)
            .padding(20)
            .style(Self::modal_style)
            .into()
    }

    fn view_recovery_phrase(&self) -> Element<Message> {
        let header = modal::header(Some("Recovery Phrase".to_string()), Some(|| Message::Close));

        let back_btn = Self::back_control(
            "border-wallet.phrase.back",
            self.wizard_msg(BorderWalletWizardMessage::Previous),
        );

        let generate_label: &'static str = if self.allow_random_grid_phrase {
            "Generate Random Phrase"
        } else {
            "Derive from Master Key"
        };
        let generate_btn = Self::secondary_control(
            "border-wallet.phrase.generate",
            generate_label,
            Some(BorderWalletWizardMessage::GeneratePhrase),
        )
        .width(Length::Fill);

        let mut word_rows = Column::new().spacing(6);
        for chunk_start in (0..12).step_by(3) {
            let mut row_widget = Row::new().spacing(8);
            for i in chunk_start..std::cmp::min(chunk_start + 3, 12) {
                let label = format!("{}.", i + 1);
                let idx = i;
                let input = TextInput::new(&format!("Word {}", i + 1), &self.phrase_words[i].value)
                    .on_input(move |s| {
                        Message::BorderWalletWizard(BorderWalletWizardMessage::PhraseWordEdited(
                            idx, s,
                        ))
                    })
                    .style(|theme: &theme::Theme, status| {
                        let mut style = theme::text_input::primary(theme, status);
                        if matches!(status, iced::widget::text_input::Status::Focused { .. }) {
                            style.border.color = theme.colors.shell.focus;
                            style.border.width = 2.0;
                        }
                        style
                    })
                    .width(Length::Fill);
                row_widget = row_widget.push(
                    Row::new()
                        .push(text(label).width(25))
                        .push(input)
                        .spacing(4)
                        .width(Length::Fill),
                );
            }
            word_rows = word_rows.push(row_widget);
        }

        let error_text: Option<Element<Message>> = self
            .error
            .as_ref()
            .map(|e| p1_regular(e.as_str()).color(color::RED).into());

        let next_btn = Self::primary_control(
            "border-wallet.phrase.next",
            "Next",
            self.phrase_valid.then_some(BorderWalletWizardMessage::Next),
        )
        .width(Length::Fill);

        let mut col = Column::new()
            .spacing(12)
            .push(header)
            .push(self.progress())
            .push(p1_regular(
                "Enter or generate the 12-word recovery phrase that will seed the word grid.",
            ))
            .push(generate_btn)
            .push(word_rows);

        if let Some(err) = error_text {
            col = col.push(err);
        }

        col = col.push(Row::new().spacing(10).push(back_btn).push(next_btn));

        Container::new(col.width(550))
            .padding(20)
            .style(Self::modal_style)
            .into()
    }

    fn view_grid(&self) -> Element<Message> {
        let header = modal::header(Some("Select Pattern".to_string()), Some(|| Message::Close));

        let back_btn = Self::back_control(
            "border-wallet.pattern.back",
            self.wizard_msg(BorderWalletWizardMessage::Previous),
        );

        let status = p1_bold(format!(
            "Selected: {} / {} cells",
            self.pattern.len(),
            PATTERN_LENGTH
        ));

        let undo_btn = Self::secondary_control(
            "border-wallet.pattern.undo",
            "Undo",
            (!self.pattern.is_empty()).then_some(BorderWalletWizardMessage::UndoLastCell),
        )
        .width(Length::Shrink);

        let clear_btn = Self::secondary_control(
            "border-wallet.pattern.clear",
            "Clear",
            (!self.pattern.is_empty()).then_some(BorderWalletWizardMessage::ClearPattern),
        )
        .width(Length::Shrink);

        let grid_content: Element<Message> = if let Some(grid) = &self.grid {
            // Reserve the focus ring outset at both scroll endpoints.
            let mut grid_col = Column::new().spacing(4).push(Space::new().height(4));

            let mut column_numbers = Row::new().spacing(4).push(Space::new().width(24));
            for col_idx in 0..WordGrid::COLS {
                column_numbers = column_numbers.push(
                    text(format!("{}", col_idx + 1))
                        .size(10)
                        .width(46)
                        .align_x(Horizontal::Center),
                );
            }
            for row_idx in 0..WordGrid::ROWS {
                let mut row_widget = Row::new().spacing(4).push(
                    text(format!("{}", row_idx + 1))
                        .size(10)
                        .width(24)
                        .align_x(Horizontal::Right),
                );
                for col_idx in 0..WordGrid::COLS {
                    let word = grid.word_at(row_idx, col_idx).unwrap_or("???");
                    let prefix = &word[..std::cmp::min(4, word.len())];
                    let cell = CellRef::new(row_idx as u16, col_idx as u8);
                    let is_selected = self.pattern.cells().contains(&cell);

                    let order_num = self
                        .pattern
                        .cells()
                        .iter()
                        .position(|c| c == &cell)
                        .map(|p| p + 1);

                    let label = if let Some(n) = order_num {
                        format!("{}", n)
                    } else {
                        prefix.to_string()
                    };

                    let r = row_idx as u16;
                    let c = col_idx as u8;
                    let cell_btn = Action::new(
                        self.grid_focus_ids[row_idx * WordGrid::COLS + col_idx].clone(),
                        text(label).size(11).align_x(Horizontal::Center),
                        Message::FocusAction,
                    )
                    .style(move |theme, status| Self::grid_cell_style(theme, status, is_selected))
                    .focus_radius(4.0)
                    .on_press(self.wizard_msg(BorderWalletWizardMessage::ToggleCellSelection(r, c)))
                    .width(46)
                    .height(28);

                    row_widget = row_widget.push(cell_btn);
                }
                grid_col = grid_col.push(row_widget);
            }
            grid_col = grid_col.push(Space::new().height(4));
            Column::new()
                .spacing(4)
                .push(column_numbers)
                .push(
                    scrollable(grid_col)
                        .id(BORDER_WALLET_GRID_SCROLL_ID)
                        // Embed the scrollbar beside the last column instead
                        // of floating over its text and focus ring.
                        .spacing(4)
                        .height(360),
                )
                .into()
        } else {
            p1_regular("No grid available").into()
        };

        let error_text: Option<Element<Message>> = self
            .error
            .as_ref()
            .map(|e| p1_regular(e.as_str()).color(color::RED).into());

        let next_btn = Self::primary_control(
            "border-wallet.pattern.next",
            "Next",
            self.pattern
                .is_complete()
                .then_some(BorderWalletWizardMessage::Next),
        )
        .width(Length::Fill);

        let mut col = Column::new()
            .spacing(10)
            .push(header)
            .push(self.progress())
            .push(p1_regular(
                "Select 11 cells from the grid in order. This pattern is your key \u{2014} remember it exactly.",
            ))
            .push(
                Row::new()
                    .spacing(10)
                    .push(status)
                    .push(undo_btn)
                    .push(clear_btn),
            )
            .push(p1_regular(format!(
                "{} rows × {} columns · first four letters shown · selected numbers show order",
                WordGrid::ROWS,
                WordGrid::COLS
            )))
            .push(grid_content);

        if let Some(err) = error_text {
            col = col.push(err);
        }

        col = col.push(Row::new().spacing(10).push(back_btn).push(next_btn));

        Container::new(col.width(850))
            .padding(20)
            .style(Self::modal_style)
            .into()
    }

    fn view_checksum(&self) -> Element<Message> {
        let header = modal::header(Some("Checksum & Key".to_string()), Some(|| Message::Close));

        let back_btn = Self::back_control(
            "border-wallet.checksum.back",
            self.wizard_msg(BorderWalletWizardMessage::Previous),
        );

        let checksum_word = self.checksum_word.as_deref().unwrap_or("???");

        let enrollment_display = if let Some(enrollment) = &self.enrollment {
            Column::new()
                .spacing(4)
                .push(p1_bold("Derived Key Info"))
                .push(p1_regular(format!(
                    "Fingerprint: {}",
                    enrollment.fingerprint
                )))
                .push(p1_regular(format!(
                    "Derivation Path: {}",
                    enrollment.derivation_path
                )))
                .push(p1_regular(format!("Network: {:?}", enrollment.network)))
        } else {
            Column::new().push(p1_regular("No enrollment data"))
        };

        let next_btn = Self::primary_control(
            "border-wallet.checksum.next",
            "Next",
            Some(BorderWalletWizardMessage::Next),
        )
        .width(Length::Fill);

        let col = Column::new()
            .spacing(12)
            .push(header)
            .push(self.progress())
            .push(p1_bold(format!("Checksum word: \"{}\"", checksum_word)))
            .push(p1_regular(
                "This is the only word you need to memorize. The checksum word is \
                 the 12th word of the derived mnemonic, calculated automatically from \
                 your pattern. Together with the recovery phrase and your pattern, it \
                 forms the three components needed to reconstruct this key.",
            ))
            .push(enrollment_display)
            .push(Row::new().spacing(10).push(back_btn).push(next_btn))
            .width(500);

        Container::new(col)
            .padding(20)
            .style(Self::modal_style)
            .into()
    }

    fn view_confirm(&self) -> Element<Message> {
        let header = modal::header(Some("Confirm".to_string()), Some(|| Message::Close));

        let back_btn = Self::back_control(
            "border-wallet.confirm.back",
            self.wizard_msg(BorderWalletWizardMessage::Previous),
        );

        let fingerprint = self
            .enrollment
            .as_ref()
            .map(|e| format!("{}", e.fingerprint))
            .unwrap_or_default();

        let checksum_word = self.checksum_word.as_deref().unwrap_or("???");

        let warning = Column::new()
            .spacing(8)
            .push(p1_bold("Important \u{2014} Read carefully"))
            .push(p1_regular(
                "After confirming, the mnemonic and private keys will be permanently \
                 erased from memory. Only the public key (xpub) and fingerprint will \
                 be stored in the wallet descriptor.",
            ))
            .push(p1_regular(
                "To reconstruct this key in the future, you need all three components:",
            ))
            .push(p1_regular(
                "1. The recovery phrase (12 words that seed the grid)",
            ))
            .push(p1_regular("2. Your exact pattern (the 11 cells in order)"))
            .push(p1_bold(format!(
                "3. The checksum word: \"{}\"",
                checksum_word
            )))
            .push(p1_regular(
                "Write down and securely store all three. Without any one of them, \
                 the key cannot be recovered.",
            ))
            .push(p1_bold(format!("Key fingerprint: {}", fingerprint)));

        let confirm_btn = Self::primary_control(
            "border-wallet.confirm.enroll",
            "Confirm & Enroll Key",
            Some(BorderWalletWizardMessage::ConfirmEnrollment),
        )
        .width(Length::Fill);

        let col = Column::new()
            .spacing(15)
            .push(header)
            .push(self.progress())
            .push(warning)
            .push(Row::new().spacing(10).push(back_btn).push(confirm_btn))
            .width(500);

        Container::new(col)
            .padding(20)
            .style(Self::modal_style)
            .into()
    }
}

/// Zeroize recovery phrase words when the wizard is dropped so they don't
/// linger in memory after the user navigates away or completes enrollment.
impl Drop for BorderWalletWizard {
    fn drop(&mut self) {
        for word in &mut self.phrase_words {
            word.value.zeroize();
        }
    }
}

impl super::DescriptorEditModal for BorderWalletWizard {
    fn processing(&self) -> bool {
        false
    }

    fn update(&mut self, _hws: &mut HardwareWallets, message: Message) -> Task<Message> {
        match message {
            Message::BorderWalletWizard(msg) => match msg {
                BorderWalletWizardMessage::Next => self.on_next(),
                BorderWalletWizardMessage::Previous => self.on_previous(),
                BorderWalletWizardMessage::GeneratePhrase => self.on_generate_phrase(),
                BorderWalletWizardMessage::PhraseWordEdited(idx, word) => {
                    self.on_phrase_word_edited(idx, word)
                }
                BorderWalletWizardMessage::ToggleCellSelection(row, col) => {
                    self.on_toggle_cell(row, col)
                }
                BorderWalletWizardMessage::UndoLastCell => {
                    self.pattern.undo_last();
                    self.error = None;
                    Task::none()
                }
                BorderWalletWizardMessage::ClearPattern => {
                    self.pattern.clear();
                    self.error = None;
                    Task::none()
                }
                BorderWalletWizardMessage::ConfirmEnrollment => self.on_confirm(),
            },
            _ => Task::none(),
        }
    }

    fn view<'a>(&'a self, _hws: &'a HardwareWallets) -> Element<'a, Message> {
        match self.step {
            WizardStep::Intro => self.view_intro(),
            WizardStep::RecoveryPhrase => self.view_recovery_phrase(),
            WizardStep::Grid => self.view_grid(),
            WizardStep::Checksum => self.view_checksum(),
            WizardStep::Confirm => self.view_confirm(),
        }
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    use coincube_ui::widget::action;
    use iced::advanced::{
        clipboard::Null,
        layout,
        renderer::Headless,
        widget::{
            operation, operation::focusable, operation::Focusable, operation::Outcome, Operation,
            Tree,
        },
        Layout, Shell,
    };
    use iced::futures::StreamExt;
    use iced::{keyboard, mouse, Event, Rectangle, Size, Vector};

    #[derive(Default)]
    struct FocusTargets(Vec<Id>);

    impl Operation for FocusTargets {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn focusable(&mut self, id: Option<&Id>, _bounds: Rectangle, _state: &mut dyn Focusable) {
            if let Some(id) = id {
                self.0.push(id.clone());
            }
        }
    }

    struct GridViewport {
        focused_id: Id,
        viewport: Option<Rectangle>,
        offset: f32,
        focused: Option<Rectangle>,
    }

    impl GridViewport {
        fn new(focused_id: Id) -> Self {
            Self {
                focused_id,
                viewport: None,
                offset: 0.0,
                focused: None,
            }
        }

        fn screen_bounds(&self) -> Rectangle {
            let mut bounds = self.focused.expect("focused grid cell must be found");
            bounds.y -= self.offset;
            bounds
        }
    }

    impl Operation for GridViewport {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn scrollable(
            &mut self,
            id: Option<&Id>,
            bounds: Rectangle,
            _content_bounds: Rectangle,
            translation: Vector,
            _state: &mut dyn operation::Scrollable,
        ) {
            if id == Some(&Id::new(BORDER_WALLET_GRID_SCROLL_ID)) {
                self.viewport = Some(bounds);
                self.offset = translation.y;
            }
        }

        fn focusable(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Focusable) {
            if id == Some(&self.focused_id) && state.is_focused() {
                self.focused = Some(bounds);
            }
        }
    }

    fn operate<T: 'static>(
        element: &mut Element<'_, Message>,
        tree: &mut Tree,
        node: &layout::Node,
        renderer: &iced::Renderer,
        operation: impl Operation<T> + 'static,
    ) -> Option<T> {
        let mut operation: Box<dyn Operation<T>> = Box::new(operation);
        loop {
            element.as_widget_mut().operate(
                tree,
                Layout::new(node),
                renderer,
                &mut operation::black_box(operation.as_mut()),
            );
            match operation.finish() {
                Outcome::None => return None,
                Outcome::Some(result) => return Some(result),
                Outcome::Chain(next) => operation = next,
            }
        }
    }

    async fn apply_focus_task(
        task: Task<Message>,
        element: &mut Element<'_, Message>,
        tree: &mut Tree,
        node: &mut layout::Node,
        renderer: &iced::Renderer,
        size: Size,
    ) {
        let mut stream = iced_runtime::task::into_stream(task).expect("focus task has operations");
        while let Some(action) = stream.next().await {
            let iced_runtime::Action::Widget(mut operation) = action else {
                panic!("focus task must only operate on widgets");
            };
            loop {
                element.as_widget_mut().operate(
                    tree,
                    Layout::new(node),
                    renderer,
                    operation.as_mut(),
                );
                match operation.finish() {
                    Outcome::None | Outcome::Some(()) => break,
                    Outcome::Chain(next) => operation = next,
                }
            }
            *node = element.as_widget_mut().layout(
                tree,
                renderer,
                &layout::Limits::new(Size::ZERO, size),
            );
        }
    }

    fn capture(
        directory: &str,
        state: &str,
        name: &str,
        element: &Element<'_, Message>,
        tree: &Tree,
        node: &layout::Node,
        renderer: &mut iced::Renderer,
        theme: &theme::Theme,
        size: Size,
    ) {
        std::fs::create_dir_all(directory).unwrap();
        iced::advanced::Renderer::reset(renderer, Rectangle::with_size(size));
        let style = iced::advanced::renderer::Style {
            text_color: theme.colors.shell.ink,
        };
        element.as_widget().draw(
            tree,
            renderer,
            theme,
            &style,
            Layout::new(node),
            iced::mouse::Cursor::Unavailable,
            &Rectangle::with_size(size),
        );
        let pixels = renderer.screenshot(Size::new(1024, 768), 1.0, theme.colors.shell.background);
        image::save_buffer(
            std::path::Path::new(directory).join(format!(
                "border-wallet-synthetic-{state}-{name}-1024x768.png"
            )),
            &pixels,
            1024,
            768,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn tab_reveals_below_viewport_grid_cell_at_1024_in_both_themes() {
        assert_eq!(
            format!(
                "{:?}",
                BorderWalletWizardMessage::ToggleCellSelection(127, 15)
            ),
            "ToggleCellSelection(<redacted>)"
        );
        for (name, theme) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            for bytes in coincube_ui::font::load() {
                iced::advanced::graphics::text::font_system()
                    .write()
                    .unwrap()
                    .load_font(bytes);
            }
            // A public BIP39 test vector is a synthetic grid fixture, never a
            // user's recovery phrase. Neither phrase nor pattern is logged.
            let grid = GridRecoveryPhrase::from_phrase(
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
            )
            .unwrap()
            .generate_grid();
            let wizard = BorderWalletWizard {
                network: Network::Regtest,
                coordinates: Vec::new(),
                step: WizardStep::Grid,
                phrase_words: vec![form::Value::default(); 12],
                phrase_valid: false,
                grid: Some(grid),
                pattern: OrderedPattern::new(),
                grid_focus_ids: BorderWalletWizard::grid_focus_ids(),
                checksum_word: None,
                enrollment: None,
                allow_random_grid_phrase: true,
                signer: None,
                phrase_source: GridSeedSource::Independent,
                error: None,
            };
            let previous = wizard.grid_focus_ids[11 * WordGrid::COLS + 15].clone();
            let target = wizard.grid_focus_ids[12 * WordGrid::COLS].clone();
            let mut element: Element<'_, Message> = Container::new(wizard.view_grid())
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into();
            let mut renderer = <iced::Renderer as Headless>::new(
                coincube_ui::font::REGULAR,
                iced::Pixels(14.0),
                Some("tiny-skia"),
            )
            .await
            .unwrap();
            let size = Size::new(1024.0, 768.0);
            let mut tree = Tree::new(element.as_widget());
            let mut node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, size),
            );
            let modal = node.children()[0].bounds();
            assert!(modal.x >= 0.0 && modal.x + modal.width <= size.width);
            assert!(modal.y >= 0.0 && modal.y + modal.height <= size.height);
            let mut targets = FocusTargets::default();
            element
                .as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut targets);
            assert!(targets.0.contains(&Id::new("border-wallet.pattern.back")));
            for disabled in [
                "border-wallet.pattern.undo",
                "border-wallet.pattern.clear",
                "border-wallet.pattern.next",
            ] {
                assert!(!targets.0.contains(&Id::new(disabled)));
            }
            let first_cell = wizard.grid_focus_ids[0].clone();
            operate(
                &mut element,
                &mut tree,
                &node,
                &renderer,
                focusable::focus::<()>(first_cell.clone()),
            );
            apply_focus_task(
                action::reveal_focused(BORDER_WALLET_GRID_SCROLL_ID),
                &mut element,
                &mut tree,
                &mut node,
                &renderer,
                size,
            )
            .await;
            let mut top = GridViewport::new(first_cell);
            element
                .as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut top);
            let first_bounds = top.screen_bounds();
            let top_viewport = top.viewport.unwrap();
            assert!(first_bounds.y >= top_viewport.y + 4.0);
            if let Ok(directory) = std::env::var("BORDER_WALLET_CAPTURE_DIR") {
                capture(
                    &directory,
                    "top-left",
                    name,
                    &element,
                    &tree,
                    &node,
                    &mut renderer,
                    &theme,
                    size,
                );
            }
            operate(
                &mut element,
                &mut tree,
                &node,
                &renderer,
                focusable::focus::<()>(previous.clone()),
            );
            apply_focus_task(
                action::reveal_focused(BORDER_WALLET_GRID_SCROLL_ID),
                &mut element,
                &mut tree,
                &mut node,
                &renderer,
                size,
            )
            .await;
            let mut before = GridViewport::new(previous);
            element
                .as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut before);
            let before_offset = before.offset;
            let before_bounds = before.screen_bounds();
            let viewport = before.viewport.unwrap();
            assert!(before_bounds.y >= viewport.y + 4.0);
            assert!(before_bounds.y + before_bounds.height <= viewport.y + viewport.height - 4.0);

            // This is the global Tab path: keep the outer shell reveal and
            // then reveal the focused cell in the nested Border Wallet grid.
            apply_focus_task(
                action::focus_next_visible(crate::app::view::nav::shell::MIDDLE_SCROLL_ID)
                    .chain(action::reveal_focused(BORDER_WALLET_GRID_SCROLL_ID)),
                &mut element,
                &mut tree,
                &mut node,
                &renderer,
                size,
            )
            .await;
            let mut after = GridViewport::new(target);
            element
                .as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut after);
            assert!(
                after.offset > before_offset,
                "Tab must scroll to the next row"
            );
            let focused = after.screen_bounds();
            let viewport = after.viewport.unwrap();
            assert!(focused.y >= viewport.y + 4.0);
            assert!(focused.y + focused.height <= viewport.y + viewport.height - 4.0);

            let key = keyboard::Key::Named(keyboard::key::Named::Enter);
            let press = Event::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key,
                physical_key: keyboard::key::Physical::Unidentified(
                    keyboard::key::NativeCode::Unidentified,
                ),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            });
            let mut messages = Vec::new();
            element.as_widget_mut().update(
                &mut tree,
                &press,
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut Null,
                &mut Shell::new(&mut messages),
                &Rectangle::with_size(size),
            );
            assert_eq!(
                messages
                    .iter()
                    .filter(|message| matches!(
                        message,
                        Message::BorderWalletWizard(
                            BorderWalletWizardMessage::ToggleCellSelection(12, 0)
                        )
                    ))
                    .count(),
                1,
                "Enter must activate the visible focused cell once"
            );

            // Tab into the final row through the same reveal chain used by
            // the GUI, then inspect the last cell's scrollbar clearance.
            let penultimate_row_end =
                wizard.grid_focus_ids[WordGrid::TOTAL_CELLS - WordGrid::COLS - 1].clone();
            let final_row_start =
                wizard.grid_focus_ids[WordGrid::TOTAL_CELLS - WordGrid::COLS].clone();
            operate(
                &mut element,
                &mut tree,
                &node,
                &renderer,
                focusable::focus::<()>(penultimate_row_end.clone()),
            );
            apply_focus_task(
                action::reveal_focused(BORDER_WALLET_GRID_SCROLL_ID),
                &mut element,
                &mut tree,
                &mut node,
                &renderer,
                size,
            )
            .await;
            let mut penultimate = GridViewport::new(penultimate_row_end);
            element.as_widget_mut().operate(
                &mut tree,
                Layout::new(&node),
                &renderer,
                &mut penultimate,
            );
            let mut final_row = GridViewport::new(final_row_start);
            apply_focus_task(
                action::focus_next_visible(crate::app::view::nav::shell::MIDDLE_SCROLL_ID)
                    .chain(action::reveal_focused(BORDER_WALLET_GRID_SCROLL_ID)),
                &mut element,
                &mut tree,
                &mut node,
                &renderer,
                size,
            )
            .await;
            element.as_widget_mut().operate(
                &mut tree,
                Layout::new(&node),
                &renderer,
                &mut final_row,
            );
            assert!(final_row.offset > penultimate.offset);
            let final_row_bounds = final_row.screen_bounds();
            let final_row_viewport = final_row.viewport.unwrap();
            assert!(final_row_bounds.y >= final_row_viewport.y + 4.0);
            assert!(
                final_row_bounds.y + final_row_bounds.height
                    <= final_row_viewport.y + final_row_viewport.height - 4.0
            );

            let last_cell = wizard.grid_focus_ids[WordGrid::TOTAL_CELLS - 1].clone();
            operate(
                &mut element,
                &mut tree,
                &node,
                &renderer,
                focusable::focus::<()>(last_cell.clone()),
            );
            apply_focus_task(
                action::reveal_focused(BORDER_WALLET_GRID_SCROLL_ID),
                &mut element,
                &mut tree,
                &mut node,
                &renderer,
                size,
            )
            .await;
            let mut bottom = GridViewport::new(last_cell);
            element
                .as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut bottom);
            assert!(bottom.offset > after.offset);
            let focused = bottom.screen_bounds();
            let viewport = bottom.viewport.unwrap();
            assert!(focused.y >= viewport.y + 4.0);
            assert!(focused.y + focused.height <= viewport.y + viewport.height - 4.0);
            assert!(
                focused.x + focused.width + 4.0 <= viewport.x + viewport.width - 10.0,
                "column 16 focus ring must clear the 10 px embedded scrollbar"
            );

            if let Ok(directory) = std::env::var("BORDER_WALLET_CAPTURE_DIR") {
                capture(
                    &directory,
                    "bottom-right",
                    name,
                    &element,
                    &tree,
                    &node,
                    &mut renderer,
                    &theme,
                    size,
                );
            }

            drop(element);
            wizard.error = Some(
                "Your pattern already has all 11 cells. Deselect one before choosing another."
                    .to_string(),
            );
            let mut error_element: Element<'_, Message> = Container::new(wizard.view_grid())
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into();
            let mut error_tree = Tree::new(error_element.as_widget());
            let error_node = error_element.as_widget_mut().layout(
                &mut error_tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, size),
            );
            let error_modal = error_node.children()[0].bounds();
            assert!(error_modal.y >= 0.0 && error_modal.y + error_modal.height <= size.height);
            if let Ok(directory) = std::env::var("BORDER_WALLET_CAPTURE_DIR") {
                capture(
                    &directory,
                    "error-line",
                    name,
                    &error_element,
                    &error_tree,
                    &error_node,
                    &mut renderer,
                    &theme,
                    size,
                );
            }
        }
    }
}
