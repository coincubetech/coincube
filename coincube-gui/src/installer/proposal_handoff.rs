//! Presentation-only entry to the opt-in native Regtest proposal flow.
use super::{proposal, Context, Message, Step};
use coincube_ui::{
    component::{
        button, card,
        text::{caption, h3, h4_bold, text},
    },
    widget::Element,
};
use iced::{
    widget::{column, row, scrollable},
    Length,
};
use std::sync::Arc;

pub(crate) const SCROLL_ID: &str = "proposal-handoff-scroll";

pub(crate) struct ProposalHandoff {
    session: Arc<proposal::Session>,
}

impl ProposalHandoff {
    pub(crate) fn new(session: Arc<proposal::Session>) -> Self {
        Self { session }
    }
}

impl Step for ProposalHandoff {
    fn apply(&mut self, _ctx: &mut Context) -> bool {
        self.session.active()
    }

    fn view<'a>(
        &'a self,
        _hws: &'a crate::hw::HardwareWallets,
        _progress: (usize, usize),
        _email: Option<&'a str>,
    ) -> Element<'a, Message> {
        let topology = self.session.topology();
        let active = self.session.active();
        let handoff = card::simple(
            column![
                caption(if active { "PROPOSAL PREPARED · WAITING FOR YOU" } else { "PROPOSAL ACCESS ENDED" }),
                h3(if active { "Continue setup in Tenshu" } else { "Return to the Cube" }),
                text(if active {
                    "A paired client prepared a policy proposal. No Vault has been created. You will select the real keys and review the compiled descriptor in Tenshu before creation."
                } else {
                    "This proposal can no longer continue. Return to the Cube and prepare a fresh proposal after unlocking."
                }),
            ]
            .spacing(12),
        )
        .width(Length::Fill);

        let plan = card::simple(
            column![
                h4_bold("Proposed plan"),
                text(format!(
                    "Primary · {} of {} public-key slots",
                    topology.primary.threshold, topology.primary.key_count
                )),
                text(format!(
                    "Recovery · {} of {} keys after {} relative blocks for each coin",
                    topology.recovery.quorum.threshold,
                    topology.recovery.quorum.key_count,
                    topology.recovery.relative_delay_blocks
                )),
                caption("Block time varies; the delay is not a calendar deadline."),
                text("Script · P2WSH"),
                text("Network · Bitcoin Regtest"),
                text("Node · isolated loopback, port 28443"),
            ]
            .spacing(10),
        )
        .width(Length::FillPortion(1));

        let checks = card::simple(
            column![
                h4_bold("What happens next"),
                text("1  Choose and check the real public keys in Tenshu"),
                text("2  Complete native backup and node steps"),
                text("3  Review the actual descriptor and approve creation"),
                caption("Device control · not verified"),
                caption("Backup possession · not verified"),
                caption("Recovery test · not verified"),
            ]
            .spacing(10),
        )
        .width(Length::FillPortion(1));

        let content = column![
            caption("EXPERIMENTAL REGTEST SETUP"),
            h3("Vault setup from a paired client"),
            text("This opt-in handoff is available only for an empty Regtest Cube. Tenshu keeps creation approval with you."),
            handoff,
            row![plan, checks].spacing(16).width(Length::Fill),
            caption("Creating the Vault does not grant the paired client read access. Ending this setup keeps its existing pairing until you revoke it."),
            row![
                button::primary_action("proposal-handoff-continue", None, "Continue to key selection", Message::FocusAction)
                    .on_press_maybe(active.then_some(Message::Next)),
                button::secondary_action("proposal-handoff-return", None, "Return to Cube", Message::FocusAction)
                    .on_press(if active { Message::Previous } else { Message::BackToApp(coincube_core::miniscript::bitcoin::Network::Regtest) }),
            ]
            .spacing(12),
        ]
        .spacing(16)
        .padding(24)
        .max_width(980);
        scrollable(content).id(SCROLL_ID).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::installer::proposal::tests::{fixture, TestDir};
    use coincube_core::miniscript::bitcoin::Network;
    use coincube_ui::{font, theme, widget::Renderer};
    use iced::{
        advanced::{
            renderer::Headless,
            widget::{operation, operation::Focusable, Id, Operation},
        },
        mouse, Rectangle, Size,
    };
    use iced_runtime::user_interface::{Cache, UserInterface};

    #[derive(Default)]
    struct Probe {
        labels: Vec<String>,
        focus: Vec<Id>,
    }
    impl Operation for Probe {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn text(&mut self, _: Option<&Id>, _: Rectangle, value: &str) {
            self.labels.push(value.to_owned());
        }
        fn focusable(&mut self, id: Option<&Id>, _: Rectangle, _: &mut dyn Focusable) {
            if let Some(id) = id {
                self.focus.push(id.clone());
            }
        }
    }

    #[tokio::test]
    async fn proposal_handoff_shows_actual_plan_and_only_active_actions_focus() {
        let _guard = crate::app::session::test_guard();
        let dir = TestDir::new();
        let (mut ctx, session) = fixture(dir.path());
        let hws = crate::hw::HardwareWallets::new(
            crate::dir::CoincubeDirectory::new(dir.path().to_owned()),
            Network::Regtest,
        );
        let mut step = ProposalHandoff::new(session.clone());
        assert!(step.apply(&mut ctx));

        for bytes in font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        let mut renderer = <Renderer as Headless>::new(font::REGULAR, 16.into(), Some("tiny-skia"))
            .await
            .unwrap();
        for (name, palette) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            let mut ui = UserInterface::build(
                step.view(&hws, (0, 8), None),
                Size::new(1024.0, 768.0),
                Cache::new(),
                &mut renderer,
            );
            let mut probe = Probe::default();
            ui.operate(&renderer, &mut operation::black_box(&mut probe));
            assert!(probe
                .labels
                .iter()
                .any(|s| s == "PROPOSAL PREPARED · WAITING FOR YOU"));
            assert!(probe
                .labels
                .iter()
                .any(|s| s.contains("3 of 5 public-key slots")));
            assert!(probe.labels.iter().any(|s| s.contains("port 28443")));
            assert!(probe
                .labels
                .iter()
                .any(|s| s == "Recovery test · not verified"));
            assert!(probe.focus.contains(&Id::new("proposal-handoff-continue")));
            assert!(probe.focus.contains(&Id::new("proposal-handoff-return")));
            let mut messages = Vec::new();
            ui.update(
                &[iced::Event::Window(iced::window::Event::RedrawRequested(
                    std::time::Instant::now(),
                ))],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            ui.draw(
                &mut renderer,
                &palette,
                &iced::advanced::renderer::Style {
                    text_color: palette.colors.shell.ink,
                },
                mouse::Cursor::Unavailable,
            );
            if let Ok(output) = std::env::var("AI_SETUP_CAPTURE_DIR") {
                std::fs::create_dir_all(&output).unwrap();
                let pixels =
                    renderer.screenshot(Size::new(1024, 768), 1.0, palette.colors.shell.background);
                image::save_buffer(
                    std::path::Path::new(&output)
                        .join(format!("ai-setup-handoff-{name}-1024x768.png")),
                    &pixels,
                    1024,
                    768,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
        session.revoke();
        assert!(!step.apply(&mut ctx));
        let mut ui = UserInterface::build(
            step.view(&hws, (0, 8), None),
            Size::new(1024.0, 768.0),
            Cache::new(),
            &mut renderer,
        );
        let mut probe = Probe::default();
        ui.operate(&renderer, &mut operation::black_box(&mut probe));
        assert!(probe.labels.iter().any(|s| s == "PROPOSAL ACCESS ENDED"));
        assert!(!probe.focus.contains(&Id::new("proposal-handoff-continue")));
        assert!(probe.focus.contains(&Id::new("proposal-handoff-return")));
    }
}
