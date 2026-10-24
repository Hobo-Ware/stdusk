//! What a paste puts into a pty: line breaks as CR, no control bytes, Tabby's trim rules.

/// The two settings that shape a paste.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PasteRules {
    pub newlines_to_spaces: bool,
    pub trim: bool,
}

/// CRLF and LF become CR, control bytes other than CR and TAB are dropped (so a paste cannot
/// carry `ESC [201~` out of bracketed-paste mode), and `newlines_to_spaces` collapses each run
/// of line breaks into one space. Multiline-ness afterwards is `contains('\r')`.
pub fn normalize(input: &str, newlines_to_spaces: bool) -> String {
    let flat = input.replace("\r\n", "\r").replace('\n', "\r");
    let mut out = String::with_capacity(flat.len());
    let mut in_run = false;
    for ch in flat.chars() {
        if ch == '\r' {
            if newlines_to_spaces {
                if !in_run {
                    out.push(' ');
                    in_run = true;
                }
            } else {
                out.push(ch);
            }
        } else if ch == '\t' || !ch.is_control() {
            in_run = false;
            out.push(ch);
        }
    }
    out
}

/// Tabby's trim rule: the end is always trimmed, the start only when what is left is a single
/// line.
pub fn trim(s: &str, enabled: bool) -> String {
    if !enabled {
        return s.to_owned();
    }
    let end = s.trim_end();
    if end.contains('\r') { end.to_owned() } else { end.trim_start().to_owned() }
}

/// `normalize` then `trim`; `None` when nothing is left to send.
pub fn prepare(input: &str, rules: PasteRules) -> Option<String> {
    let text = trim(&normalize(input, rules.newlines_to_spaces), rules.trim);
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_breaks_become_cr_and_runs_collapse_to_one_space_on_request() {
        assert_eq!(normalize("a\r\nb\nc", false), "a\rb\rc");
        assert_eq!(normalize("a\n\n\nb", true), "a b");
        assert_eq!(normalize("ls -la\n", true), "ls -la ");
    }

    #[test]
    fn control_bytes_are_dropped_but_tab_and_text_stay() {
        let cases = [
            ("a\x1b[201~b\x03c", "a[201~bc"),
            ("tab\tstays", "tab\tstays"),
            ("naive \u{e9} text \u{1f600}", "naive \u{e9} text \u{1f600}"),
            ("nul\0here\x7f", "nulhere"),
        ];
        for (text, want) in cases {
            assert_eq!(normalize(text, false), want, "{text:?}");
        }
    }

    #[test]
    fn trim_rules() {
        assert_eq!(trim("  ls -la \r", true), "ls -la");
        assert_eq!(trim("  a\rb  \r", true), "  a\rb");
        assert_eq!(trim("  x  ", false), "  x  ");
    }

    #[test]
    fn prepare_is_the_whole_pipeline_and_none_when_empty() {
        let rules = PasteRules { newlines_to_spaces: false, trim: true };
        let cases = [
            ("one\r\ntwo\nthree", Some("one\rtwo\rthree")),
            ("a\x1b[201~b\x03c", Some("a[201~bc")),
            ("  padded  \n", Some("padded")),
            ("  a\n  b\n\n", Some("  a\r  b")),
            ("tab\tstays", Some("tab\tstays")),
            ("", None),
            (" \r\n\t ", None),
        ];
        for (text, want) in cases {
            assert_eq!(prepare(text, rules).as_deref(), want, "{text:?}");
        }
    }
}
