//! Clickable links (M11): find URLs and file paths in a rendered row so the grid can underline
//! the one under the pointer (with the command modifier held) and open it on click. Detection +
//! path resolution are pure and unit-tested; `open` is the only side effect.
use std::sync::OnceLock;

use regex::Regex;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LinkKind {
    Url,
    Ip, // bare IPv4 literal (no scheme) - opened as http://<ip>
    Path,
}

/// A link span within one row, in character columns (one grid cell = one char).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Link {
    pub(crate) start: usize,
    pub(crate) len: usize,
    pub(crate) kind: LinkKind,
}

fn url_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?i)\b(?:https?|ftp|file)://[^\s<>"'`|{}\^\[\]()]+"#).unwrap())
}

fn ip_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // Bare IPv4 with optional port (Tabby's IPHandler). Octet ranges are not validated - a
    // false-positive like 999.1.1.1 just opens a dead page.
    R.get_or_init(|| Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}(?::\d{1,5})?\b").unwrap())
}

fn path_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // Absolute / home / relative paths: an optional ~ or .[.] prefix, then one or more /segments.
    R.get_or_init(|| Regex::new(r"(?:~|\.{1,2})?(?:/[A-Za-z0-9._@%+\-]+)+/?").unwrap())
}

const TRAILING: [char; 10] = [')', '.', ',', ';', ':', '!', '?', ']', '}', '\''];

/// Find non-overlapping links in a single row of text, left to right. URLs win over paths where
/// they overlap (a `file://` URL also matches the path regex).
pub(crate) fn find_in_row(text: &str) -> Vec<Link> {
    let n = text.chars().count();
    let mut taken = vec![false; n];
    let mut out: Vec<Link> = Vec::new();
    let col = |byte: usize| text[..byte].chars().count();

    let mut push = |start: usize, len: usize, kind: LinkKind, taken: &mut [bool]| {
        if len == 0 || start + len > taken.len() {
            return;
        }
        if taken[start..start + len].iter().any(|&t| t) {
            return; // overlaps a higher-priority match
        }
        for t in &mut taken[start..start + len] {
            *t = true;
        }
        out.push(Link { start, len, kind });
    };

    for m in url_re().find_iter(text) {
        let trimmed = m.as_str().trim_end_matches(TRAILING);
        push(col(m.start()), trimmed.chars().count(), LinkKind::Url, &mut taken);
    }
    for m in ip_re().find_iter(text) {
        let trimmed = m.as_str().trim_end_matches(TRAILING);
        push(col(m.start()), trimmed.chars().count(), LinkKind::Ip, &mut taken);
    }
    for m in path_re().find_iter(text) {
        let trimmed = m.as_str().trim_end_matches(TRAILING);
        push(col(m.start()), trimmed.chars().count(), LinkKind::Path, &mut taken);
    }
    out.sort_by_key(|l| l.start);
    out
}

/// One row's slice of a link, in character columns.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LinkSpan {
    pub(crate) row: usize,
    pub(crate) col: usize,
    pub(crate) len: usize,
}

/// The link under the pointer, joined back together across the rows it wraps over.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HitLink {
    pub(crate) kind: LinkKind,
    pub(crate) text: String,
    pub(crate) spans: Vec<LinkSpan>,
}

/// A row with a glyph in its last column runs on into the next row - either the terminal
/// hard-wrapped it (the tail starts at column 0) or a TUI wrapped it itself (the tail starts
/// after its own indent). Both leave the last column filled.
fn overflows(row: &str) -> bool {
    row.chars().next_back().is_some_and(|c| !c.is_whitespace() && c != '\0')
}

fn blank(row: &str) -> bool {
    row.chars().all(|c| c.is_whitespace() || c == '\0')
}

/// One row's contribution to the joined line: where it sits on the grid, and where its text
/// starts within that line.
struct Piece {
    span: LinkSpan,
    joined_start: usize,
}

/// Find the link at `(row, col)` of a visible grid, following the wrap in both directions so a
/// path or URL broken over two rows is one link. `rows` holds every visible row, each padded to
/// the full column count. Continuation rows contribute their text minus the leading indent.
pub(crate) fn link_at(rows: &[String], row: usize, col: usize) -> Option<HitLink> {
    if row >= rows.len() || blank(&rows[row]) {
        return None;
    }
    let mut first = row;
    while first > 0 && overflows(&rows[first - 1]) {
        first -= 1;
    }
    let mut last = row;
    while overflows(&rows[last]) && last + 1 < rows.len() && !blank(&rows[last + 1]) {
        last += 1;
    }

    let mut joined = String::new();
    let mut pieces: Vec<Piece> = Vec::new();
    for (i, text) in rows[first..=last].iter().enumerate() {
        let indent =
            if i == 0 { 0 } else { text.chars().take_while(|c| c.is_whitespace()).count() };
        let span = LinkSpan {
            row: first + i,
            col: indent,
            len: text.chars().count().saturating_sub(indent),
        };
        let joined_start = joined.chars().count();
        joined.extend(text.chars().skip(indent));
        pieces.push(Piece { span, joined_start });
    }

    let hit = pieces.iter().find_map(|p| {
        (p.span.row == row && col >= p.span.col && col < p.span.col + p.span.len)
            .then(|| p.joined_start + (col - p.span.col))
    })?;
    let link =
        find_in_row(&joined).into_iter().find(|l| hit >= l.start && hit < l.start + l.len)?;
    let spans = pieces
        .iter()
        .filter_map(|p| {
            let lo = link.start.max(p.joined_start);
            let hi = (link.start + link.len).min(p.joined_start + p.span.len);
            (lo < hi).then(|| LinkSpan {
                row: p.span.row,
                col: p.span.col + (lo - p.joined_start),
                len: hi - lo,
            })
        })
        .collect();
    Some(HitLink {
        kind: link.kind,
        text: joined.chars().skip(link.start).take(link.len).collect(),
        spans,
    })
}

/// Resolve a clicked link to what should be handed to `open`: URLs pass through; paths are
/// `~`-expanded and made absolute against `cwd`.
pub(crate) fn resolve_target(text: &str, kind: LinkKind, cwd: Option<&str>, home: &str) -> String {
    match kind {
        LinkKind::Url => text.to_owned(),
        LinkKind::Ip => format!("http://{text}"),
        LinkKind::Path => {
            if text == "~" {
                home.to_owned()
            } else if let Some(rest) = text.strip_prefix("~/") {
                format!("{home}/{rest}")
            } else if text.starts_with('/') {
                text.to_owned()
            } else {
                // relative (./x, ../x, x): join to cwd if known
                match cwd {
                    Some(c) => format!("{}/{text}", c.trim_end_matches('/')),
                    None => text.to_owned(),
                }
            }
        }
    }
}

/// Open a link via the system handler (`open` on macOS). URLs go straight through; paths are
/// resolved against `cwd` + `$HOME` first.
pub(crate) fn open(text: &str, kind: LinkKind, cwd: Option<&str>) {
    let home = std::env::var("HOME").unwrap_or_default();
    let target = resolve_target(text, kind, cwd, &home);
    let _ = std::process::Command::new("open").arg(target).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(usize, usize, LinkKind)> {
        find_in_row(text).into_iter().map(|l| (l.start, l.len, l.kind)).collect()
    }

    #[test]
    fn finds_url_with_columns() {
        let links = find_in_row("see https://example.com/x now");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Url);
        assert_eq!(links[0].start, 4); // "see " = 4 chars
        assert_eq!(
            "see https://example.com/x now".chars().skip(4).take(links[0].len).collect::<String>(),
            "https://example.com/x"
        );
    }

    #[test]
    fn trims_trailing_punctuation_and_parens() {
        let links = find_in_row("(https://x.com).");
        assert_eq!(links.len(), 1);
        let got: String =
            "(https://x.com).".chars().skip(links[0].start).take(links[0].len).collect();
        assert_eq!(got, "https://x.com");
    }

    #[test]
    fn finds_paths() {
        assert_eq!(kinds("open /usr/local/bin/x"), vec![(5, 16, LinkKind::Path)]);
        assert_eq!(kinds("cd ~/proj/src"), vec![(3, 10, LinkKind::Path)]);
        assert_eq!(kinds("edit ./a/b.rs"), vec![(5, 8, LinkKind::Path)]);
    }

    #[test]
    fn no_links_in_plain_text() {
        assert!(find_in_row("just some words, no 3 links").is_empty());
    }

    #[test]
    fn url_beats_overlapping_path() {
        // file:// URL must not also yield a path for its /... tail.
        let links = find_in_row("file:///etc/hosts");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Url);
    }

    #[test]
    fn finds_bare_ip_literals() {
        assert_eq!(kinds("ping 192.168.1.10 now"), vec![(5, 12, LinkKind::Ip)]);
        assert_eq!(
            kinds("curl 10.0.0.1:8080/x"),
            vec![(5, 13, LinkKind::Ip), (18, 2, LinkKind::Path)]
        );
        // An IP inside a full URL stays one URL match.
        assert_eq!(find_in_row("http://10.0.0.1:8080/x").len(), 1);
        assert_eq!(find_in_row("http://10.0.0.1:8080/x")[0].kind, LinkKind::Url);
        // Version strings don't match (only 3 dots + digits do; 1.2.3 has 2 dots).
        assert!(kinds("v1.2.3 released").is_empty());
    }

    fn grid(rows: &[&str], cols: usize) -> Vec<String> {
        rows.iter().map(|r| format!("{r:cols$}")).collect()
    }

    #[test]
    fn joins_a_hard_wrapped_path() {
        // Terminal wrap: the tail starts at column 0 of the next row.
        let rows = grid(&["see /usr/local/share/doc/very-long-na", "me.txt here"], 37);
        let hit = link_at(&rows, 0, 10).unwrap();
        assert_eq!(hit.text, "/usr/local/share/doc/very-long-name.txt");
        assert_eq!(
            hit.spans,
            vec![LinkSpan { row: 0, col: 4, len: 33 }, LinkSpan { row: 1, col: 0, len: 6 },]
        );
    }

    #[test]
    fn joins_an_app_wrapped_path_across_its_indent() {
        // A TUI that wraps its own output indents the tail; the first row is still flush right.
        let rows = grid(&["  x /tmp/scratchpad/og/scrobble-st", "    art.png"], 34);
        let hit = link_at(&rows, 1, 6).unwrap();
        assert_eq!(hit.text, "/tmp/scratchpad/og/scrobble-start.png");
        assert_eq!(
            hit.spans,
            vec![LinkSpan { row: 0, col: 4, len: 30 }, LinkSpan { row: 1, col: 4, len: 7 },]
        );
    }

    #[test]
    fn hovering_either_half_yields_the_whole_link() {
        let rows = grid(&["https://example.com/a/very/long/pa", "  th?q=1"], 34);
        let from_head = link_at(&rows, 0, 0).unwrap();
        let from_tail = link_at(&rows, 1, 4).unwrap();
        assert_eq!(from_head, from_tail);
        assert_eq!(from_head.text, "https://example.com/a/very/long/path?q=1");
        assert_eq!(from_head.kind, LinkKind::Url);
    }

    #[test]
    fn a_row_that_ends_short_does_not_join() {
        let rows = grid(&["/etc/hosts", "/tmp/other"], 20);
        let hit = link_at(&rows, 0, 2).unwrap();
        assert_eq!(hit.text, "/etc/hosts");
        assert_eq!(hit.spans, vec![LinkSpan { row: 0, col: 0, len: 10 }]);
    }

    #[test]
    fn unwrapped_link_keeps_its_single_row_span() {
        let rows = grid(&["open /usr/local/bin/x now"], 40);
        let hit = link_at(&rows, 0, 7).unwrap();
        assert_eq!(hit.text, "/usr/local/bin/x");
        assert_eq!(hit.spans, vec![LinkSpan { row: 0, col: 5, len: 16 }]);
        assert!(link_at(&rows, 0, 0).is_none()); // "open" is not a link
        assert!(link_at(&rows, 1, 0).is_none()); // out of range
    }

    #[test]
    fn resolve_paths_and_urls() {
        let home = "/home/vlad";
        let cwd = Some("/home/vlad/proj");
        assert_eq!(resolve_target("https://x.com", LinkKind::Url, cwd, home), "https://x.com");
        assert_eq!(resolve_target("~/a", LinkKind::Path, cwd, home), "/home/vlad/a");
        assert_eq!(resolve_target("~", LinkKind::Path, cwd, home), "/home/vlad");
        assert_eq!(resolve_target("/etc/hosts", LinkKind::Path, cwd, home), "/etc/hosts");
        assert_eq!(resolve_target("./x", LinkKind::Path, cwd, home), "/home/vlad/proj/./x");
        assert_eq!(resolve_target("rel", LinkKind::Path, None, home), "rel");
    }
}
