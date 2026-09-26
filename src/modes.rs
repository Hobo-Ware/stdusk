//! Color-scheme change reports, DEC private mode 2031 from contour's spec (Claude Code uses it).
//! Once an app enables the mode, the terminal owes it a dark or light report on every theme change
//! so it can re-query colors and repaint. alacritty_terminal drops private modes it does not know,
//! so the pty reader watches for the set and reset itself.

const THEME_REPORTS: &[u8] = b"2031";
/// Longest `CSI ? params` prefix carried between reads; anything longer isn't a mode toggle we track.
const MAX_CARRY: usize = 64;

pub(crate) struct ThemeReportScanner {
    carry: Vec<u8>,
}

impl ThemeReportScanner {
    pub(crate) fn new() -> Self {
        Self { carry: Vec::new() }
    }

    /// The last set (`true`) or reset (`false`) of mode 2031 in this chunk, if any.
    pub(crate) fn feed(&mut self, data: &[u8]) -> Option<bool> {
        let mut bytes = std::mem::take(&mut self.carry);
        bytes.extend_from_slice(data);
        let mut last = None;
        let mut i = 0;
        while let Some(off) = bytes[i..].iter().position(|&b| b == 0x1b) {
            let start = i + off;
            match parse_private_mode(&bytes[start..]) {
                Parsed::Toggle { len, set, params } => {
                    if params.split(|&b| b == b';').any(|p| p == THEME_REPORTS) {
                        last = Some(set);
                    }
                    i = start + len;
                }
                Parsed::Partial => {
                    if bytes.len() - start <= MAX_CARRY {
                        self.carry = bytes[start..].to_vec();
                    }
                    break;
                }
                Parsed::Other => i = start + 1,
            }
        }
        last
    }
}

enum Parsed<'a> {
    Toggle { len: usize, set: bool, params: &'a [u8] },
    Partial,
    Other,
}

fn parse_private_mode(seq: &[u8]) -> Parsed<'_> {
    let prefix = b"\x1b[?";
    let head = &seq[..seq.len().min(prefix.len())];
    if head != &prefix[..head.len()] {
        return Parsed::Other;
    }
    if seq.len() < prefix.len() {
        return Parsed::Partial;
    }
    let params_end = seq[prefix.len()..]
        .iter()
        .position(|b| !(b.is_ascii_digit() || *b == b';'))
        .map(|p| prefix.len() + p);
    let Some(end) = params_end else { return Parsed::Partial };
    let params = &seq[prefix.len()..end];
    match seq[end] {
        b'h' => Parsed::Toggle { len: end + 1, set: true, params },
        b'l' => Parsed::Toggle { len: end + 1, set: false, params },
        _ => Parsed::Other,
    }
}

/// The report for the current theme: `CSI ? 997 ; 1 n` when dark, `; 2 n` when light.
pub(crate) fn theme_report(dark: bool) -> &'static [u8] {
    if dark { b"\x1b[?997;1n" } else { b"\x1b[?997;2n" }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn mode_2031_toggles_are_detected() {
        let cases: [(&[u8], Option<bool>); 9] = [
            (b"\x1b[?2031h", Some(true)),
            (b"\x1b[?2031l", Some(false)),
            (b"\x1b[?1004;2031;2004h", Some(true)),
            (b"text\x1b[?2031hmore\x1b[?2031l", Some(false)),
            (b"\x1b[?20310h", None),
            (b"\x1b[?12031h", None),
            (b"\x1b[?2004h\x1b[?1049h", None),
            (b"\x1b]0;?2031h\x07", None),
            (b"\x1b[2031h", None),
        ];
        for (input, want) in cases {
            assert_eq!(ThemeReportScanner::new().feed(input), want, "input {input:?}");
        }
    }

    #[test]
    fn a_toggle_split_across_reads_is_still_seen() {
        let whole = b"abc\x1b[?1004;2031h";
        for cut in 0..whole.len() {
            let mut sc = ThemeReportScanner::new();
            let first = sc.feed(&whole[..cut]);
            let second = sc.feed(&whole[cut..]);
            assert_eq!(first.or(second), Some(true), "cut at {cut}");
        }
    }

    #[test]
    fn runaway_params_are_not_carried_forever() {
        let mut sc = ThemeReportScanner::new();
        let long = [b"\x1b[?".as_slice(), &[b'1'; 200]].concat();
        assert_eq!(sc.feed(&long), None);
        assert!(sc.carry.is_empty());
    }

    #[test]
    fn reports_follow_the_theme_darkness() {
        assert_eq!(theme_report(true), b"\x1b[?997;1n");
        assert_eq!(theme_report(false), b"\x1b[?997;2n");
    }

    fn toggles() -> impl Strategy<Value = Vec<u8>> {
        let piece = prop_oneof![
            Just(b"\x1b[?2031h".to_vec()),
            Just(b"\x1b[?2031l".to_vec()),
            Just(b"\x1b[?1004;2031h".to_vec()),
            Just(b"\x1b[?25l".to_vec()),
            proptest::collection::vec(any::<u8>(), 0..8),
        ];
        proptest::collection::vec(piece, 0..12).prop_map(|v| v.concat())
    }

    proptest! {
        #[test]
        fn split_invariant(data in toggles(), cut in 0usize..256) {
            let cut = cut.min(data.len());
            let whole = ThemeReportScanner::new().feed(&data);
            let mut sc = ThemeReportScanner::new();
            let first = sc.feed(&data[..cut]);
            let second = sc.feed(&data[cut..]);
            prop_assert_eq!(whole, second.or(first));
        }
    }
}
