//! Presentation language for desktop text, independent of worldview and wallet data.

use std::cell::Cell;

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
