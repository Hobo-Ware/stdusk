//! Progress-on-tabs. Mirrors Tabby's %-regex scrape (alt-screen guarded) as the primary
//! signal - the exact regex and rules from tabby-terminal's baseTerminalTab.component.ts.
//! OSC 9;4 (parsed in osc.rs) is preferred by the caller when present.
use regex::Regex;
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Progress {
    #[default]
    None,
    Normal(u8),
    Error(u8),
    Indeterminate,
    Paused(u8),
}

// Tabby's exact regex: `/(^|[^\d])(\d+(\.\d+)?)%([^\d]|$)/`
static PCT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|[^\d])(\d+(\.\d+)?)%([^\d]|$)").unwrap());

pub struct ProgressScanner {
    enabled: bool,
    carry: String, // trailing digits carried so a % split across chunks still matches
}

impl ProgressScanner {
    pub fn new(enabled: bool) -> Self {
        Self { enabled, carry: String::new() }
    }

    /// One decision per chunk (Tabby semantics): a chunk with a valid percentage sets it,
    /// any other chunk clears to None. Suppressed while the alt-screen is active.
    pub fn feed(&mut self, chunk: &str, alt_screen: bool) -> Progress {
        if !self.enabled {
            self.carry.clear();
            return Progress::None;
        }
        let mut scan = std::mem::take(&mut self.carry);
        scan.push_str(chunk);

        if alt_screen {
            return Progress::None;
        }
        if let Some(caps) = PCT.captures(&scan) {
            if let Ok(v) = caps[2].parse::<f64>() {
                if v > 0.0 && v <= 100.0 {
                    return Progress::Normal(v as u8);
                }
            }
            return Progress::None; // matched a %, but out of range
        }
        // No match: if the chunk ends mid-number, keep those digits for the next boundary.
        self.carry = trailing_number(&scan);
        Progress::None
    }
}

/// Trailing run of digits/dot (capped), so "...42" + "%" reunites into "42%".
fn trailing_number(s: &str) -> String {
    let mut t: String = s
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    t.truncate(16);
    t.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(enabled: bool) -> ProgressScanner {
        ProgressScanner::new(enabled)
    }

    #[test]
    fn basic_percentages() {
        assert_eq!(s(true).feed("Installing... 42%", false), Progress::Normal(42));
        assert_eq!(s(true).feed("100%", false), Progress::Normal(100));
        assert_eq!(s(true).feed("done", false), Progress::None);
    }

    #[test]
    fn zero_and_out_of_range_are_none() {
        assert_eq!(s(true).feed("0%", false), Progress::None);
        assert_eq!(s(true).feed("150%", false), Progress::None);
    }

    #[test]
    fn float_truncates_like_parseint_branch() {
        assert_eq!(s(true).feed("downloading 3.5%", false), Progress::Normal(3));
    }

    #[test]
    fn alt_screen_suppresses() {
        assert_eq!(s(true).feed("42%", true), Progress::None);
    }

    #[test]
    fn disabled_never_fires() {
        assert_eq!(s(false).feed("42%", false), Progress::None);
    }

    #[test]
    fn split_across_chunks() {
        let mut sc = s(true);
        assert_eq!(sc.feed("Building ...42", false), Progress::None); // carries "42"
        assert_eq!(sc.feed("%\n", false), Progress::Normal(42));
    }

    #[test]
    fn clears_after_progress_line_ends() {
        let mut sc = s(true);
        assert_eq!(sc.feed("50%", false), Progress::Normal(50));
        assert_eq!(sc.feed("all done", false), Progress::None);
    }
}
