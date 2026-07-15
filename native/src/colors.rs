//! The single color module. Holds the active `Theme` (set once at startup from config) and
//! all color accessors: terminal cell mapping + derived UI-chrome colors. Everything reads
//! the active theme, so swapping themes recolors the whole app.
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use eframe::egui::Color32;
use std::sync::OnceLock;

/// A full theme: default bg/fg/cursor + the 16 ANSI colors. UI-chrome colors are derived.
#[derive(Clone)]
pub struct Theme {
    pub bg: Color32,
    pub fg: Color32,
    pub cursor: Color32,
    pub ansi: [Color32; 16],
}

static THEME: OnceLock<Theme> = OnceLock::new();

pub fn init(theme: Theme) {
    let _ = THEME.set(theme);
}

fn theme() -> &'static Theme {
    THEME.get_or_init(one_half_dark)
}

// ---- terminal cell mapping ----

pub fn is_default_bg(c: &Color) -> bool {
    matches!(c, Color::Named(NamedColor::Background))
}

pub fn to_color32(c: Color) -> Color32 {
    match c {
        Color::Spec(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
        Color::Indexed(i) => indexed(i),
        Color::Named(n) => named(n),
    }
}

fn named(n: NamedColor) -> Color32 {
    use NamedColor::*;
    let a = &theme().ansi;
    match n {
        Black => a[0],
        Red => a[1],
        Green => a[2],
        Yellow => a[3],
        Blue => a[4],
        Magenta => a[5],
        Cyan => a[6],
        White => a[7],
        BrightBlack => a[8],
        BrightRed => a[9],
        BrightGreen => a[10],
        BrightYellow => a[11],
        BrightBlue => a[12],
        BrightMagenta => a[13],
        BrightCyan => a[14],
        BrightWhite => a[15],
        Background => theme().bg,
        _ => theme().fg, // Foreground, Cursor, Dim*, BrightForeground, ...
    }
}

fn indexed(i: u8) -> Color32 {
    match i {
        0..=15 => theme().ansi[i as usize],
        16..=231 => {
            let i = i - 16;
            let step = |v: u8| if v == 0 { 0 } else { v * 40 + 55 };
            Color32::from_rgb(step(i / 36), step((i % 36) / 6), step(i % 6))
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
    }
}

// ---- derived UI-chrome colors (everything keyed off the theme) ----

pub fn bg() -> Color32 {
    theme().bg
}
pub fn fg() -> Color32 {
    theme().fg
}
pub fn cursor() -> Color32 {
    theme().cursor
}
pub fn dim() -> Color32 {
    theme().ansi[8]
}
pub fn accent() -> Color32 {
    theme().ansi[4]
}
pub fn red() -> Color32 {
    theme().ansi[1]
}
pub fn green() -> Color32 {
    theme().ansi[2]
}
pub fn yellow() -> Color32 {
    theme().ansi[3]
}
pub fn panel() -> Color32 {
    shade(theme().bg, 0.78) // slightly darker than bg
}
pub fn elevated() -> Color32 {
    shade(theme().bg, 1.22) // slightly lighter than bg (active tab)
}
/// Swatches offered by the M5 right-click Color menu.
#[allow(dead_code)] // consumed by the M5 color picker
pub fn tab_colors() -> [Color32; 6] {
    let a = &theme().ansi;
    [a[1], a[4], a[3], a[5], a[2], a[6]]
}

fn shade(c: Color32, factor: f32) -> Color32 {
    let f = |v: u8| ((v as f32 * factor).round().clamp(0.0, 255.0)) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

// ---- built-in themes ----

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub fn one_half_dark() -> Theme {
    Theme {
        bg: rgb(0x28, 0x2c, 0x34),
        fg: rgb(0xdc, 0xdf, 0xe4),
        cursor: rgb(0x61, 0xaf, 0xef),
        ansi: [
            rgb(0x28, 0x2c, 0x34), rgb(0xe0, 0x6c, 0x75), rgb(0x98, 0xc3, 0x79), rgb(0xe5, 0xc0, 0x7b),
            rgb(0x61, 0xaf, 0xef), rgb(0xc6, 0x78, 0xdd), rgb(0x56, 0xb6, 0xc2), rgb(0xdc, 0xdf, 0xe4),
            rgb(0x5c, 0x63, 0x70), rgb(0xe0, 0x6c, 0x75), rgb(0x98, 0xc3, 0x79), rgb(0xe5, 0xc0, 0x7b),
            rgb(0x61, 0xaf, 0xef), rgb(0xc6, 0x78, 0xdd), rgb(0x56, 0xb6, 0xc2), rgb(0xff, 0xff, 0xff),
        ],
    }
}

pub fn dracula() -> Theme {
    Theme {
        bg: rgb(0x28, 0x2a, 0x36),
        fg: rgb(0xf8, 0xf8, 0xf2),
        cursor: rgb(0xbd, 0x93, 0xf9),
        ansi: [
            rgb(0x21, 0x22, 0x2c), rgb(0xff, 0x55, 0x55), rgb(0x50, 0xfa, 0x7b), rgb(0xf1, 0xfa, 0x8c),
            rgb(0xbd, 0x93, 0xf9), rgb(0xff, 0x79, 0xc6), rgb(0x8b, 0xe9, 0xfd), rgb(0xf8, 0xf8, 0xf2),
            rgb(0x62, 0x72, 0xa4), rgb(0xff, 0x6e, 0x6e), rgb(0x69, 0xff, 0x94), rgb(0xff, 0xff, 0xa5),
            rgb(0xd6, 0xac, 0xff), rgb(0xff, 0x92, 0xdf), rgb(0xa4, 0xff, 0xff), rgb(0xff, 0xff, 0xff),
        ],
    }
}

pub fn tokyo_night() -> Theme {
    Theme {
        bg: rgb(0x1a, 0x1b, 0x26),
        fg: rgb(0xc0, 0xca, 0xf5),
        cursor: rgb(0x7a, 0xa2, 0xf7),
        ansi: [
            rgb(0x15, 0x16, 0x1e), rgb(0xf7, 0x76, 0x8e), rgb(0x9e, 0xce, 0x6a), rgb(0xe0, 0xaf, 0x68),
            rgb(0x7a, 0xa2, 0xf7), rgb(0xbb, 0x9a, 0xf7), rgb(0x7d, 0xcf, 0xff), rgb(0xa9, 0xb1, 0xd6),
            rgb(0x41, 0x48, 0x68), rgb(0xf7, 0x76, 0x8e), rgb(0x9e, 0xce, 0x6a), rgb(0xe0, 0xaf, 0x68),
            rgb(0x7a, 0xa2, 0xf7), rgb(0xbb, 0x9a, 0xf7), rgb(0x7d, 0xcf, 0xff), rgb(0xc0, 0xca, 0xf5),
        ],
    }
}

/// Look up a built-in theme by config name (falls back to OneHalfDark).
pub fn by_name(name: &str) -> Theme {
    match name.to_ascii_lowercase().replace([' ', '_'], "-").as_str() {
        "dracula" => dracula(),
        "tokyo-night" | "tokyonight" => tokyo_night(),
        _ => one_half_dark(),
    }
}
