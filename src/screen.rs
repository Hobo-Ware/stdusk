//! Screen replay for session handoff: turn a live pane's grid back into ANSI so a successor
//! process can paint what was already on screen instead of starting blank.
//!
//! "alacritty's `Term` has no serialization" was long read here as "screen content cannot survive a
//! handoff". It does not follow: we own the donor's grid, so we can READ it and REPLAY it as escape
//! sequences - the same way tmux redraws a client that reattaches. The successor feeds the bytes
//! through its own parser, so it re-renders through ITS theme: the dump therefore carries the RAW
//! cell colors (`Color`), never resolved `Color32`.
//!
//! What it deliberately does NOT try to be: a full terminal-state serializer. Scroll regions, tab
//! stops, wrap flags, character sets and the cursor's saved state are not represented; a wrapped
//! long line comes back as two hard lines. The goal is the picture, plus a cursor in the right
//! place, and then live output takes over.
use std::fmt::Write as _;

use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color, NamedColor};

/// Scrollback lines carried on top of the visible screen. Covers the "my loop printed 100 lines"
/// case and a couple of screens of context; a full 25k-line history would be megabytes per pane for
/// content the user has already scrolled past.
pub(crate) const MAX_HISTORY_LINES: usize = 200;

/// Hard cap on one pane's dump. Lines are dropped OLDEST first, so the newest screen always
/// survives - a huge grid degrades to less scrollback rather than failing the handoff.
pub(crate) const MAX_DUMP_BYTES: usize = 128 * 1024;

/// One cell as the donor saw it. Raw `Color`s on purpose (see the module header).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Cell {
    pub(crate) c: char,
    pub(crate) fg: Color,
    pub(crate) bg: Color,
    pub(crate) flags: Flags,
}

impl Cell {
    /// A cell that carries nothing: trailing runs of these are dropped, which is what keeps a dump
    /// of a mostly-empty screen small.
    fn is_blank(&self) -> bool {
        self.c == ' '
            && matches!(self.bg, Color::Named(NamedColor::Background))
            && !self.flags.intersects(STYLE_FLAGS)
    }
}

/// The flags a dump reproduces. Everything else (wide-char bookkeeping, wrap markers, underline
/// variants) is either re-derived by the parser or deliberately dropped.
const STYLE_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::UNDERLINE)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

/// Serialize `lines` (oldest first; the LAST `screen_rows` of them are the visible screen) plus the
/// cursor's screen position into ANSI ready to feed a fresh terminal.
///
/// The lines are emitted separated by CRLF and NOT newline-terminated, so a fresh grid ends up with
/// the last `screen_rows` lines visible and everything before them scrolled into history - exactly
/// the donor's layout. `cursor` is `(row, col)` inside the visible screen, 0-based.
///
/// Over `MAX_DUMP_BYTES` the oldest lines are dropped; if that eats into the screen itself, the gap
/// is padded with empty lines so the surviving rows still land where they were and the cursor
/// position stays truthful.
pub(crate) fn encode(lines: &[Vec<Cell>], screen_rows: usize, cursor: (usize, usize)) -> Vec<u8> {
    // Nothing on screen: send nothing, so the caller can tell "no replay" from "a blank replay"
    // and fall back to asking the shell to repaint instead.
    if lines.iter().all(|l| l.iter().all(Cell::is_blank)) {
        return Vec::new();
    }
    let encoded: Vec<Vec<u8>> = lines.iter().map(|l| encode_line(l)).collect();
    // Keep the newest lines that fit (the joining CRLF counts, hence the +2).
    let mut budget = MAX_DUMP_BYTES;
    let mut keep = 0;
    for line in encoded.iter().rev() {
        let cost = line.len() + 2;
        if cost > budget {
            break;
        }
        budget -= cost;
        keep += 1;
    }
    let kept = &encoded[encoded.len() - keep..];
    // Screen rows lost to the cap come back as blanks, so the rest keeps its row.
    let pad = screen_rows.saturating_sub(keep);

    let mut out = Vec::with_capacity(MAX_DUMP_BYTES.min(kept.iter().map(Vec::len).sum::<usize>()));
    out.extend_from_slice(b"\x1b[0m");
    for i in 0..pad {
        if i > 0 {
            out.extend_from_slice(b"\r\n");
        }
    }
    for (i, line) in kept.iter().enumerate() {
        if i > 0 || pad > 0 {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(line);
    }
    // Reset attributes before live output resumes, then put the cursor back (1-based CUP).
    let _ = write!(
        StringSink(&mut out),
        "\x1b[0m\x1b[{};{}H",
        cursor.0.saturating_add(1),
        cursor.1.saturating_add(1)
    );
    out
}

/// One line, self-contained: it re-states its own attributes, so dropping earlier lines can never
/// leave a stale SGR state behind.
fn encode_line(cells: &[Cell]) -> Vec<u8> {
    let end = cells.iter().rposition(|c| !c.is_blank()).map_or(0, |i| i + 1);
    let mut out = Vec::new();
    let mut style: Option<(Color, Color, Flags)> = None;
    for cell in &cells[..end] {
        // The trailing half of a wide glyph is not a character of its own - the wide cell before it
        // already carries the whole thing, and emitting a second one would shift the row.
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let want = (cell.fg, cell.bg, cell.flags & STYLE_FLAGS);
        if style != Some(want) {
            out.extend_from_slice(sgr(want.0, want.1, want.2).as_bytes());
            style = Some(want);
        }
        let mut buf = [0u8; 4];
        out.extend_from_slice(cell.c.encode_utf8(&mut buf).as_bytes());
    }
    out
}

/// An absolute SGR sequence (always from a reset) for one cell style. Absolute rather than
/// incremental so a state comparison is all the caller needs to decide whether to emit anything.
fn sgr(fg: Color, bg: Color, flags: Flags) -> String {
    let mut s = String::from("\x1b[0");
    for (flag, code) in [
        (Flags::BOLD, 1),
        (Flags::DIM, 2),
        (Flags::ITALIC, 3),
        (Flags::UNDERLINE, 4),
        (Flags::INVERSE, 7),
        (Flags::HIDDEN, 8),
        (Flags::STRIKEOUT, 9),
    ] {
        if flags.contains(flag) {
            let _ = write!(s, ";{code}");
        }
    }
    write_color(&mut s, fg, true);
    write_color(&mut s, bg, false);
    s.push('m');
    s
}

/// SGR parameters for one color slot. The default fg/bg are left implicit (the leading `0` reset
/// already selected them), which keeps the common case short.
fn write_color(s: &mut String, c: Color, foreground: bool) {
    let extended = if foreground { 38 } else { 48 };
    match c {
        Color::Named(n) => {
            if let Some(i) = palette_index(n) {
                let _ = write!(s, ";{extended};5;{i}");
            }
        }
        Color::Indexed(i) => {
            let _ = write!(s, ";{extended};5;{i}");
        }
        Color::Spec(rgb) => {
            let _ = write!(s, ";{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b);
        }
    }
}

/// A named color as a 256-palette index, or `None` for "the default" (which needs no parameter).
/// The `Dim*` names map onto their base color - the DIM flag travels separately - and `Cursor`,
/// which is not a text color at all, falls back to the default.
fn palette_index(n: NamedColor) -> Option<u8> {
    use NamedColor::{
        Background, Black, Blue, BrightBlack, BrightBlue, BrightCyan, BrightForeground,
        BrightGreen, BrightMagenta, BrightRed, BrightWhite, BrightYellow, Cursor, Cyan, DimBlack,
        DimBlue, DimCyan, DimForeground, DimGreen, DimMagenta, DimRed, DimWhite, DimYellow,
        Foreground, Green, Magenta, Red, White, Yellow,
    };
    match n {
        Black | DimBlack => Some(0),
        Red | DimRed => Some(1),
        Green | DimGreen => Some(2),
        Yellow | DimYellow => Some(3),
        Blue | DimBlue => Some(4),
        Magenta | DimMagenta => Some(5),
        Cyan | DimCyan => Some(6),
        White | DimWhite => Some(7),
        BrightBlack => Some(8),
        BrightRed => Some(9),
        BrightGreen => Some(10),
        BrightYellow => Some(11),
        BrightBlue => Some(12),
        BrightMagenta => Some(13),
        BrightCyan => Some(14),
        BrightWhite => Some(15),
        Foreground | Background | Cursor | BrightForeground | DimForeground => None,
    }
}

/// `write!` into a byte vec without pulling in `io::Write` (which would make every call fallible
/// for no reason - a `Vec` push cannot fail).
struct StringSink<'a>(&'a mut Vec<u8>);
impl std::fmt::Write for StringSink<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(s: &str) -> Vec<Cell> {
        s.chars()
            .map(|c| Cell {
                c,
                fg: Color::Named(NamedColor::Foreground),
                bg: Color::Named(NamedColor::Background),
                flags: Flags::empty(),
            })
            .collect()
    }

    /// Pad a line to `cols` with blanks, like a real grid row.
    fn row(s: &str, cols: usize) -> Vec<Cell> {
        let mut cells = plain(s);
        while cells.len() < cols {
            cells.extend(plain(" "));
        }
        cells
    }

    #[test]
    fn a_plain_screen_replays_as_its_lines_and_a_cursor_jump() {
        let lines = vec![row("hello", 20), row("world", 20), row("", 20)];
        let out = String::from_utf8(encode(&lines, 3, (2, 0))).unwrap();
        // Trailing blanks are dropped (a full-width pad would triple the payload), lines are
        // CRLF-separated and NOT newline-terminated, and the cursor is restored 1-based. Each line
        // re-states its own attributes (the leading `ESC[0m` per line) so any of them can be
        // dropped by the size cap without leaving a stale style behind.
        assert_eq!(out, "\x1b[0m\x1b[0mhello\r\n\x1b[0mworld\r\n\x1b[0m\x1b[3;1H");
    }

    #[test]
    fn attributes_are_emitted_once_per_run_and_survive_the_round_trip() {
        let mut cells = plain("ab");
        cells[1].flags = Flags::BOLD;
        cells[1].fg = Color::Indexed(200);
        let mut tail = plain("cd");
        tail[0].flags = Flags::BOLD; // same style as 'b': one SGR must cover both
        tail[0].fg = Color::Indexed(200);
        tail[1].bg = Color::Spec(alacritty_terminal::vte::ansi::Rgb { r: 1, g: 2, b: 3 });
        cells.extend(tail);
        let out = String::from_utf8(encode(&[cells], 1, (0, 4))).unwrap();
        assert_eq!(
            out, "\x1b[0m\x1b[0ma\x1b[0;1;38;5;200mbc\x1b[0;48;2;1;2;3md\x1b[0m\x1b[1;5H",
            "one SGR per style RUN, absolute (indexed fg, truecolor bg, bold)"
        );
    }

    #[test]
    fn a_wide_glyphs_spacer_cell_is_not_emitted_twice() {
        // The grid stores a wide char plus a spacer cell; replaying the spacer would shift the row.
        let mut cells = plain("你 x");
        cells[0].flags = Flags::WIDE_CHAR;
        cells[1].flags = Flags::WIDE_CHAR_SPACER;
        let out = String::from_utf8(encode(&[cells], 1, (0, 0))).unwrap();
        assert!(out.contains("你x"), "got {out:?}");
    }

    #[test]
    fn named_colors_map_to_their_palette_slots() {
        assert_eq!(palette_index(NamedColor::Red), Some(1));
        assert_eq!(palette_index(NamedColor::BrightWhite), Some(15));
        // Dim keeps the base color; the DIM flag carries the dimming.
        assert_eq!(palette_index(NamedColor::DimRed), palette_index(NamedColor::Red));
        // The defaults have no parameter at all - the SGR reset already selected them.
        for n in [NamedColor::Foreground, NamedColor::Background, NamedColor::Cursor] {
            assert_eq!(palette_index(n), None);
        }
        assert_eq!(
            sgr(
                Color::Named(NamedColor::Foreground),
                Color::Named(NamedColor::Background),
                Flags::empty()
            ),
            "\x1b[0m"
        );
    }

    #[test]
    fn an_oversized_dump_drops_the_oldest_lines_and_keeps_the_screen_in_place() {
        // 8 rows of screen preceded by history far beyond the cap: the newest content must survive,
        // the payload must respect the cap, and the cursor must still point at the right row.
        // Real content, not padding: trailing blanks are trimmed, so a "wide" line only costs
        // bytes if it is actually full.
        let fill = "-".repeat(300);
        let history: Vec<Vec<Cell>> =
            (0..2000).map(|i| plain(&format!("history-{i}{fill}"))).collect();
        let screen: Vec<Vec<Cell>> = (0..8).map(|i| plain(&format!("screen-{i}{fill}"))).collect();
        let lines: Vec<Vec<Cell>> = history.into_iter().chain(screen).collect();
        let out = String::from_utf8(encode(&lines, 8, (7, 3))).unwrap();

        assert!(out.len() <= MAX_DUMP_BYTES + 32, "payload {} bytes", out.len());
        assert!(out.contains("screen-7"), "the newest screen line must never be dropped");
        assert!(out.contains("\x1b[8;4H"), "the cursor row must still be the donor's, got {out:?}");
        assert!(!out.contains("history-0-"), "the oldest history must be the first thing dropped");
        assert!(out.contains("history-1999"), "the newest history must survive");
    }

    #[test]
    fn a_screen_bigger_than_the_cap_still_lands_at_the_right_row() {
        // Pathological: one screen alone exceeds the cap, so even screen rows are dropped. The
        // survivors are padded back down so the cursor row stays truthful.
        let wide = "x".repeat(4000);
        let lines: Vec<Vec<Cell>> = (0..60).map(|_| plain(&wide)).collect();
        let out = String::from_utf8(encode(&lines, 60, (59, 0))).unwrap();
        assert!(out.len() <= MAX_DUMP_BYTES + 32);
        assert!(out.ends_with("\x1b[0m\x1b[60;1H"), "cursor must stay on the donor's row");
        // The kept lines plus the padding still add up to the full screen height.
        assert_eq!(out.matches("\r\n").count(), 59);
    }

    #[test]
    fn a_blank_screen_dumps_to_nothing_at_all() {
        // "No replay" has to be distinguishable from "a replay of an empty screen": the caller
        // falls back to asking the shell to repaint, which is right for a blank pane.
        assert!(encode(&[], 0, (0, 0)).is_empty());
        assert!(encode(&[row("", 40), row("", 40)], 2, (1, 0)).is_empty());
        assert!(!encode(&[row("", 40), row("x", 40)], 2, (1, 1)).is_empty());
    }
}
