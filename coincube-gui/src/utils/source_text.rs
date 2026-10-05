//! Test-only helpers for tests that read this crate's own source text
//! (`include_str!` or `read_to_string` of a `.rs` file).
//!
//! Git for Windows checks sources out with CRLF line endings by default
//! (`core.autocrlf`), so a guard that searches the raw text for a marker
//! spanning a line break, such as `"\n#[cfg(test)]\n"`, never finds it there.
//! Such guards search [`lf_only`] text instead, and pin that with a
//! [`as_crlf`] copy of the same source.

/// `text` with every CRLF line ending turned into LF. A lone `\r` is kept.
pub fn lf_only(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// `text` as a CRLF checkout reads it: every line ending is CRLF.
pub fn as_crlf(text: &str) -> String {
    lf_only(text).replace('\n', "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lf_only_turns_crlf_into_lf_and_keeps_everything_else() {
        assert_eq!(lf_only("a\r\nb\nc\r\n"), "a\nb\nc\n");
        assert_eq!(lf_only("a\rb\r\r\n"), "a\rb\r\n");
        assert_eq!(lf_only("no line break"), "no line break");
        assert_eq!(lf_only(""), "");
    }

    #[test]
    fn as_crlf_is_a_crlf_checkout_of_either_form() {
        let lf = "fn a() {}\n#[cfg(test)]\nmod tests {}\n";
        let crlf = "fn a() {}\r\n#[cfg(test)]\r\nmod tests {}\r\n";
        assert_eq!(as_crlf(lf), crlf);
        assert_eq!(as_crlf(crlf), crlf);
        assert_eq!(lf_only(&as_crlf(lf)), lf);
        assert!(!as_crlf(lf).contains("\n#[cfg(test)]\n"));
    }
}
