//! Explicit publication of accepted display preferences to the current account.

use super::{preferences_view, Message, GUI};
use crate::services::coincube::worldview::{Error, Selection, Snapshot};
use coincube_ui::locale::copy;
use iced::Task;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Refresh,
    Publish,
    Revoke,
}

#[derive(Debug, Default)]
pub struct Sharing {
    context: Option<(u32, u64, u64)>,
    pending: Option<uuid::Uuid>,
    snapshot: Option<Snapshot>,
    error: Option<Error>,
}

impl Sharing {
    fn complete(&mut self, request: uuid::Uuid, result: Result<Snapshot, Error>) {
        if self.pending != Some(request) {
            return;
        }
        self.pending = None;
        match result {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.error = None;
            }
            Err(error) => {
                // A write may have reached Connect even if its reply was lost.
                // Require a fresh read and another explicit user action.
                self.snapshot = None;
                self.error = Some(error);
            }
        }
    }
}

impl GUI {
    fn worldview_context(&self) -> Option<(u32, u64, u64)> {
        let sheet = self.preferences_sheet.as_ref()?;
        let account = self.connect_panel(&sheet.origin)?;
        if !account.is_authenticated() {
            return None;
        }
        Some((
            account.user.as_ref()?.id,
            account.session_generation(),
            account.auth_epoch(),
        ))
    }

    pub(super) fn worldview_view(&self) -> preferences_view::WorldviewSharing {
        let Some(context) = self.worldview_context() else {
            return preferences_view::WorldviewSharing {
                status: copy("Sign in to Connect to share with Keychain."),
                ..Default::default()
            };
        };
        let Some(sheet) = &self.preferences_sheet else {
            return Default::default();
        };
        let sharing = &sheet.worldview_sharing;
        let same_context = sharing.context == Some(context);
        let pending = same_context && sharing.pending.is_some();
        let snapshot = same_context.then_some(sharing.snapshot.as_ref()).flatten();
        let error = same_context.then_some(sharing.error).flatten();
        let status = if pending {
            copy("Checking or updating the shared selection…")
        } else if let Some(error) = error {
            match error {
                Error::Conflict => copy(
                    "The shared selection changed. Refresh, then choose Share or Stop sharing again.",
                ),
                Error::Locked => copy("Sharing changes are unavailable while Connect is locked."),
                Error::Unavailable => {
                    copy("Couldn’t confirm the shared selection. Refresh before trying again.")
                }
            }
        } else if snapshot.is_some_and(|value| value.selection.is_some()) {
            copy("Shared with Keychain")
        } else if snapshot.is_some() {
            copy("No Worldview selection is shared with Keychain.")
        } else {
            copy("Refresh to check the selection shared with Keychain.")
        };
        let ready = !self.preferences.is_busy()
            && !self.preferences.has_queued_changes()
            && self.preferences.accepted().is_some();
        preferences_view::WorldviewSharing {
            status,
            shared_selection: snapshot
                .and_then(|value| value.selection)
                .map(selection_names),
            can_refresh: !pending,
            can_publish: !pending && ready && snapshot.is_some(),
            can_revoke: !pending && snapshot.is_some_and(|value| value.selection.is_some()),
        }
    }

    pub(super) fn worldview_action(&mut self, action: Action) -> Task<Message> {
        let Some(context) = self.worldview_context() else {
            return Task::none();
        };
        let Some(sheet) = self.preferences_sheet.as_ref() else {
            return Task::none();
        };
        if !self.overlay_origin_is_current(&sheet.origin) {
            return Task::none();
        }
        let Some(client) = self
            .connect_panel(&sheet.origin)
            .and_then(|panel| panel.authenticated_client())
        else {
            return Task::none();
        };
        let view = self.worldview_view();
        let selection = match action {
            Action::Refresh if view.can_refresh => None,
            Action::Publish if view.can_publish => self
                .preferences
                .accepted()
                .map(|saved| Selection::from(saved.preferences)),
            Action::Revoke if view.can_revoke => None,
            _ => return Task::none(),
        };
        let previous = sheet.worldview_sharing.snapshot.clone();
        let origin = sheet.origin.clone();
        let request = uuid::Uuid::new_v4();
        let sharing = &mut self.preferences_sheet.as_mut().unwrap().worldview_sharing;
        if sharing.context != Some(context) {
            sharing.snapshot = None;
        }
        sharing.context = Some(context);
        sharing.pending = Some(request);
        sharing.error = None;
        Task::perform(
            async move {
                match action {
                    Action::Refresh => client.get_worldview().await,
                    Action::Publish | Action::Revoke => {
                        let current = previous.ok_or(Error::Unavailable)?;
                        client.share_worldview(&current, selection).await
                    }
                }
            },
            move |result| Message::Worldview(origin.clone(), context, request, result),
        )
    }

    pub(super) fn worldview_complete(
        &mut self,
        origin: super::OverlayOrigin,
        context: (u32, u64, u64),
        request: uuid::Uuid,
        result: Result<Snapshot, Error>,
    ) {
        if self.worldview_context() != Some(context) || !self.overlay_origin_is_current(&origin) {
            return;
        }
        if let Some(sheet) = self
            .preferences_sheet
            .as_mut()
            .filter(|sheet| sheet.origin == origin)
        {
            sheet.worldview_sharing.complete(request, result);
        }
    }
}

fn selection_names(selection: Selection) -> (&'static str, &'static str) {
    use crate::app::settings::preferences::{ArtworkStyle::*, WorldviewMode::*};
    match (selection.mode, selection.artwork) {
        (Samurai, Existing) => ("Samurai", "Kage"),
        (Samurai, Scene) => ("Samurai", "Mountain Watch"),
        (Suitcoiner, Existing) => ("Suitcoiner", "Gilded Wall Street"),
        (Suitcoiner, Scene) => ("Suitcoiner", "Gilded Exchange"),
        (Luke, Existing) => ("Luke", "Purist’s Woodcut"),
        (Luke, Scene) => ("Luke", "The Stone Crossing"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_completions_cannot_replace_a_new_request() {
        let mut sharing = Sharing::default();
        let old = uuid::Uuid::new_v4();
        let new = uuid::Uuid::new_v4();
        sharing.pending = Some(new);
        sharing.complete(old, Err(Error::Locked));
        assert_eq!(sharing.pending, Some(new));
        assert_eq!(sharing.error, None);
        sharing.complete(new, Err(Error::Conflict));
        assert_eq!(sharing.pending, None);
        assert_eq!(sharing.error, Some(Error::Conflict));
        assert_eq!(sharing.snapshot, None);
    }
    #[test]
    fn account_replacement_and_closed_sheets_refuse_old_results() {
        let _guard = crate::app::session::test_guard();
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let _enter = runtime.enter();
                let path = std::env::temp_dir()
                    .join(format!("worldview-sharing-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir_all(&path).unwrap();
                let root = crate::dir::CoincubeDirectory::new(path.clone());
                let mut gui = super::super::app_preferences_sheet_tests::launcher_gui(&root);
                let pane_id = gui.focus.unwrap();
                let tab_id = gui.panes.get(pane_id).unwrap().tabs[0].id;
                let super::super::tab::State::Home(home) =
                    &mut gui.panes.get_mut(pane_id).unwrap().tabs[0].state
                else {
                    unreachable!()
                };
                home.connect_account.user = Some(crate::services::coincube::User {
                    id: 7,
                    email: "fixture@example.invalid".into(),
                    email_verified: Some(true),
                });
                home.connect_account.step = crate::app::state::connect::ConnectFlowStep::Dashboard;
                drop(gui.update(Message::Pane(
                    pane_id,
                    super::super::pane::Message::tab(
                        tab_id,
                        super::super::tab::Message::OpenAppPreferences(
                            super::super::tab::PreferencesSource::Home,
                        ),
                    ),
                )));
                let origin = gui.preferences_sheet.as_ref().unwrap().origin.clone();
                let context = gui.worldview_context().unwrap();
                let old = gui
                    .preferences_sheet
                    .as_ref()
                    .unwrap()
                    .worldview_sharing
                    .pending
                    .unwrap();
                gui.preferences_sheet
                    .as_mut()
                    .unwrap()
                    .worldview_sharing
                    .snapshot = Some(Snapshot {
                    account_uuid: "82473444-c619-4c95-bc6e-ef2d9f692e8b".into(),
                    revision: 1,
                    selection: Some(Selection {
                        mode: crate::app::settings::preferences::WorldviewMode::Luke,
                        artwork: crate::app::settings::preferences::ArtworkStyle::Scene,
                    }),
                    etag: "\"old-account-selection\"".into(),
                });
                let super::super::tab::State::Home(home) =
                    &mut gui.panes.get_mut(pane_id).unwrap().tabs[0].state
                else {
                    unreachable!()
                };
                home.connect_account.user.as_mut().unwrap().id = 8;
                gui.worldview_complete(origin.clone(), context, old, Err(Error::Locked));
                assert_eq!(
                    gui.preferences_sheet
                        .as_ref()
                        .unwrap()
                        .worldview_sharing
                        .error,
                    None
                );
                assert!(!gui.worldview_view().can_publish);
                assert!(!gui.worldview_view().can_revoke);
                assert_eq!(gui.worldview_view().shared_selection, None);
                drop(gui.worldview_action(Action::Refresh));
                assert_eq!(gui.worldview_view().shared_selection, None);
                assert!(gui
                    .preferences_sheet
                    .as_ref()
                    .unwrap()
                    .worldview_sharing
                    .snapshot
                    .is_none());
                let active_context = gui.worldview_context().unwrap();
                let active = gui
                    .preferences_sheet
                    .as_ref()
                    .unwrap()
                    .worldview_sharing
                    .pending
                    .unwrap();
                gui.worldview_complete(
                    origin.clone(),
                    active_context,
                    active,
                    Err(Error::Conflict),
                );
                assert!(!gui.worldview_view().can_publish);
                assert!(gui.worldview_view().can_refresh);
                drop(gui.close_preferences_sheet());
                gui.worldview_complete(origin, active_context, active, Err(Error::Locked));
                assert!(gui.preferences_sheet.is_none());
                drop(gui);
                std::fs::remove_dir_all(path).unwrap();
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
