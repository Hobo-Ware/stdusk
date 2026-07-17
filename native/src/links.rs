//! Clickable links (M11): find URLs and file paths in a rendered row so the grid can underline
//! the one under the pointer (with the command modifier held) and open it on click. Detection +
//! path resolution are pure and unit-tested; `open` is the only side effect.
use std::sync::OnceLock;

use regex::Regex;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LinkKind {
    Url,
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
    for m in path_re().find_iter(text) {
        let trimmed = m.as_str().trim_end_matches(TRAILING);
        push(col(m.start()), trimmed.chars().count(), LinkKind::Path, &mut taken);
    }
    out.sort_by_key(|l| l.start);
    out
}

/// Resolve a clicked link to what should be handed to `open`: URLs pass through; paths are
/// `~`-expanded and made absolute against `cwd`.
pub(crate) fn resolve_target(text: &str, kind: LinkKind, cwd: Option<&str>, home: &str) -> String {
    match kind {
        LinkKind::Url => text.to_owned(),
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
