use coincube_ui::locale::copy as localized_copy;
use coincube_ui::{
    component::{form, text::*},
    font, theme,
    widget::*,
};
use iced::{
    widget::button::{Status as ButtonStatus, Style as ButtonStyle},
    Alignment, Background, Length,
};

/// Priority tier for the mempool-driven feerate presets. Canonical definition
/// — `view/message.rs` imports it for the `FetchTransferFeeratePreset` and
/// `TransferFeerateEstimated` message variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeratePreset {
    /// ~4h confirmation target.
    Slow,
    /// ~1h confirmation target.
    Normal,
    /// ~10m confirmation target.
    Fast,
}

/// A simple feerate input (sats/vbyte). The warning string and placeholder
/// match the regular Vault spend flow (`view/vault/spend/mod.rs:367-372`) so
/// the two feerate controls feel identical.
///
/// `disabled = true` renders the input read-only — used on the Vault transfer
/// confirm screen after the PSBT is signed so the user can't edit the rate
/// out from under the signature.
pub fn feerate_input<'a, M, F>(
    feerate: &'a form::Value<String>,
    on_change: F,
    disabled: bool,
) -> Element<'a, M>
where
    M: 'a + Clone,
    F: 'static + Fn(String) -> M,
{
    let form = if disabled {
        form::Form::new_disabled("42 (in sats/vbyte)", feerate)
    } else {
        form::Form::new_trimmed(localized_copy("42 (in sats/vbyte)"), feerate, on_change).warning(
            localized_copy("Feerate must be an integer between 1 and 1000 sats/vbyte"),
        )
    };
    form.size(P1_SIZE).padding(10).into()
}

/// Wide enough for the longest label ("Normal (~1h)") on one line, so the
/// three presets are the same size and none wraps.
const PRESET_WIDTH: f32 = 140.0;

/// Three mempool-driven feerate presets, rendered left-to-right as
/// Slow / Normal / Fast (ascending priority). The button
/// corresponding to `loading` is rendered non-pressable to indicate an
/// estimate is in flight; the on_select message fires for the others. The
/// `selected` preset — the one whose estimate currently fills the input — is
/// drawn with the accent border and selected fill. When `disabled`, every
/// button is non-pressable — see `feerate_input` for the reason this matters
/// on the Vault transfer confirm screen.
pub fn feerate_presets_row<'a, M>(
    loading: Option<FeeratePreset>,
    selected: Option<FeeratePreset>,
    on_select: impl Fn(FeeratePreset) -> M + 'static,
    disabled: bool,
) -> Element<'a, M>
where
    M: 'a + Clone,
{
    let make_button =
        |label: &'static str, preset: FeeratePreset, on_select: M| -> Element<'a, M> {
            let is_loading = loading == Some(preset);
            let is_selected = selected == Some(preset);
            let label = text(if is_loading { "…" } else { label })
                .font(if is_selected {
                    font::SEMIBOLD
                } else {
                    font::MEDIUM
                })
                .wrapping(iced::widget::text::Wrapping::None);
            Button::new(
                Container::new(label)
                    .center_x(Length::Fill)
                    .align_y(Alignment::Center)
                    .padding(5),
            )
            .width(Length::Fixed(PRESET_WIDTH))
            .style(move |theme, status| preset_style(theme, status, is_selected))
            .on_press_maybe((!is_loading && !disabled).then_some(on_select))
            .into()
        };

    Row::new()
        .spacing(10)
        .push(make_button(
            "Slow (~4h)",
            FeeratePreset::Slow,
            on_select(FeeratePreset::Slow),
        ))
        .push(make_button(
            "Normal (~1h)",
            FeeratePreset::Normal,
            on_select(FeeratePreset::Normal),
        ))
        .push(make_button(
            "Fast (~10m)",
            FeeratePreset::Fast,
            on_select(FeeratePreset::Fast),
        ))
        .into()
}

/// The secondary button, with the selected preset picked out by the accent
/// border, the shell's selected fill and full-strength ink.
fn preset_style(theme: &theme::Theme, status: ButtonStatus, selected: bool) -> ButtonStyle {
    let mut style = theme::button::secondary(theme, status);
    if selected {
        let shell = &theme.colors.shell;
        style.background = Some(Background::Color(shell.selected));
        style.text_color = shell.ink;
        style.border.color = shell.accent;
        style.border.width = 1.0;
    }
    style
}
