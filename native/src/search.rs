//! Scrollback search: the pure match-finding logic. Given the terminal's buffer lines
//! (each tagged with its alacritty `Line` coordinate), a query, and options (case / regex /
//! whole-word), return every occurrence. Column index == char index (one char per cell).
//! The UI glue (overlay, cycling, highlight-via-selection) lives in `main.rs`/`terminal.rs`.
use regex::RegexBuilder;

/// A single match: which buffer line, starting column, and length in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Match {
    pub(crate) line: i32,
    pub(crate) col: usize,
    pub(crate) len: usize,
}

/// Search modifiers, mirrored by the find-bar toggles (Tabby parity).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SearchOpts {
    pub(crate) case_sensitive: bool,
    pub(crate) regex: bool,
    pub(crate) whole_word: bool,
}

/// Every non-overlapping occurrence of `query` across `lines`, top-to-bottom, left-to-right.
/// Empty query yields nothing; an invalid regex (in regex mode) also yields nothing.
pub(crate) fn find_matches(lines: &[(i32, String)], query: &str, opts: SearchOpts) -> Vec<Match> {
    if query.is_empty() {
        return Vec::new();
    }
    let base = if opts.regex { query.to_string() } else { regex::escape(query) };
    let pattern = if opts.whole_word { format!(r"\b(?:{base})\b") } else { base };
    let Ok(re) = RegexBuilder::new(&pattern).case_insensitive(!opts.case_sensitive).build() else {
        return Vec::new(); // invalid regex -> treated as no matches (find bar shows red)
    };
    let mut out = Vec::new();
    for (line, text) in lines {
        for m in re.find_iter(text) {
            if m.range().is_empty() {
                continue; // ignore zero-width matches (e.g. `a*` on empty runs)
            }
            out.push(Match {
                line: *line,
                col: text[..m.start()].chars().count(),
                len: m.as_str().chars().count(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(line: i32, col: usize, len: usize) -> Match {
        Match { line, col, len }
    }
    fn plain() -> SearchOpts {
        SearchOpts::default()
    }

    #[test]
    fn finds_substring_with_column() {
        let lines = [(0, "hello world".to_string())];
        assert_eq!(find_matches(&lines, "world", plain()), vec![m(0, 6, 5)]);
    }

    #[test]
    fn case_insensitive_by_default_sensitive_when_set() {
        let lines = [(0, "Hello World".to_string())];
        assert_eq!(find_matches(&lines, "world", plain()), vec![m(0, 6, 5)]);
        let cs = SearchOpts { case_sensitive: true, ..plain() };
        assert_eq!(find_matches(&lines, "world", cs), vec![]);
        assert_eq!(find_matches(&lines, "World", cs), vec![m(0, 6, 5)]);
    }

    #[test]
    fn multiple_non_overlapping_in_one_line() {
        let lines = [(0, "aXaXa".to_string())];
        assert_eq!(find_matches(&lines, "a", plain()), vec![m(0, 0, 1), m(0, 2, 1), m(0, 4, 1)]);
        assert_eq!(
            find_matches(&[(0, "aaaa".to_string())], "aa", plain()),
            vec![m(0, 0, 2), m(0, 2, 2)]
        );
    }

    #[test]
    fn spans_lines_with_their_coordinates() {
        let lines = [(-1, "foo".to_string()), (0, "foofoo".to_string())];
        assert_eq!(find_matches(&lines, "foo", plain()), vec![m(-1, 0, 3), m(0, 0, 3), m(0, 3, 3)]);
    }

    #[test]
    fn empty_query_and_no_match_and_too_long() {
        let lines = [(0, "abc".to_string())];
        assert_eq!(find_matches(&lines, "", plain()), vec![]);
        assert_eq!(find_matches(&lines, "zzz", plain()), vec![]);
        assert_eq!(find_matches(&lines, "abcd", plain()), vec![]);
    }

    #[test]
    fn whole_word_only_matches_bounded() {
        let lines = [(0, "foo foofoo food foo".to_string())];
        let ww = SearchOpts { whole_word: true, ..plain() };
        // bounded "foo" at col 0 and col 16 only (not inside foofoo / food).
        assert_eq!(find_matches(&lines, "foo", ww), vec![m(0, 0, 3), m(0, 16, 3)]);
    }

    #[test]
    fn regex_mode_matches_pattern() {
        let lines = [(0, "cat cot cut".to_string())];
        let rx = SearchOpts { regex: true, ..plain() };
        assert_eq!(find_matches(&lines, "c.t", rx), vec![m(0, 0, 3), m(0, 4, 3), m(0, 8, 3)]);
        // greedy, non-overlapping
        assert_eq!(find_matches(&[(0, "aaa".to_string())], "a+", rx), vec![m(0, 0, 3)]);
    }

    #[test]
    fn regex_is_literal_when_mode_off() {
        // "c.t" as a literal should not match "cat" when regex is off.
        let lines = [(0, "cat c.t".to_string())];
        assert_eq!(find_matches(&lines, "c.t", plain()), vec![m(0, 4, 3)]);
    }

    #[test]
    fn invalid_regex_yields_nothing() {
        let lines = [(0, "abc".to_string())];
        let rx = SearchOpts { regex: true, ..plain() };
        assert_eq!(find_matches(&lines, "a(", rx), vec![]); // unbalanced paren
    }
}
