//! Connect sheet › Account › Your name.
//!
//! The signed-in user can give the name they want shown beside their email to
//! the people in their Cubes (`PUT /api/v1/user` with `displayName`). It is
//! optional, and saving a blank field removes it. The name lives on the
//! Connect account only: this app keeps no copy on disk and reads it from the
//! profile (`GET /api/v1/user`) each time the Account section opens.
//!
//! The field is shown only once the profile has been read and carried a
//! `displayName`. A server that predates display names sends none, so the
//! Account section then looks exactly as it did before.

use crate::{
    app::view::YourNameFailure,
    services::coincube::{display_name, CoincubeError, User},
};

/// The one line under the field after the last action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YourNameNotice {
    Saved,
    Removed,
    /// Longer than [`display_name::MAX_CHARS`].
    TooLong,
    /// A character that isn't allowed, or no letter or number.
    NotAllowed,
    /// The request failed, or the server answered without the name.
    NotSaved,
}

#[derive(Debug, Default)]
pub struct YourNameState {
    /// The name on the account as the server last reported it; empty when
    /// none is set. `None` until the profile has been read, and when the
    /// server has no name field: the field is not shown.
    pub saved: Option<String>,
    /// What the field holds while it differs from `saved`.
    pub draft: Option<String>,
    /// `PUT` in flight.
    pub saving: bool,
    pub notice: Option<YourNameNotice>,
    /// Bumped for each profile read and each save. A result carrying an older
    /// value belongs to a superseded request or session and is dropped.
    pub seq: u64,
    /// The token of the profile read in flight, if any.
    loading: Option<u64>,
    /// The token of the save in flight, if any.
    sending: Option<u64>,
}

impl YourNameState {
    /// The text the field shows.
    pub fn value(&self) -> &str {
        self.draft
            .as_deref()
            .or(self.saved.as_deref())
            .unwrap_or_default()
    }

    /// Whether Save would send anything.
    pub fn can_save(&self) -> bool {
        self.saved.is_some() && self.draft.is_some() && !self.saving
    }

    /// Starts a profile read. Returns its token.
    pub fn begin_load(&mut self) -> u64 {
        let seq = self.next();
        self.loading = Some(seq);
        seq
    }

    /// Applies a profile read. `name` is the profile's `displayName`: `None`
    /// when the server sent no such field. A failed read changes nothing, so
    /// the field stays as it was (hidden, if it was never read).
    pub fn loaded(&mut self, name: Result<Option<String>, ()>, seq: u64) {
        if self.loading != Some(seq) {
            return;
        }
        self.loading = None;
        // A save in flight will report the newer value.
        if self.saving {
            return;
        }
        if let Ok(name) = name {
            self.set_saved(name.map(|n| shown_or_empty(&n)));
        }
    }

    /// The field changed. Typing is capped at the longest name the API takes.
    pub fn edit(&mut self, value: String) {
        if self.saving || self.saved.is_none() {
            return;
        }
        let value: String = value.chars().take(display_name::MAX_CHARS).collect();
        self.notice = None;
        self.draft = (Some(value.as_str()) != self.saved.as_deref()).then_some(value);
    }

    /// Starts a save when the field holds a name that differs from the saved
    /// one. Returns the name to send (empty to remove it) and the request's
    /// token, or `None` when nothing should be sent.
    pub fn begin_save(&mut self) -> Option<(String, u64)> {
        if !self.can_save() {
            return None;
        }
        let name = match display_name::normalize(self.draft.as_deref().unwrap_or_default()) {
            Ok(name) => name,
            Err(refusal) => {
                self.notice = Some(refusal.into());
                return None;
            }
        };
        if Some(name.as_str()) == self.saved.as_deref() {
            // Only spacing differed: there is nothing to save.
            self.draft = None;
            self.notice = None;
            return None;
        }
        self.saving = true;
        self.notice = None;
        // A profile read still in flight predates this save.
        self.loading = None;
        let seq = self.next();
        self.sending = Some(seq);
        Some((name, seq))
    }

    /// Applies the result of a save. `Ok` carries the `displayName` of the
    /// profile the server answered with (`None` when it sent no such field).
    /// Returns the failure for the caller to act on, or `None` when the
    /// result was applied or was stale.
    pub fn save_finished(
        &mut self,
        result: Result<Option<String>, YourNameFailure>,
        seq: u64,
    ) -> Option<YourNameFailure> {
        if self.sending != Some(seq) {
            return None;
        }
        self.sending = None;
        self.saving = false;
        match result {
            Ok(Some(name)) => {
                let name = shown_or_empty(&name);
                self.notice = Some(if name.is_empty() {
                    YourNameNotice::Removed
                } else {
                    YourNameNotice::Saved
                });
                self.saved = Some(name);
                self.draft = None;
                None
            }
            // The server accepted the request but knows no name field, so
            // nothing was stored. What was typed stays in the field.
            Ok(None) => {
                self.notice = Some(YourNameNotice::NotSaved);
                None
            }
            Err(YourNameFailure::Refused(refusal)) => {
                self.notice = Some(refusal.into());
                None
            }
            Err(YourNameFailure::Other) => {
                self.notice = Some(YourNameNotice::NotSaved);
                None
            }
            Err(YourNameFailure::SessionEnded) => Some(YourNameFailure::SessionEnded),
        }
    }

    /// Forget everything on sign-out; results still in flight are dropped.
    pub fn reset(&mut self) {
        *self = Self {
            seq: self.seq.wrapping_add(1),
            ..Self::default()
        };
    }

    fn set_saved(&mut self, name: Option<String>) {
        if name.is_none() {
            self.draft = None;
            self.notice = None;
        } else if self.draft.as_deref() == name.as_deref() {
            self.draft = None;
        }
        self.saved = name;
    }

    fn next(&mut self) -> u64 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }
}

impl From<display_name::Refusal> for YourNameNotice {
    fn from(refusal: display_name::Refusal) -> Self {
        match refusal {
            display_name::Refusal::TooLong => Self::TooLong,
            display_name::Refusal::NotAllowed => Self::NotAllowed,
        }
    }
}

/// A received name as this app shows it: normalised, or empty when the server
/// sent something that is not a name this app accepts.
fn shown_or_empty(name: &str) -> String {
    display_name::shown(Some(name)).unwrap_or_default()
}

/// The `displayName` of a profile the server answered with, for
/// [`YourNameState::loaded`] and [`YourNameState::save_finished`].
pub fn profile_name(user: &User) -> Option<String> {
    user.display_name.clone()
}

/// An auth refusal (401/403) means the session ended and goes to the app's
/// sign-out handling. A 400 is the API refusing the name; its message says
/// which rule, and never repeats the name.
pub fn save_failure(error: &CoincubeError) -> YourNameFailure {
    if error.is_auth_error() {
        return YourNameFailure::SessionEnded;
    }
    match error {
        CoincubeError::Unsuccessful(info) if info.status_code == 400 => {
            YourNameFailure::Refused(if info.text.contains("60 characters") {
                display_name::Refusal::TooLong
            } else {
                display_name::Refusal::NotAllowed
            })
        }
        _ => YourNameFailure::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_name(name: &str) -> YourNameState {
        let mut state = YourNameState::default();
        let seq = state.begin_load();
        state.loaded(Ok(Some(name.to_string())), seq);
        state
    }

    #[test]
    fn hidden_until_the_profile_carries_a_name_field() {
        let mut state = YourNameState::default();
        assert!(state.saved.is_none());
        state.edit("Ama".into());
        assert!(state.draft.is_none(), "nothing to edit before the read");

        let seq = state.begin_load();
        state.loaded(Ok(None), seq);
        assert!(state.saved.is_none(), "an older server sends no field");

        let seq = state.begin_load();
        state.loaded(Err(()), seq);
        assert!(state.saved.is_none(), "a failed read shows nothing");

        let seq = state.begin_load();
        state.loaded(Ok(Some(String::new())), seq);
        assert_eq!(state.saved.as_deref(), Some(""));
        assert_eq!(state.value(), "");
    }

    #[test]
    fn saves_a_name() {
        let mut state = with_name("");
        state.edit("  Ama   Adeyemi ".into());
        assert!(state.can_save());
        let (name, seq) = state.begin_save().unwrap();
        assert_eq!(name, "Ama Adeyemi");
        assert!(state.saving);
        assert!(state.begin_save().is_none(), "one save at a time");
        assert!(state
            .save_finished(Ok(Some("Ama Adeyemi".into())), seq)
            .is_none());
        assert!(!state.saving);
        assert_eq!(state.saved.as_deref(), Some("Ama Adeyemi"));
        assert_eq!(state.value(), "Ama Adeyemi");
        assert_eq!(state.notice, Some(YourNameNotice::Saved));
        assert!(!state.can_save());
    }

    #[test]
    fn a_blank_field_removes_the_name() {
        let mut state = with_name("Ama Adeyemi");
        state.edit("   ".into());
        let (name, seq) = state.begin_save().unwrap();
        assert_eq!(name, "");
        assert!(state.save_finished(Ok(Some(String::new())), seq).is_none());
        assert_eq!(state.saved.as_deref(), Some(""));
        assert_eq!(state.value(), "");
        assert_eq!(state.notice, Some(YourNameNotice::Removed));
    }

    #[test]
    fn typing_is_capped_and_unchanged_text_is_not_a_draft() {
        let mut state = with_name("Ama");
        state.edit("a".repeat(80));
        assert_eq!(state.value().chars().count(), display_name::MAX_CHARS);
        state.edit("Ama".into());
        assert!(state.draft.is_none());
        assert!(!state.can_save());
        // Only spacing differs: nothing is sent and the field tidies up.
        state.edit("Ama ".into());
        assert!(state.begin_save().is_none());
        assert!(state.draft.is_none());
        assert!(!state.saving);
    }

    #[test]
    fn a_name_the_rules_refuse_is_not_sent() {
        let mut state = with_name("");
        state.edit("ama@example.com".into());
        assert!(state.begin_save().is_none());
        assert!(!state.saving);
        assert_eq!(state.notice, Some(YourNameNotice::NotAllowed));
        assert_eq!(state.value(), "ama@example.com", "the text is kept");
        state.edit("Ama".into());
        assert_eq!(state.notice, None, "editing clears the notice");
    }

    #[test]
    fn a_server_refusal_or_failure_keeps_what_was_typed() {
        for (failure, notice) in [
            (
                YourNameFailure::Refused(display_name::Refusal::TooLong),
                YourNameNotice::TooLong,
            ),
            (
                YourNameFailure::Refused(display_name::Refusal::NotAllowed),
                YourNameNotice::NotAllowed,
            ),
            (YourNameFailure::Other, YourNameNotice::NotSaved),
        ] {
            let mut state = with_name("Ama");
            state.edit("Ama A".into());
            let (_, seq) = state.begin_save().unwrap();
            assert!(state.save_finished(Err(failure), seq).is_none());
            assert!(!state.saving);
            assert_eq!(state.notice, Some(notice));
            assert_eq!(state.saved.as_deref(), Some("Ama"));
            assert_eq!(state.value(), "Ama A");
            assert!(state.can_save(), "Save is the retry");
        }
    }

    #[test]
    fn a_server_without_the_field_stores_nothing_and_says_so() {
        let mut state = with_name("");
        state.edit("Ama".into());
        let (_, seq) = state.begin_save().unwrap();
        assert!(state.save_finished(Ok(None), seq).is_none());
        assert_eq!(state.notice, Some(YourNameNotice::NotSaved));
        assert_eq!(state.saved.as_deref(), Some(""));
        assert_eq!(state.value(), "Ama");
    }

    #[test]
    fn an_ended_session_is_returned_to_the_caller() {
        let mut state = with_name("");
        state.edit("Ama".into());
        let (_, seq) = state.begin_save().unwrap();
        assert!(matches!(
            state.save_finished(Err(YourNameFailure::SessionEnded), seq),
            Some(YourNameFailure::SessionEnded)
        ));
    }

    #[test]
    fn stale_results_are_dropped() {
        let mut state = with_name("Ama");
        state.edit("Kofi".into());
        let (_, seq) = state.begin_save().unwrap();
        state.reset();
        assert!(state.save_finished(Ok(Some("Kofi".into())), seq).is_none());
        assert!(state.saved.is_none(), "signed out: nothing is applied");

        // A read begun before a save does not overwrite the save's result.
        let mut state = with_name("Ama");
        let read = state.begin_load();
        state.edit("Kofi".into());
        let (_, save) = state.begin_save().unwrap();
        state.loaded(Ok(Some("Ama".into())), read);
        assert!(state.saving);
        assert!(state.save_finished(Ok(Some("Kofi".into())), save).is_none());
        assert_eq!(state.saved.as_deref(), Some("Kofi"));

        // A superseded read is dropped.
        let mut state = YourNameState::default();
        let first = state.begin_load();
        let second = state.begin_load();
        state.loaded(Ok(Some("Old".into())), first);
        assert!(state.saved.is_none());
        state.loaded(Ok(Some("New".into())), second);
        assert_eq!(state.saved.as_deref(), Some("New"));
    }

    #[test]
    fn a_reload_keeps_a_draft_and_drops_one_that_now_matches() {
        let mut state = with_name("Ama");
        state.edit("Ama Adeyemi".into());
        let seq = state.begin_load();
        state.loaded(Ok(Some("Ama".into())), seq);
        assert_eq!(state.value(), "Ama Adeyemi");
        // Saved from another device meanwhile.
        let seq = state.begin_load();
        state.loaded(Ok(Some("Ama Adeyemi".into())), seq);
        assert!(state.draft.is_none());
        assert_eq!(state.value(), "Ama Adeyemi");
    }

    #[test]
    fn a_received_name_this_app_would_refuse_is_shown_as_none() {
        let state = with_name("boss@bank.example");
        assert_eq!(state.saved.as_deref(), Some(""));
    }

    #[test]
    fn failures_are_classified_without_reading_the_name() {
        use crate::services::http::NotSuccessResponseInfo;
        let unsuccessful = |status_code, text: &str| {
            CoincubeError::Unsuccessful(NotSuccessResponseInfo {
                status_code,
                text: text.to_string(),
            })
        };
        assert!(matches!(
            save_failure(&unsuccessful(401, "")),
            YourNameFailure::SessionEnded
        ));
        assert!(matches!(
            save_failure(&unsuccessful(
                400,
                r#"{"success":false,"error":{"code":"BAD_REQUEST","message":"Display name must be 60 characters or fewer"}}"#
            )),
            YourNameFailure::Refused(display_name::Refusal::TooLong)
        ));
        assert!(matches!(
            save_failure(&unsuccessful(
                400,
                r#"{"success":false,"error":{"code":"BAD_REQUEST","message":"Display name contains characters that are not allowed"}}"#
            )),
            YourNameFailure::Refused(display_name::Refusal::NotAllowed)
        ));
        assert!(matches!(
            save_failure(&unsuccessful(500, "boom")),
            YourNameFailure::Other
        ));
        assert!(matches!(
            save_failure(&CoincubeError::Api("offline".into())),
            YourNameFailure::Other
        ));
    }
}
