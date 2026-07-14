//! OSC sequence scanner. Frames `ESC ] ... (BEL | ST)` across chunk boundaries (mirrors
//! Tabby's middleware/oscProcessing.ts) and emits the events we care about:
//!   - OSC 7  / OSC 1337 CurrentDir=  -> cwd
//!   - OSC 52 c;<base64>              -> clipboard (raw payload; decoded at use site, M6)
//!   - OSC 9;4;state;pct              -> precise progress (ConEmu protocol)
//! All input bytes still flow to the terminal engine untouched; this only observes them.
use crate::progress::Progress;

#[derive(Debug, PartialEq, Eq)]
pub enum OscEvent {
    Cwd(String),
    Clipboard(String),
    Progress(Progress),
}

pub struct OscScanner {
    buf: Vec<u8>, // partial OSC carried across reads
}

impl OscScanner {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<OscEvent> {
        let mut bytes = std::mem::take(&mut self.buf);
        bytes.extend_from_slice(data);

        let mut events = Vec::new();
        let mut i = 0;
        while let Some(p) = find(&bytes, b"\x1b]", i) {
            let payload_start = p + 2;
            match find_suffix(&bytes, payload_start) {
                Some((s, end)) => {
                    if let Some(ev) = parse_osc(&bytes[payload_start..s]) {
                        events.push(ev);
                    }
                    i = end;
                }
                None => {
                    // Incomplete OSC - keep from the prefix for the next chunk.
                    self.buf = bytes[p..].to_vec();
                    return events;
                }
            }
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
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|k| from + k)
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
    let text = String::from_utf8_lossy(payload);
    let fields: Vec<&str> = text.split(';').collect();
    match *fields.first()? {
        "1337" => {
            let rest = fields[1..].join(";");
            let dir = rest.strip_prefix("CurrentDir=")?;
            Some(OscEvent::Cwd(expand_home(dir)))
        }
        "7" => {
            // file://host/path
            let url = fields.get(1)?;
            let path = url.strip_prefix("file://").and_then(|r| r.find('/').map(|k| &r[k..]));
            Some(OscEvent::Cwd(expand_home(path.unwrap_or(url))))
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
        _ => None,
    }
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix('~') {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}{rest}");
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            sc.feed(b"\x1b]7;file://host/Users/x\x1b\\"),
            vec![OscEvent::Cwd("/Users/x".into())]
        );
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
    fn buffers_partial_osc_across_reads() {
        let mut sc = OscScanner::new();
        assert_eq!(sc.feed(b"\x1b]1337;CurrentDir=/tmp"), vec![]);
        assert_eq!(sc.feed(b"/foo\x07"), vec![OscEvent::Cwd("/tmp/foo".into())]);
    }

    #[test]
    fn ignores_plain_text_and_unknown_osc() {
        let mut sc = OscScanner::new();
        assert_eq!(sc.feed(b"hello world"), vec![]);
        assert_eq!(sc.feed(b"\x1b]0;window title\x07"), vec![]);
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
