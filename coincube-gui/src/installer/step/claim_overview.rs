//! Explain the Bitcoin Blake2b copy before any setup (Tenshu canvas v157,
//! `P-Split`). The copy reuses the source Cube's seed, keys and unlock method;
//! splitting coins happens afterwards, from either Cube's Coin control.
use crate::{
    hw::HardwareWallets,
    installer::{message::Message, step::Step, view},
};
use coincube_ui::{
    component::{button, text::*},
    theme,
    widget::*,
};
use iced::Length;

pub struct ClaimOverview {
    /// The source Cube's name.
    pub source_name: String,
    /// Whether the source (and therefore the copy) unlocks with a passkey.
    pub passkey: bool,
}

impl ClaimOverview {
    /// The inherited unlock line: the copy has no PIN or passkey picker.
    pub fn unlock_line(&self) -> String {
        format!(
            "Same {} as {}",
            if self.passkey { "passkey" } else { "PIN" },
            self.source_name
        )
    }
}

impl Step for ClaimOverview {
    fn view<'a>(
        &'a self,
        _hws: &'a HardwareWallets,
        progress: (usize, usize),
        email: Option<&'a str>,
    ) -> Element<'a, Message> {
        view::layout(
            progress,
            email,
            "Bitcoin Blake2b copy",
            Column::new()
                .spacing(20)
                .push(p1_regular(format!(
                    "Creates a Cube on Bitcoin Blake2b with the same Vault as {}, so you can see and spend your Bitcoin Blake2b coins. Creating it doesn't move or split any coins.",
                    self.source_name
                )))
                .push(h4_bold(self.unlock_line()))
                .push(
                    p1_regular(
                        "The copy unlocks the same way as the Cube it comes from. There is nothing new to remember.",
                    )
                    .style(theme::text::secondary),
                )
                .push(p1_regular(format!(
                    "Same seed and keys as {}. Bitcoin Blake2b Cubes hold a Vault only, no Spark or Liquid.",
                    self.source_name
                )))
                .push(
                    p1_regular(
                        "You only need the copy to spend your Bitcoin Blake2b coins, or to split coins from the Bitcoin Blake2b side. Splitting from Bitcoin needs no copy.",
                    )
                    .style(theme::text::secondary),
                )
                .push(
                    button::primary(None, "Create copy")
                        .on_press(Message::Next)
                        .width(Length::Fixed(260.0)),
                ),
            true,
            Some(Message::Previous),
        )
    }
}

impl From<ClaimOverview> for Box<dyn Step> {
    fn from(step: ClaimOverview) -> Self {
        Box::new(step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_copy_inherits_the_source_unlock_method() {
        let pin = ClaimOverview {
            source_name: "Everyday".into(),
            passkey: false,
        };
        assert_eq!(pin.unlock_line(), "Same PIN as Everyday");
        let passkey = ClaimOverview {
            source_name: "Everyday".into(),
            passkey: true,
        };
        assert_eq!(passkey.unlock_line(), "Same passkey as Everyday");
    }
}
