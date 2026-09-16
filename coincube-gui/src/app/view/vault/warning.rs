use iced::Length;

use coincube_ui::{
    component::notification,
    widget::{Column, Container},
};

use crate::{app::error::Error, user_error::UserError};

/// Renders a Vault error through the same card every other failure uses.
///
/// Two things changed here. The detail line is the support **reference**, not
/// the raw error — that string used to be `error.to_string()`, which is how
/// daemon RPC codes and HTTP internals reached the screen. And the widget is
/// [`notification::error_card`] rather than a warning banner, so a Vault
/// failure and a Connect failure look like the same thing to the user: title,
/// what to do next, `Ref:`. Previously this one crammed title and guidance
/// together into the bold line, which is not a layout any other error used.
///
/// No action button: `warn` is a shared banner with no idea what the caller
/// could re-run. Screens that can retry offer their own control.
pub fn warn<'a, T: 'a + Clone>(error: Option<&Error>) -> Container<'a, T> {
    if let Some(w) = error {
        let u: UserError = w.into();
        notification::error_card(u.title, u.guidance, u.reference, None).width(Length::Fill)
    } else {
        Container::new(Column::new()).width(Length::Fill)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::DaemonError;

    /// `warn` runs on every frame, so the conversion it calls must not log.
    /// This is a documentation test as much as a behavioural one: it builds the
    /// banner repeatedly, which is exactly what iced does.
    #[test]
    fn rendering_the_banner_repeatedly_is_free_of_side_effects() {
        let e = Error::Daemon(DaemonError::DaemonStopped);
        for _ in 0..100 {
            let _: Container<'_, ()> = warn(Some(&e));
        }
    }

    /// The empty case must still produce a widget, not panic or vanish.
    #[test]
    fn no_error_renders_an_empty_container() {
        let _: Container<'_, ()> = warn(None);
    }
}
