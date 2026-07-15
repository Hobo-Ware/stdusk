//! Scrollback search: the pure match-finding logic. Given the terminal's buffer lines
//! (each tagged with its alacritty `Line` coordinate) and a query, return every occurrence.
//! ASCII-case-insensitive substring match; column index == char index (one char per cell).
//! The UI glue (overlay, cycling, highlight-via-selection) lives in `main.rs`/`terminal.rs`.

/// A single match: which buffer line, starting column, and length in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Match {
    pub(crate) line: i32,
    pub(crate) col: usize,
    pub(crate) len: usize,
}

/// Every non-overlapping occurrence of `query` across `lines`, top-to-bottom, left-to-right.
/// Empty query (or query longer than a line) yields nothing. Case-insensitive over ASCII.
pub(crate) fn find_matches(lines: &[(i32, String)], query: &str) -> Vec<Match> {
    let needle: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
    let n = needle.len();
    if n == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (line, text) in lines {
        let hay: Vec<char> = text.chars().map(|c| c.to_ascii_lowercase()).collect();
        if hay.len() < n {
            continue;
        }
        let mut i = 0;
        while i + n <= hay.len() {
            if hay[i..i + n] == needle[..] {
                out.push(Match { line: *line, col: i, len: n });
                i += n; // non-overlapping
            } else {
                i += 1;
            }
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

    #[test]
    fn finds_substring_with_column() {
        let lines = [(0, "hello world".to_string())];
        assert_eq!(find_matches(&lines, "world"), vec![m(0, 6, 5)]);
    }

    #[test]
    fn is_ascii_case_insensitive() {
        let lines = [(0, "Hello World".to_string())];
        assert_eq!(find_matches(&lines, "WORLD"), vec![m(0, 6, 5)]);
    }

    #[test]
    fn multiple_non_overlapping_in_one_line() {
        let lines = [(0, "aXaXa".to_string())];
        assert_eq!(find_matches(&lines, "a"), vec![m(0, 0, 1), m(0, 2, 1), m(0, 4, 1)]);
        // "aa" in "aaaa" -> non-overlapping at 0 and 2.
        assert_eq!(find_matches(&[(0, "aaaa".to_string())], "aa"), vec![m(0, 0, 2), m(0, 2, 2)]);
    }

    #[test]
    fn spans_lines_with_their_coordinates() {
        let lines = [(-1, "foo".to_string()), (0, "foofoo".to_string())];
        assert_eq!(find_matches(&lines, "foo"), vec![m(-1, 0, 3), m(0, 0, 3), m(0, 3, 3)]);
    }

    #[test]
    fn empty_query_and_no_match_and_too_long() {
        let lines = [(0, "abc".to_string())];
        assert_eq!(find_matches(&lines, ""), vec![]);
        assert_eq!(find_matches(&lines, "zzz"), vec![]);
        assert_eq!(find_matches(&lines, "abcd"), vec![]); // longer than the line
    }
}
