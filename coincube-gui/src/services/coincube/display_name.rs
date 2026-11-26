//! A person's display name: the optional name a Connect user chose to be shown
//! beside their email (`displayName` on the API).
//!
//! A name is a self-asserted **label**. It is not unique or verified, it never
//! selects a person, and it never replaces the email: every row that shows a
//! name shows the email too, and code that identifies someone keeps using the
//! email or the id.
//!
//! The API normalises and validates a name when it is saved
//! (`models.NormalizeUserDisplayName` in coincube-api). [`normalize`] applies
//! the same rules here for two reasons: the "Your name" field can refuse a
//! name before sending it, and a name received from a server is shown only if
//! it passes ([`shown`]), so a server that skipped the checks cannot put
//! direction controls or a second address into a row.

/// The longest name, in Unicode scalar values, after whitespace is collapsed.
/// Matches the API's `UserDisplayNameMaxRunes`.
pub const MAX_CHARS: usize = 60;

/// How many characters of a name a one-line label keeps before an ellipsis, so
/// a long name cannot push the email beside it out of a clipped line.
const ONE_LINE_NAME_CHARS: usize = 32;

/// Why a typed name can't be saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// More than [`MAX_CHARS`] characters.
    TooLong,
    /// A character that isn't allowed, or nothing a reader could see.
    NotAllowed,
}

/// Turns typed or received text into the name that is stored and shown.
///
/// - Leading and trailing whitespace goes; an inner run becomes one space.
/// - Control characters and invisible format characters (the Unicode
///   direction controls among them) are refused. The zero-width joiner and
///   non-joiner are allowed: several scripts and emoji sequences need them.
/// - `@`, `<`, `>` and their fullwidth and small forms are refused, so a name
///   cannot read as a second email address.
/// - A name needs at least one letter or number.
/// - At most [`MAX_CHARS`] characters.
///
/// `Ok("")` means "no name": saving it clears the name.
pub fn normalize(raw: &str) -> Result<String, Refusal> {
    let mut visible = false;
    for c in raw.chars() {
        if c.is_whitespace() || is_joiner(c) {
            continue;
        }
        if c.is_control() || is_format(c) || is_blank_graphic(c) || is_address_punctuation(c) {
            return Err(Refusal::NotAllowed);
        }
        if c.is_alphanumeric() {
            visible = true;
        }
    }
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Ok(name);
    }
    if !visible {
        return Err(Refusal::NotAllowed);
    }
    if name.chars().count() > MAX_CHARS {
        return Err(Refusal::TooLong);
    }
    Ok(name)
}

/// The name to show for a person, from the `displayName` a response carried.
/// `None` when the field was absent (an older server), empty, or not a name
/// [`normalize`] accepts: the row then shows the email alone.
pub fn shown(raw: Option<&str>) -> Option<String> {
    normalize(raw?).ok().filter(|name| !name.is_empty())
}

/// One line naming a person: `Name · email`, or the email alone without a
/// name. The email is always whole; a long name is shortened instead.
pub fn one_line(name: Option<&str>, email: &str) -> String {
    match shown(name) {
        Some(name) => {
            let mut chars = name.chars();
            let head: String = chars.by_ref().take(ONE_LINE_NAME_CHARS).collect();
            if chars.next().is_some() {
                format!("{}… · {email}", head.trim_end())
            } else {
                format!("{head} · {email}")
            }
        }
        None => email.to_string(),
    }
}

/// U+200C zero-width non-joiner and U+200D zero-width joiner.
fn is_joiner(c: char) -> bool {
    matches!(c, '\u{200C}' | '\u{200D}')
}

/// Invisible format characters (Unicode category Cf), listed by range because
/// `std` has no general-category lookup. Includes the direction marks,
/// embeddings, overrides and isolates. The joiners are handled before this.
fn is_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    ) || is_unassigned_or_private(c)
}

/// Line and paragraph separators are whitespace to Rust and handled as such;
/// private-use and noncharacter code points have no agreed appearance.
fn is_unassigned_or_private(c: char) -> bool {
    matches!(
        c,
        '\u{E000}'..='\u{F8FF}'
            | '\u{FDD0}'..='\u{FDEF}'
            | '\u{FFFE}'..='\u{FFFF}'
            | '\u{F0000}'..='\u{10FFFF}'
    )
}

/// Assigned code points that draw as empty space: the combining grapheme
/// joiner, the Hangul fillers and the blank braille cell.
fn is_blank_graphic(c: char) -> bool {
    matches!(
        c,
        '\u{034F}' | '\u{115F}' | '\u{1160}' | '\u{3164}' | '\u{FFA0}' | '\u{2800}'
    )
}

/// `@`, `<`, `>` and the compatibility forms that look the same.
fn is_address_punctuation(c: char) -> bool {
    matches!(
        c,
        '@' | '<'
            | '>'
            | '\u{FF20}'
            | '\u{FF1C}'
            | '\u{FF1E}'
            | '\u{FE6B}'
            | '\u{FE64}'
            | '\u{FE65}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_and_collapses_whitespace() {
        assert_eq!(normalize("  Ama   Adeyemi \n").unwrap(), "Ama Adeyemi");
        assert_eq!(normalize("Ama\tAdeyemi").unwrap(), "Ama Adeyemi");
    }

    #[test]
    fn blank_means_no_name() {
        assert_eq!(normalize("").unwrap(), "");
        assert_eq!(normalize("  \t ").unwrap(), "");
        assert_eq!(shown(Some("")), None);
        assert_eq!(shown(Some("   ")), None);
        assert_eq!(shown(None), None);
    }

    #[test]
    fn sixty_characters_is_the_limit_counted_after_collapsing() {
        let sixty = "a".repeat(60);
        assert_eq!(normalize(&sixty).unwrap(), sixty);
        assert_eq!(normalize(&"a".repeat(61)), Err(Refusal::TooLong));
        // Counted in characters, not bytes.
        let accented = "é".repeat(60);
        assert_eq!(normalize(&accented).unwrap(), accented);
        // Padding that collapses away doesn't count.
        assert_eq!(normalize(&format!("  {sixty}  ")).unwrap(), sixty);
    }

    #[test]
    fn refuses_what_could_imitate_an_address_or_reorder_the_line() {
        for name in [
            "Ama <ama@example.com>",
            "ama@example.com",
            "Ama\u{FF20}example",
            "Ama\u{202E}imedA",
            "Ama\u{2066}x\u{2069}",
            "Ama\u{200B}Adeyemi",
            "Ama\u{0007}",
            "\u{FEFF}Ama",
            "Ama\u{3164}",
        ] {
            assert_eq!(normalize(name), Err(Refusal::NotAllowed), "{name:?}");
            assert_eq!(shown(Some(name)), None, "{name:?}");
        }
    }

    #[test]
    fn a_name_needs_a_letter_or_number() {
        assert_eq!(normalize("---"), Err(Refusal::NotAllowed));
        assert_eq!(normalize("\u{200D}"), Err(Refusal::NotAllowed));
        assert_eq!(normalize("🙂"), Err(Refusal::NotAllowed));
        assert_eq!(normalize("Ama 🙂").unwrap(), "Ama 🙂");
        assert_eq!(normalize("7").unwrap(), "7");
    }

    #[test]
    fn other_scripts_marks_and_joiners_pass() {
        for name in [
            "Ọláyínká Adébáyọ̀",
            "李小龍",
            "محمد علي",
            "क्‍ष",
            "O'Neil-Smith Jr.",
        ] {
            assert_eq!(normalize(name).unwrap(), name, "{name:?}");
        }
    }

    #[test]
    fn one_line_keeps_the_whole_email() {
        assert_eq!(one_line(None, "ama@example.com"), "ama@example.com");
        assert_eq!(one_line(Some(""), "ama@example.com"), "ama@example.com");
        assert_eq!(
            one_line(Some("Ama Adeyemi"), "ama@example.com"),
            "Ama Adeyemi · ama@example.com"
        );
        // A refused name is not shown.
        assert_eq!(
            one_line(Some("boss@bank.example"), "ama@example.com"),
            "ama@example.com"
        );
        let long = "N".repeat(60);
        let line = one_line(Some(&long), "ama@example.com");
        assert_eq!(line, format!("{}… · ama@example.com", "N".repeat(32)));
    }
}
