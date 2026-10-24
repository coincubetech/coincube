//! Display-only Cube names. Persisted names and identity bytes are untouched.

pub(crate) fn name(raw: &str) -> String {
    const LIMIT: usize = 80;
    let mut rendered = String::new();
    let mut last_was_space = false;
    let mut truncated = false;
    for character in raw.chars() {
        if character.is_whitespace() {
            if !last_was_space && !rendered.is_empty() {
                rendered.push(' ');
                last_was_space = true;
            }
            continue;
        }
        if character.is_control()
            || matches!(
                character,
                '\u{061c}'
                    | '\u{180e}'
                    | '\u{200b}'..='\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2060}'..='\u{206f}'
                    | '\u{feff}'
            )
        {
            continue;
        }
        if rendered.chars().count() == LIMIT {
            truncated = true;
            break;
        }
        rendered.push(character);
        last_was_space = false;
    }
    let mut rendered = rendered.trim().to_string();
    if truncated {
        rendered.pop();
        rendered.push('…');
    }
    if rendered.is_empty() {
        "Unnamed Cube".to_string()
    } else {
        rendered
    }
}

#[cfg(test)]
mod tests {
    use super::name;

    #[test]
    fn removes_control_and_format_markers_without_changing_storage_identity() {
        let stored = "  Family\n\u{202e}\u{200b}\u{061c} Cube\u{2066}\u{feff}  ";
        assert_eq!(name(stored), "Family Cube");
        assert!(stored.contains('\u{202e}'));
        assert_eq!(name("\u{0000}\u{200d}\n"), "Unnamed Cube");
        assert_eq!(name(" A\t \n B "), "A B");
    }

    #[test]
    fn wide_names_are_bounded_to_eighty_visible_scalars() {
        let stored = "界".repeat(300);
        let shown = name(&stored);
        assert_eq!(shown.chars().count(), 80);
        assert!(shown.ends_with('…'));
        assert_eq!(stored.chars().count(), 300);
        assert_eq!(name(&"界".repeat(80)), "界".repeat(80));
    }
}
