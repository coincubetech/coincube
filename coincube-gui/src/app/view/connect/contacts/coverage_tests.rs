use super::*;
use crate::app::state::connect::{AddToCubeDialog, InviteCubeOption};
use crate::app::view::shared::landing::capture;
use crate::services::coincube::{Contact, ContactCube, ReceivedInviteCube};

fn inspect(element: Element<'_, ConnectAccountMessage>) -> capture::Inspection {
    capture::inspect_rendered_full_window(
        "contacts-behavior",
        element,
        (1024, 1600),
        "dark",
        theme::Theme::dark(),
    )
}

fn cube() -> InviteCubeOption {
    InviteCubeOption {
        id: 9,
        name: "Synthetic Cube".into(),
        network: "bitcoin".into(),
    }
}

fn received() -> ReceivedInvite {
    ReceivedInvite {
        id: 7,
        owner_email: "sender@example.invalid".into(),
        owner_display_name: None,
        role: ContactRole::Keyholder,
        expires_at: "2099-01-01T00:00:00Z".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        cubes: vec![],
    }
}

fn has_submit(messages: &[ConnectAccountMessage]) -> bool {
    messages.iter().any(|m| {
        matches!(
            m,
            ConnectAccountMessage::Contacts(ContactsMessage::SubmitInvite)
        )
    })
}

#[test]
fn list_transitions_from_loading_to_empty_and_received_invites() {
    let mut state = ConnectAccountPanel::new();
    state.contacts_state.loading = true;
    assert!(inspect(contacts_ux(&state)).contains("Loading…"));
    state.contacts_state.contacts = Some(vec![]);
    assert!(inspect(contacts_ux(&state)).contains("No contacts yet"));
    state.contacts_state.received_invites = Some(vec![received()]);
    let view = inspect(contacts_ux(&state));
    assert!(view.contains("Received invites"));
    assert!(view.contains("sender@example.invalid"));
    assert!(!view.contains("No contacts yet"));
    state.contacts_state.loading = false;
    state.contacts_state.error = Some("Synthetic load failure".into());
    assert!(inspect(contacts_ux(&state)).contains("Synthetic load failure"));
}

#[test]
fn invite_accept_button_emits_one_id_and_disables_while_accepting() {
    let invite = received();
    let messages = capture::click_text(
        received_invite_card(&invite, false),
        "Accept",
        0,
        (1024, 1000),
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| matches!(
                m,
                ConnectAccountMessage::Contacts(ContactsMessage::AcceptReceivedInvite(7))
            ))
            .count(),
        1
    );
    assert!(inspect(received_invite_card(&invite, true)).contains("Accepting…"));
    assert!(capture::click_text(
        received_invite_card(&invite, true),
        "Accepting…",
        0,
        (1024, 1000)
    )
    .is_empty());
}

#[test]
fn outbound_invites_expose_resend_and_revoke_only_for_pending_rows() {
    let mut state = ConnectAccountPanel::new();
    let invite = Invite {
        id: 12,
        owner_user_id: 1,
        invitee_email: "invitee@example.invalid".into(),
        invitee_user_id: None,
        role: ContactRole::Beneficiary,
        status: "pending".into(),
        expires_at: "2099-01-01T00:00:00Z".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
    };
    state.contacts_state.invites = Some(vec![invite.clone()]);
    assert!(inspect(contacts_ux(&state)).contains("Pending invites"));
    for (label, revoke) in [("Resend", false), ("Revoke", true)] {
        let messages = capture::click_text(invite_card(&invite), label, 0, (1024, 1000));
        assert!(messages.iter().any(|m| match m {
            ConnectAccountMessage::Contacts(ContactsMessage::RevokeInvite(12)) => revoke,
            ConnectAccountMessage::Contacts(ContactsMessage::ResendInvite(12)) => !revoke,
            _ => false,
        }));
    }
    state.contacts_state.invites.as_mut().unwrap()[0].status = "accepted".into();
    assert!(!inspect(contacts_ux(&state)).contains("invitee@example.invalid"));
}

#[test]
fn email_validation_gates_submit_and_busy_forms_cannot_resubmit() {
    let mut state = ConnectAccountPanel::new();
    state.contacts_state.step = ContactsStep::InviteForm;
    state.contacts_state.invite_available_cubes = Some(vec![cube()]);
    state.contacts_state.invite_cube_selections = vec![9];
    for (email, valid) in [
        ("", false),
        ("missing-at", false),
        ("a@localhost", false),
        ("synthetic@example.invalid", true),
        ("  synthetic@example.invalid  ", true),
    ] {
        state.contacts_state.invite_email = email.into();
        let messages = capture::click_text(contacts_ux(&state), "Send Invite", 0, (1024, 1600));
        assert_eq!(has_submit(&messages), valid, "{}", email);
    }
    state.contacts_state.invite_sending = true;
    assert!(inspect(contacts_ux(&state)).contains("Sending…"));
    let messages = capture::click_text(contacts_ux(&state), "Sending…", 0, (1024, 1600));
    assert!(!has_submit(&messages));
}

#[test]
fn invite_cube_conflicts_keep_form_values_and_offer_reselection() {
    let mut state = ConnectAccountPanel::new();
    state.contacts_state.step = ContactsStep::InviteForm;
    state.contacts_state.invite_available_cubes = Some(vec![cube()]);
    state.contacts_state.invite_cube_selections = vec![9];
    state.contacts_state.invite_cube_error = Some("Synthetic membership changed".into());
    state.contacts_state.error = Some("Synthetic invite failure".into());
    let view = inspect(contacts_ux(&state));
    assert!(view.contains("Synthetic Cube (bitcoin)"));
    assert!(view.contains("Synthetic membership changed"));
    assert!(view.contains("Synthetic invite failure"));
    assert_eq!(state.contacts_state.invite_cube_selections, [9]);
}

#[test]
fn contact_detail_distinguishes_missing_loading_retry_and_recovery_kit() {
    let mut state = ConnectAccountPanel::new();
    state.contacts_state.step = ContactsStep::Detail(5);
    assert!(inspect(contacts_ux(&state)).contains("Contact not found"));
    let contact: Contact = serde_json::from_value(serde_json::json!({"id":5, "role":"keyholder", "contactUser":{"id":6, "email":"contact@example.invalid"}, "createdAt":"2026-01-01T00:00:00Z"})).unwrap();
    state.contacts_state.contacts = Some(vec![contact]);
    let view = inspect(contacts_ux(&state));
    assert!(view.contains("contact@example.invalid"));
    assert!(view.contains("Loading cubes…"));
    assert!(view.contains("Add to Cube(s)…"));
    state.contacts_state.detail_cubes_error = Some("Synthetic cube fetch failure".into());
    assert!(inspect(contacts_ux(&state)).contains("Synthetic cube fetch failure"));
    let messages = capture::click_text(contacts_ux(&state), "Retry", 0, (1024, 1600));
    assert!(messages.iter().any(|m| matches!(
        m,
        ConnectAccountMessage::Contacts(ContactsMessage::ShowDetail(5))
    )));
    state.contacts_state.detail_cubes = Some(vec![]);
    assert!(inspect(contacts_ux(&state)).contains("No cubes found"));
    state.contacts_state.detail_cubes = Some(vec![ContactCube {
        id: 9,
        uuid: "synthetic-cube".into(),
        name: "Synthetic Cube".into(),
        network: "bitcoin".into(),
        has_recovery_kit: true,
    }]);
    let view = inspect(contacts_ux(&state));
    assert!(view.contains("Synthetic Cube") && view.contains("Recovery Kit"));
    state.contacts_state.contacts.as_mut().unwrap()[0].contact_user = None;
    let view = inspect(contacts_ux(&state));
    assert!(view.contains("unknown contact"));
    assert!(!view.contains("Add to Cube(s)…"));
}

#[test]
fn add_to_cube_dialog_requires_loaded_selection_and_prevents_duplicate_submit() {
    let mut dialog = AddToCubeDialog {
        contact_id: 5,
        contact_email: "contact@example.invalid".into(),
        candidate_cubes: None,
        selections: vec![],
        submitting: false,
        failures: Default::default(),
    };
    assert!(inspect(add_to_cube_dialog_card(&dialog)).contains("Loading eligible cubes…"));
    dialog.candidate_cubes = Some(vec![]);
    assert!(inspect(add_to_cube_dialog_card(&dialog)).contains("No eligible cubes"));
    dialog.candidate_cubes = Some(vec![cube()]);
    let click = |d: &AddToCubeDialog, label| {
        capture::click_text(add_to_cube_dialog_card(d), label, 0, (1024, 1600))
    };
    assert!(!click(&dialog, "Add").iter().any(|m| matches!(
        m,
        ConnectAccountMessage::Contacts(ContactsMessage::ConfirmAddToCube)
    )));
    dialog.selections = vec![9];
    assert!(click(&dialog, "Add").iter().any(|m| matches!(
        m,
        ConnectAccountMessage::Contacts(ContactsMessage::ConfirmAddToCube)
    )));
    dialog
        .failures
        .insert(9, "Synthetic per-cube failure".into());
    assert!(inspect(add_to_cube_dialog_card(&dialog)).contains("Synthetic per-cube failure"));
    dialog.submitting = true;
    assert!(!click(&dialog, "Adding…").iter().any(|m| matches!(
        m,
        ConnectAccountMessage::Contacts(ContactsMessage::ConfirmAddToCube)
    )));
}

#[test]
fn expiry_labels_use_utc_calendar_days_and_invalid_dates_are_unknown() {
    let today = chrono::NaiveDate::from_ymd_opt(2024, 2, 29).unwrap();
    for (days, label) in [
        (-2, "Expired"),
        (0, "Expires today"),
        (1, "Expires in 1 day"),
        (5, "Expires in 5 days"),
    ] {
        let date = (today + chrono::Duration::days(days))
            .format("%Y-%m-%dT12:00:00Z")
            .to_string();
        let days_until = compute_days_until_at(&date, today);
        assert_eq!(days_until, Some(days));
        assert!(inspect(expiry_element_for_days(days_until)).contains(label));
    }
    assert_eq!(compute_days_until_at("invalid", today), None);
    for role in [
        ContactRole::Keyholder,
        ContactRole::Beneficiary,
        ContactRole::Observer,
        ContactRole::Owner,
        ContactRole::Unknown,
    ] {
        let c = role_color(&role);
        assert!(c.a > 0.0);
    }
}

fn contact(display_name: Option<&str>) -> Contact {
    let mut value = serde_json::json!({
        "id": 5, "role": "keyholder", "createdAt": "2026-01-01T00:00:00Z",
        "contactUser": {"id": 6, "email": "contact@example.invalid"}
    });
    if let Some(name) = display_name {
        value["contactUser"]["displayName"] = serde_json::json!(name);
    }
    serde_json::from_value(value).unwrap()
}

#[test]
fn a_contact_row_shows_the_name_with_the_email_and_the_email_alone_without_one() {
    let mut state = ConnectAccountPanel::new();
    state.contacts_state.contacts = Some(vec![contact(Some("Casey Example"))]);
    let view = inspect(contacts_ux(&state));
    assert!(view.contains("Casey Example"));
    assert!(view.contains("contact@example.invalid"));

    // No name set (""), an older server (no field), and a "name" that reads
    // as an address all show the email alone.
    for name in [Some(""), None, Some("boss@bank.example")] {
        state.contacts_state.contacts = Some(vec![contact(name)]);
        let view = inspect(contacts_ux(&state));
        assert!(view.contains("contact@example.invalid"), "{:?}", name);
        assert!(!view.contains("Casey Example"), "{:?}", name);
        assert!(!view.contains("boss@bank.example"), "{:?}", name);
    }
}

#[test]
fn contact_detail_keeps_the_email_under_the_name() {
    let mut state = ConnectAccountPanel::new();
    state.contacts_state.step = ContactsStep::Detail(5);
    state.contacts_state.contacts = Some(vec![contact(Some("Casey Example"))]);
    let view = inspect(contacts_ux(&state));
    assert!(view.contains("Casey Example"));
    assert!(view.contains("contact@example.invalid"));
    // The page where the contact is removed or added to a Cube still names
    // them by email.
    assert!(view.contains("Add to Cube(s)…"));
}

#[test]
fn a_received_invitation_names_the_sender_and_always_shows_their_email() {
    let mut invite = received();
    invite.owner_display_name = Some("Sam Example".into());
    let view = inspect(received_invite_card(&invite, false));
    assert!(view.contains("Sam Example"));
    assert!(view.contains("sender@example.invalid"));
    assert!(view.contains("Accept"));
    // Accepting is still one click on the same invite.
    let messages = capture::click_text(
        received_invite_card(&invite, false),
        "Accept",
        0,
        (1024, 1000),
    );
    assert!(messages.iter().any(|m| matches!(
        m,
        ConnectAccountMessage::Contacts(ContactsMessage::AcceptReceivedInvite(7))
    )));

    for name in [None, Some(String::new())] {
        invite.owner_display_name = name;
        let view = inspect(received_invite_card(&invite, false));
        assert!(view.contains("sender@example.invalid"));
        assert!(!view.contains("Sam Example"));
    }
}

// ── Cube avatars CD4: invitations are for a Cube you own ──────────────

fn invite_form(cubes: Option<Vec<InviteCubeOption>>) -> ConnectAccountPanel {
    let mut state = ConnectAccountPanel::new();
    state.contacts_state.step = ContactsStep::InviteForm;
    state.contacts_state.invite_available_cubes = cubes;
    state.contacts_state.invite_email = "synthetic@example.invalid".into();
    state
}

const NO_CUBE: &str = "Invitations are for a Cube you own.";

#[test]
fn send_needs_a_cube_ticked() {
    let mut state = invite_form(Some(vec![cube()]));
    let view = inspect(contacts_ux(&state));
    assert!(view.contains("For which Cubes?"));
    assert!(view.contains("Synthetic Cube (bitcoin)"));
    assert!(!view.contains("(optional)"));
    let messages = capture::click_text(contacts_ux(&state), "Send Invite", 0, (1024, 1600));
    assert!(
        !has_submit(&messages),
        "Send is disabled with no Cube ticked"
    );

    state.contacts_state.invite_cube_selections = vec![9];
    let messages = capture::click_text(contacts_ux(&state), "Send Invite", 0, (1024, 1600));
    assert!(has_submit(&messages), "Send is enabled with a Cube ticked");
}

#[test]
fn no_cube_replaces_the_form_with_one_line() {
    let state = invite_form(Some(vec![]));
    let view = inspect(contacts_ux(&state));
    assert!(view.contains(NO_CUBE));
    assert!(view.contains("Invite Contact"));
    assert!(!view.contains("Email Address"));
    assert!(!view.contains("Send Invite"));
    // Back still returns to the list.
    let messages = capture::click_text(contacts_ux(&state), "Back", 0, (1024, 1600));
    assert!(messages.iter().any(|m| matches!(
        m,
        ConnectAccountMessage::Contacts(ContactsMessage::BackToList)
    )));
}

/// A send refused because the only Cube is gone reloads an empty list: the
/// no-Cube line replaces the form, and the conflict still says why.
#[test]
fn a_refused_send_that_leaves_no_cube_still_explains_itself() {
    let mut state = invite_form(Some(vec![]));
    state.contacts_state.invite_cube_error = Some("Synthetic refusal".into());
    let view = inspect(contacts_ux(&state));
    assert!(view.contains(NO_CUBE));
    assert!(view.contains("One or more selected cubes is no longer available"));
    assert!(view.contains("Synthetic refusal"));
    assert!(!view.contains("Send Invite"));
}

#[test]
fn the_form_waits_for_the_cube_list_without_flashing_the_no_cube_line() {
    let mut state = invite_form(None);
    let view = inspect(contacts_ux(&state));
    assert!(view.contains("Loading…"));
    assert!(!view.contains(NO_CUBE));
    assert!(!view.contains("Send Invite"));

    // A failed load says why and offers a retry; it is not "no Cube".
    state.contacts_state.invite_cubes_load_error = Some("Synthetic outage".into());
    let view = inspect(contacts_ux(&state));
    assert!(view.contains("Synthetic outage"));
    assert!(!view.contains(NO_CUBE));
    let messages = capture::click_text(contacts_ux(&state), "Retry", 0, (1024, 1600));
    assert!(messages.iter().any(|m| matches!(
        m,
        ConnectAccountMessage::Contacts(ContactsMessage::ShowInviteForm)
    )));
}

fn invite_cube(id: u64, name: &str, uuid: Option<&str>, variation: u32) -> ReceivedInviteCube {
    ReceivedInviteCube {
        cube_id: id,
        cube_name: name.into(),
        network: Some("mainnet".into()),
        cube_uuid: uuid.map(str::to_string),
        mark_variation: Some(variation),
        mark_version: Some(1),
    }
}

const UUID_A: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const UUID_B: &str = "6fa459ea-ee8a-4ca4-894e-db77e160355e";

#[test]
fn a_received_invitation_names_one_cube_with_its_mark_and_the_sender() {
    let mut invite = received();
    invite.cubes = vec![invite_cube(4, "Family Cube", Some(UUID_A), 0)];
    let view = inspect(received_invite_card(&invite, false));
    assert!(view.contains("Family Cube"));
    assert!(
        view.contains("Bitcoin mainnet"),
        "the network is beside the name"
    );
    assert!(view.contains("sender@example.invalid"));
    assert!(view.contains("Accept"));
    assert!(invite.cubes[0].has_mark());
    assert_eq!(invite.cubes[0].mark_seed().as_deref(), Some(UUID_A));
}

#[test]
fn a_received_invitation_names_two_cubes_each_with_its_own_mark() {
    let mut invite = received();
    let mut savings = invite_cube(5, "Savings Cube", Some(UUID_B), 0);
    savings.network = Some("testnet4".into());
    invite.cubes = vec![invite_cube(4, "Family Cube", Some(UUID_A), 3), savings];
    let view = inspect(received_invite_card(&invite, false));
    assert!(view.contains("Family Cube"));
    assert!(view.contains("Bitcoin mainnet"));
    assert!(view.contains("Savings Cube"));
    assert!(view.contains("Bitcoin testnet4"));
    assert!(view.contains("sender@example.invalid"));
    let seeds: Vec<Option<String>> = invite.cubes.iter().map(|c| c.mark_seed()).collect();
    assert_eq!(
        seeds,
        vec![Some(format!("{}:3", UUID_A)), Some(UUID_B.to_string())]
    );
    // Accepting is unchanged.
    let messages = capture::click_text(
        received_invite_card(&invite, false),
        "Accept",
        0,
        (1024, 1000),
    );
    assert!(messages.iter().any(|m| matches!(
        m,
        ConnectAccountMessage::Contacts(ContactsMessage::AcceptReceivedInvite(7))
    )));
}

/// No `cubes` (an older server or invitation): the row renders as before.
/// A Cube from a server without UUIDs is named with no mark.
#[test]
fn a_received_invitation_without_cubes_renders_as_before() {
    let invite = received();
    assert!(invite.cubes.is_empty());
    let view = inspect(received_invite_card(&invite, false));
    assert!(view.contains("sender@example.invalid"));
    assert!(view.contains("Keyholder"));
    assert!(view.contains("Accept"));

    let mut older = received();
    older.cubes = vec![invite_cube(4, "Family Cube", None, 0)];
    older.cubes[0].network = None;
    assert!(!older.cubes[0].has_mark());
    let view = inspect(received_invite_card(&older, false));
    assert!(view.contains("Family Cube"));
    assert!(view.contains("sender@example.invalid"));
    // No network: the line is the name alone.
    assert!(!view.contains("Bitcoin"));
}

#[test]
fn the_new_invite_strings_are_spanish() {
    use coincube_ui::locale::{copy, render_in, Language};
    for english in [NO_CUBE, "For which Cubes?"] {
        let spanish = render_in(Language::Spanish, || copy(english).to_string());
        assert_ne!(spanish, english, "{}", english);
    }
    let view = render_in(Language::Spanish, || {
        inspect(contacts_ux(&invite_form(Some(vec![]))))
    });
    assert!(view.contains("Las invitaciones son para un Cube que te pertenece."));
}
