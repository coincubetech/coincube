//! Presentation language for desktop text, independent of worldview and wallet data.

use std::{borrow::Cow, cell::Cell, collections::HashMap, sync::LazyLock};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(from = "serde_json::Value")]
pub enum Language {
    #[default]
    #[serde(rename = "en")]
    English,
    #[serde(rename = "es")]
    Spanish,
}

impl From<serde_json::Value> for Language {
    fn from(value: serde_json::Value) -> Self {
        match value.as_str() {
            Some("es") => Self::Spanish,
            _ => Self::English,
        }
    }
}

thread_local! {
    static LANGUAGE: Cell<Language> = const { Cell::new(Language::English) };
}

/// Scope a synchronous view build to this window's accepted language.
pub fn render_in<T>(language: Language, render: impl FnOnce() -> T) -> T {
    struct Restore(Language);
    impl Drop for Restore {
        fn drop(&mut self) {
            LANGUAGE.set(self.0);
        }
    }

    let _restore = Restore(LANGUAGE.replace(language));
    render()
}

pub fn language() -> Language {
    LANGUAGE.get()
}

mod messages;
pub use messages::Text;

static SPANISH_DESKTOP: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../static/desktop.es.json"))
        .expect("Spanish desktop copy must be valid")
});

/// Localize an existing English UI literal. Call this at presentation sites,
/// never on a wallet name, identifier, or any data used to choose an action.
pub fn copy(english: &str) -> &str {
    match language() {
        Language::English => english,
        Language::Spanish => SPANISH_DESKTOP.get(english).map_or(english, String::as_str),
    }
}

/// Localize an app-authored notice at display time. Unknown diagnostics and
/// support references are copied exactly.
pub fn notice(english: String) -> String {
    if language() == Language::English {
        return english;
    }
    if let Some((message, reference)) = english.rsplit_once(" (Ref: ") {
        if let Some((title, guidance)) = message.split_once(" — ") {
            if reference.strip_suffix(')').is_some_and(|code| {
                code.starts_with("CC-")
                    && code.bytes().all(|byte| {
                        byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            }) {
                return format!(
                    "{} — {} (Ref.: {reference}",
                    notice_field(title),
                    notice_field(guidance)
                );
            }
        }
    }
    match notice_field(&english) {
        Cow::Borrowed(value) if value == english => english,
        value => value.into_owned(),
    }
}

fn notice_field(english: &str) -> Cow<'_, str> {
    // Transfer errors prepend a guarantee about the actual failure point.
    for prefix in [
        "Nothing was sent and your balances are unchanged. ",
        "Your funds were not moved. ",
    ] {
        if let Some(guidance) = english.strip_prefix(prefix) {
            return Cow::Owned(format!(
                "{} {}",
                copy(prefix.trim_end()),
                notice_value(guidance)
            ));
        }
    }
    notice_value(english)
}

fn notice_value(english: &str) -> Cow<'_, str> {
    if let Some(text) = SPANISH_DESKTOP.get(english) {
        return Cow::Borrowed(text);
    }
    // Keep these source templates aligned with the typed errors in user_error.
    // Interpolate only the original value. A diagnostic is never translated
    // by pattern unless the surrounding app-authored template matches.
    const TEMPLATES: &[(&str, &str, &str)] = &[
        ("Too many incorrect PINs. Try again in ", "s.", "Demasiados intentos con un PIN incorrecto. Inténtalo de nuevo en {value} s."),
        ("Too many incorrect PINs. Try again in ", " minute(s).", "Demasiados intentos con un PIN incorrecto. Inténtalo de nuevo en {value} min."),
        ("Password must be at least ", " characters.", "La contraseña debe tener al menos {value} caracteres."),
        ("The generated seed phrase failed its own checksum (", "). Nothing was written to disk — please start again.", "No se pudo verificar la frase de recuperación generada ({value}). No se guardó nada en el disco. Empieza de nuevo."),
        ("Failed to prepare payment: ", "", "No se pudo preparar el pago: {value}"),
        ("Couldn't complete the passkey check, so this Cube stayed locked. The Cube itself is fine — try again, or restore it from your Recovery Kit. (", ")", "No se pudo verificar la llave de acceso y el Cube sigue bloqueado. Tu Cube está intacto. Inténtalo de nuevo o restáuralo con tu kit de recuperación. ({value})"),
        ("PIN check failed to run: ", "", "No se pudo comprobar el PIN: {value}"),
        ("The payment was rejected, so nothing was sent: ", "", "El pago fue rechazado y no se envió nada: {value}"),
        ("Couldn't confirm what happened to this payment (", "). Refresh and check Transactions before sending again.", "No se pudo confirmar qué pasó con este pago ({value}). Actualiza y revisa Transacciones antes de volver a enviar."),
        ("Your ", " wallet isn't available", "Tu cartera {value} no está disponible"),
        ("Open the ", " wallet from the dashboard, then start the transfer again.", "Abre la cartera {value} desde el resumen y vuelve a iniciar la transferencia."),
        ("Couldn't get a deposit address from your ", " wallet", "No se pudo obtener una dirección de depósito de tu cartera {value}"),
        ("Check your internet connection and your ", " wallet, then try again.", "Revisa tu conexión a internet y tu cartera {value}. Después, inténtalo de nuevo."),
        ("Your wallet is on ", ", which the Liquid wallet doesn't support. Switch to a supported network to use it.", "Tu cartera está en {value}, una red que la cartera Liquid no admite. Cambia a una red compatible para usarla."),
        ("", " Check the descriptor you entered and try again.", "Revisa el descriptor que ingresaste e inténtalo de nuevo. Detalle: {value}"),
        ("", " Check the amount and fee rate, then try again.", "Revisa el importe y la tasa de comisión e inténtalo de nuevo. Detalle: {value}"),
        ("", ". Check that the backup file is complete and matches this wallet.", "Comprueba que el archivo de copia esté completo y corresponda a esta cartera. Detalle: {value}"),
    ];
    for (before, after, template) in TEMPLATES {
        if let Some(value) = english
            .strip_prefix(before)
            .and_then(|s| s.strip_suffix(after))
        {
            return Cow::Owned(template.replacen("{value}", value, 1));
        }
    }
    Cow::Borrowed(english)
}

/// Format full UI messages in the accepted language. Both templates are Rust
/// format literals, so argument types and formatting are checked at build time.
#[macro_export]
macro_rules! localized_format {
    ($english:literal, $spanish:literal $(, $($args:tt)*)?) => {{
        match $crate::locale::language() {
            $crate::locale::Language::English => format!($english $(, $($args)*)?),
            $crate::locale::Language::Spanish => format!($spanish $(, $($args)*)?),
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spanish_error_copy_preserves_network_references_and_raw_detail() {
        let error = "Your Spark wallet isn't available — Nothing was sent and your balances are unchanged. Open the Spark wallet from the dashboard, then start the transfer again. (Ref: CC-XFER-WALLET)";
        let opaque = "Review";
        assert_eq!(notice(error.to_owned()), error);
        render_in(Language::Spanish, || {
            assert_eq!(notice(error.to_owned()), "Tu cartera Spark no está disponible — No se envió nada. Tus saldos siguen intactos. Abre la cartera Spark desde el resumen y vuelve a iniciar la transferencia. (Ref.: CC-XFER-WALLET)");
            assert_eq!(
                notice(format!(
                    "{opaque} Check the amount and fee rate, then try again."
                )),
                "Revisa el importe y la tasa de comisión e inténtalo de nuevo. Detalle: Review"
            );
            assert_eq!(
                notice("SDK diagnostic 0x019af".into()),
                "SDK diagnostic 0x019af"
            );
        });
        assert_eq!(notice(error.to_owned()), error);
    }

    #[test]
    fn spanish_format_preserves_named_and_positional_values() {
        let reference = "CC-XFER-INPUT";
        let format = || {
            localized_format!(
                "Value: {:.2} BTC ({reference})",
                "Importe: {:.2} BTC ({reference})",
                0.01
            )
        };
        assert_eq!(format(), "Value: 0.01 BTC (CC-XFER-INPUT)");
        assert_eq!(
            render_in(Language::Spanish, format),
            "Importe: 0.01 BTC (CC-XFER-INPUT)"
        );
    }
}
