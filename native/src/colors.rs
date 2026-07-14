//! Map alacritty terminal colors -> egui Color32 using the OneHalfDark theme.
//! 16 ANSI + default fg/bg (M2); 256-cube + grayscale for Indexed; truecolor for Spec.
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use eframe::egui::Color32;

pub const BG: Color32 = Color32::from_rgb(0x28, 0x2c, 0x34);
pub const FG: Color32 = Color32::from_rgb(0xdc, 0xdf, 0xe4);

/// OneHalfDark ANSI 0-15 (normal 0-7, bright 8-15).
const ANSI16: [Color32; 16] = [
    Color32::from_rgb(0x28, 0x2c, 0x34), // 0 black
    Color32::from_rgb(0xe0, 0x6c, 0x75), // 1 red
    Color32::from_rgb(0x98, 0xc3, 0x79), // 2 green
    Color32::from_rgb(0xe5, 0xc0, 0x7b), // 3 yellow
    Color32::from_rgb(0x61, 0xaf, 0xef), // 4 blue
    Color32::from_rgb(0xc6, 0x78, 0xdd), // 5 magenta
    Color32::from_rgb(0x56, 0xb6, 0xc2), // 6 cyan
    Color32::from_rgb(0xdc, 0xdf, 0xe4), // 7 white
    Color32::from_rgb(0x5c, 0x63, 0x70), // 8 bright black
    Color32::from_rgb(0xe0, 0x6c, 0x75), // 9 bright red
    Color32::from_rgb(0x98, 0xc3, 0x79), // 10 bright green
    Color32::from_rgb(0xe5, 0xc0, 0x7b), // 11 bright yellow
    Color32::from_rgb(0x61, 0xaf, 0xef), // 12 bright blue
    Color32::from_rgb(0xc6, 0x78, 0xdd), // 13 bright magenta
    Color32::from_rgb(0x56, 0xb6, 0xc2), // 14 bright cyan
    Color32::from_rgb(0xff, 0xff, 0xff), // 15 bright white
];

/// True when the cell background is the terminal default (render transparent so the
/// translucent window shows through).
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
    match n {
        Black => ANSI16[0],
        Red => ANSI16[1],
        Green => ANSI16[2],
        Yellow => ANSI16[3],
        Blue => ANSI16[4],
        Magenta => ANSI16[5],
        Cyan => ANSI16[6],
        White => ANSI16[7],
        BrightBlack => ANSI16[8],
        BrightRed => ANSI16[9],
        BrightGreen => ANSI16[10],
        BrightYellow => ANSI16[11],
        BrightBlue => ANSI16[12],
        BrightMagenta => ANSI16[13],
        BrightCyan => ANSI16[14],
        BrightWhite => ANSI16[15],
        Background => BG,
        _ => FG, // Foreground, Cursor, Dim*, BrightForeground, ...
    }
}

fn indexed(i: u8) -> Color32 {
    match i {
        0..=15 => ANSI16[i as usize],
        16..=231 => {
            // 6x6x6 color cube
            let i = i - 16;
            let step = |v: u8| if v == 0 { 0 } else { v * 40 + 55 };
            Color32::from_rgb(step(i / 36), step((i % 36) / 6), step(i % 6))
        }
        232..=255 => {
            // grayscale ramp
            let v = 8 + (i - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
    }
}
