//! OSC sequence scanner. Frames `ESC ] ... (BEL | ST)` across chunk boundaries (mirrors
//! Tabby's middleware/oscProcessing.ts) and emits the events we care about:
//!
//!   - OSC 0 / OSC 2                  -> window title (dynamic tab titles)
//!   - OSC 7  / OSC 1337 CurrentDir=  -> cwd
//!   - OSC 52 c;<base64>              -> clipboard (raw payload; decoded at use site, M6)
//!   - OSC 9;4;state;pct              -> precise progress (ConEmu protocol)
//!
//! All input bytes still flow to the terminal engine untouched; this only observes them.
use crate::progress::Progress;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OscEvent {
    Title(String), // OSC 0/2 window title; empty = reset
    Cwd(String),
    Clipboard(String),
    Progress(Progress),
    Shell(ShellEvent),
}

/// OSC 133 shell-integration marks (FinalTerm / iTerm2 protocol). Feeds the tab exit-state dot.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ShellEvent {
    PromptStart,             // 133;A
    CommandStart,            // 133;C (command begins executing)
    CommandEnd(Option<i32>), // 133;D[;exit_code]
}

pub(crate) struct OscScanner {
    buf: Vec<u8>, // partial OSC carried across reads
}

impl OscScanner {
    pub(crate) fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub(crate) fn feed(&mut self, data: &[u8]) -> Vec<OscEvent> {
        let mut bytes = std::mem::take(&mut self.buf);
        bytes.extend_from_slice(data);

        let mut events = Vec::new();
        let mut i = 0;
        while let Some(p) = find(&bytes, b"\x1b]", i) {
            let payload_start = p + 2;
            let Some((s, end)) = find_suffix(&bytes, payload_start) else {
                // Incomplete OSC - keep from the prefix for the next chunk.
                self.buf = bytes[p..].to_vec();
                return events;
            };
            if let Some(ev) = parse_osc(&bytes[payload_start..s]) {
                events.push(ev);
            }
            i = end;
        }
        // Carry a lone trailing ESC so a prefix split exactly on the boundary survives.
        if bytes.last() == Some(&0x1b) {
            self.buf = vec![0x1b];
        }
        events
    }
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|k| from + k)
}

/// Nearest OSC terminator at/after `from`: BEL (0x07) or ST (ESC \). Returns (payload_end, seq_end).
fn find_suffix(hay: &[u8], from: usize) -> Option<(usize, usize)> {
    let bel = find(hay, b"\x07", from).map(|k| (k, k + 1));
    let st = find(hay, b"\x1b\\", from).map(|k| (k, k + 2));
    match (bel, st) {
        (Some(b), Some(s)) => Some(if b.0 <= s.0 { b } else { s }),
        (Some(b), None) => Some(b),
        (None, Some(s)) => Some(s),
        (None, None) => None,
    }
}

fn parse_osc(payload: &[u8]) -> Option<OscEvent> {
    // OSC 7 is read from the raw bytes, before the `;` field split: a path may hold a `;`, and
    // percent-escaped UTF-8 only decodes bytewise.
    if let Some(url) = payload.strip_prefix(b"7;") {
        return osc7_path(url, local_host()).map(OscEvent::Cwd);
    }
    let text = String::from_utf8_lossy(payload);
    let fields: Vec<&str> = text.split(';').collect();
    match *fields.first()? {
        // 0 (icon + title) / 2 (title): the window title. OSC 1 (icon only) is ignored, and a
        // bare "0" with no `;` sets nothing; an empty title resets it.
        "0" | "2" => {
            fields.get(1)?;
            Some(OscEvent::Title(fields[1..].join(";")))
        }
        "1337" => {
            let rest = fields[1..].join(";");
            let dir = rest.strip_prefix("CurrentDir=")?;
            Some(OscEvent::Cwd(expand_home(dir)))
        }
        "52" => {
            // 52 ; (c|p|"") ; base64
            let b64 = fields.get(2)?;
            Some(OscEvent::Clipboard(b64.to_string()))
        }
        "9" => {
            if fields.get(1) != Some(&"4") {
                return None;
            }
            let pct = fields.get(3).and_then(|p| p.parse::<u8>().ok()).unwrap_or(0);
            let progress = match *fields.get(2)? {
                "0" => Progress::None,
                "1" => Progress::Normal(pct.min(100)),
                "2" => Progress::Error(pct.min(100)),
                "3" => Progress::Indeterminate,
                "4" => Progress::Paused(pct.min(100)),
                _ => return None,
            };
            Some(OscEvent::Progress(progress))
        }
        "133" => {
            // 133 ; A|B|C|D [; exit_code] - shell-integration marks.
            let ev = match *fields.get(1)? {
                "A" => ShellEvent::PromptStart,
                "C" => ShellEvent::CommandStart,
                "D" => ShellEvent::CommandEnd(fields.get(2).and_then(|s| s.parse::<i32>().ok())),
                _ => return None, // B (prompt end) and others: ignored
            };
            Some(OscEvent::Shell(ev))
        }
        _ => None,
    }
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix('~')
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}{rest}");
    }
    path.to_string()
}

/// A cwd longer than this is refused. Real paths stay far below it (PATH_MAX is 1024 on macOS).
const MAX_CWD_BYTES: usize = 4096;

/// This machine's hostname, read once: the OSC 7 host check runs for every event.
fn local_host() -> &'static str {
    static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HOST.get_or_init(|| sysinfo::System::host_name().unwrap_or_default())
}

/// `box` and `box.local` name the same machine, so only the label before the first dot counts.
fn short_name(host: &str) -> &str {
    host.split('.').next().unwrap_or(host)
}

/// Whether an OSC 7 host is this machine: empty, exactly `localhost`, or our own hostname.
fn is_local_host(host: &str, local: &str) -> bool {
    host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || (!local.is_empty() && short_name(host).eq_ignore_ascii_case(short_name(local)))
}

fn hex_pair(hi: u8, lo: u8) -> Option<u8> {
    let (hi, lo) = (char::from(hi).to_digit(16)?, char::from(lo).to_digit(16)?);
    u8::try_from(hi * 16 + lo).ok()
}

/// Decode `%XX` escapes bytewise. A `%` not followed by two hex digits stays as written.
fn percent_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while let Some(&b) = s.get(i) {
        let escaped = (b == b'%').then(|| hex_pair(*s.get(i + 1)?, *s.get(i + 2)?)).flatten();
        if let Some(byte) = escaped {
            out.push(byte);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    out
}

/// The local path an OSC 7 `file://host/path` URL names. `None` for another machine's host, a
/// malformed URL, or a path that is not plain text (control characters, invalid UTF-8, too long):
/// the path becomes a filesystem lookup, and a control character in it would be a lie about where
/// the shell is. Shells percent-encode the path (Apple's `update_terminal_cwd`, fish, our own
/// hooks), so decode it. A bare path with no `file://` is taken as it is.
fn osc7_path(url: &[u8], local_host: &str) -> Option<String> {
    let path = match url.strip_prefix(b"file://") {
        Some(rest) => {
            let slash = rest.iter().position(|&b| b == b'/')?;
            if !is_local_host(std::str::from_utf8(&rest[..slash]).ok()?, local_host) {
                return None;
            }
            String::from_utf8(percent_decode(&rest[slash..])).ok()?
        }
        None => expand_home(std::str::from_utf8(url).ok()?),
    };
    let plain =
        !path.is_empty() && path.len() <= MAX_CWD_BYTES && !path.chars().any(char::is_control);
    plain.then_some(path)
}

/// The encoding the shell hooks apply (see `shell.rs`): keep `[/._~A-Za-z0-9-]`, `%XX` every
/// other byte. The tests here and in `shell.rs` share it as the oracle for the hook scripts.
#[cfg(test)]
pub(crate) fn hook_encode(path: &str) -> String {
    path.bytes()
        .map(|b| match b {
            b'/' | b'.' | b'_' | b'~' | b'-' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        // Splitting a byte stream at ANY boundary must yield the same OSC events as feeding it
        // whole - the core guarantee of the cross-chunk framing/carry logic.
        #[test]
        fn split_invariant(data in prop::collection::vec(any::<u8>(), 0..512), cut in 0usize..512) {
            let cut = cut.min(data.len());
            let whole = OscScanner::new().feed(&data);
            let mut sc = OscScanner::new();
            let mut split = sc.feed(&data[..cut]);
            split.extend(sc.feed(&data[cut..]));
            prop_assert_eq!(whole, split);
        }
    }

    #[test]
    fn cwd_via_osc_1337() {
        let mut sc = OscScanner::new();
        assert_eq!(
            sc.feed(b"\x1b]1337;CurrentDir=/tmp/foo\x07"),
            vec![OscEvent::Cwd("/tmp/foo".into())]
        );
    }

    #[test]
    fn cwd_via_osc_7_file_url() {
        let mut sc = OscScanner::new();
        assert_eq!(
            sc.feed(b"\x1b]7;file://localhost/Users/x\x1b\\"),
            vec![OscEvent::Cwd("/Users/x".into())]
        );
        // No host at all is this machine too.
        assert_eq!(
            OscScanner::new().feed(b"\x1b]7;file:///Users/x\x07"),
            vec![OscEvent::Cwd("/Users/x".into())]
        );
    }

    #[test]
    fn a_semicolon_in_an_osc_7_path_survives_the_field_split() {
        assert_eq!(
            OscScanner::new().feed(b"\x1b]7;file:///a;b\x07"),
            vec![OscEvent::Cwd("/a;b".into())]
        );
    }

    #[test]
    fn osc_7_from_another_machine_is_dropped() {
        // An ssh session's remote path would group the tab under a repo that is not on this Mac.
        assert_eq!(
            OscScanner::new().feed(b"\x1b]7;file://some-remote-box.invalid/srv/app\x07"),
            vec![]
        );
    }

    #[test]
    fn osc_7_path_table() {
        let local = "Alexs-Mac.local";
        let cases: [(&[u8], Option<&str>); 20] = [
            (b"file:///a", Some("/a")),
            (b"file://localhost/a", Some("/a")),
            (b"file://LocalHost/a", Some("/a")),
            (b"file://Alexs-Mac.local/a", Some("/a")), // the hostname zsh's $HOST reports
            (b"file://alexs-mac/a", Some("/a")),       // short name, any case
            (b"file://other/a", None),
            (b"file://localhost.example.com/a", None), // only the exact name `localhost`
            (b"file://localhost/a%20b", Some("/a b")),
            (b"file://localhost/%E2%82%AC", Some("/\u{20ac}")), // percent-encoded UTF-8 rebuilt
            (b"file://localhost/a%3Bb", Some("/a;b")),
            (b"file://localhost/a%25b", Some("/a%b")),
            (b"file://localhost/a%zzb", Some("/a%zzb")), // a bad escape stays literal
            (b"file://localhost/100%", Some("/100%")),
            (b"file://localhost/a%4", Some("/a%4")),
            (b"file://localhost/a%00b", None), // NUL, BEL, ESC would make the path a lie
            (b"file://localhost/a%07b", None),
            (b"file://localhost/a%1Bb", None),
            (b"file://localhost/a%FF", None),    // not UTF-8
            (b"/tmp/plain", Some("/tmp/plain")), // a bare path, as some emitters send
            (b"file://localhost", None),         // a host and no path
        ];
        for (url, want) in cases {
            assert_eq!(osc7_path(url, local).as_deref(), want, "{}", String::from_utf8_lossy(url));
        }
        let long = format!("file://localhost/{}", "a".repeat(MAX_CWD_BYTES));
        assert_eq!(osc7_path(long.as_bytes(), local), None, "an absurdly long path is refused");
    }

    proptest! {
        // Whatever a directory is called, what the hooks encode is what the decoder returns.
        #[test]
        fn hook_encoding_round_trips(name in "[^\\x00-\\x1f\\x7f-\\x9f/]{1,64}") {
            let path = format!("/{name}");
            let url = format!("file://localhost{}", hook_encode(&path));
            prop_assert_eq!(osc7_path(url.as_bytes(), ""), Some(path));
        }

        // Shell output is adversarial: no byte string may panic the decoder.
        #[test]
        fn osc_7_decoding_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
            let _ = osc7_path(&bytes, "box.local");
        }
    }

    #[test]
    fn progress_osc_9_4_states() {
        assert_eq!(
            OscScanner::new().feed(b"\x1b]9;4;1;42\x07"),
            vec![OscEvent::Progress(Progress::Normal(42))]
        );
        assert_eq!(
            OscScanner::new().feed(b"\x1b]9;4;2;\x1b\\"),
            vec![OscEvent::Progress(Progress::Error(0))]
        );
        assert_eq!(
            OscScanner::new().feed(b"\x1b]9;4;3\x07"),
            vec![OscEvent::Progress(Progress::Indeterminate)]
        );
    }

    #[test]
    fn shell_integration_osc_133() {
        use ShellEvent::{CommandEnd, CommandStart, PromptStart};
        let cases: [(&[u8], ShellEvent); 5] = [
            (b"\x1b]133;A\x07", PromptStart),
            (b"\x1b]133;C\x07", CommandStart),
            (b"\x1b]133;D;0\x07", CommandEnd(Some(0))),
            (b"\x1b]133;D;127\x07", CommandEnd(Some(127))),
            (b"\x1b]133;D\x07", CommandEnd(None)),
        ];
        for (input, want) in cases {
            assert_eq!(OscScanner::new().feed(input), vec![OscEvent::Shell(want)], "{input:?}");
        }
        // 133;B (prompt end) and unknown kinds are ignored.
        assert_eq!(OscScanner::new().feed(b"\x1b]133;B\x07"), vec![]);
    }

    #[test]
    fn buffers_partial_osc_across_reads() {
        let mut sc = OscScanner::new();
        assert_eq!(sc.feed(b"\x1b]1337;CurrentDir=/tmp"), vec![]);
        assert_eq!(sc.feed(b"/foo\x07"), vec![OscEvent::Cwd("/tmp/foo".into())]);
    }

    #[test]
    fn ignores_plain_text_and_unknown_osc() {
        let mut sc = OscScanner::new();
        assert_eq!(sc.feed(b"hello world"), vec![]);
        assert_eq!(sc.feed(b"\x1b]777;whatever\x07"), vec![]);
    }

    #[test]
    fn title_osc_0_and_2() {
        let cases: [(&[u8], &str); 4] = [
            (b"\x1b]0;hello\x07", "hello"),
            (b"\x1b]2;a;b\x1b\\", "a;b"), // semicolons in the title survive
            (b"\x1b]0;\x07", ""),         // empty title = reset
            (b"\x1b]2;~/Git\x07", "~/Git"), // titles are opaque text (no ~ expansion)
        ];
        for (input, want) in cases {
            assert_eq!(
                OscScanner::new().feed(input),
                vec![OscEvent::Title(want.into())],
                "{input:?}"
            );
        }
        // OSC 1 (icon-only) and a bare "0" without a `;` are ignored.
        assert_eq!(OscScanner::new().feed(b"\x1b]1;icon\x07"), vec![]);
        assert_eq!(OscScanner::new().feed(b"\x1b]0\x07"), vec![]);
    }

    #[test]
    fn osc_between_text_is_extracted() {
        let mut sc = OscScanner::new();
        assert_eq!(
            sc.feed(b"before\x1b]9;4;1;5\x07after"),
            vec![OscEvent::Progress(Progress::Normal(5))]
        );
    }
}
