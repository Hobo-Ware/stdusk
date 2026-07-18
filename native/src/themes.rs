//! Community color schemes (XRDB/Xresources format): the 191 schemes vendored from Tabby's
//! community pack (embedded at build time by build.rs) plus any user files in
//! ~/.config/stdusk/schemes/. User files shadow the embedded pack.
use crate::colors::Theme;
use eframe::egui::Color32;
use std::collections::HashMap;

include!(concat!(env!("OUT_DIR"), "/schemes.rs"));

/// Look up a non-built-in scheme by normalized name (lowercase, spaces/underscores -> '-').
/// Rare (theme switch only), so parse on hit - no cache.
pub(crate) fn lookup(name: &str) -> Option<Theme> {
    user_scheme(name)
        .or_else(|| SCHEMES.iter().find(|(n, _)| *n == name).and_then(|(_, src)| parse_xrdb(src)))
}

/// Scan ~/.config/stdusk/schemes/ for a file whose stem normalizes to `name`.
fn user_scheme(name: &str) -> Option<Theme> {
    let dir = std::path::Path::new(&std::env::var_os("HOME")?).join(".config/stdusk/schemes");
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        if stem.to_ascii_lowercase().replace([' ', '_'], "-") == name
            && let Ok(src) = std::fs::read_to_string(&path)
            && let Some(theme) = parse_xrdb(&src)
        {
            return Some(theme);
        }
    }
    None
}

/// Parse an Xresources scheme (the format Tabby's community pack uses): `#define` variables
/// plus `*.key: value` / `*key: value` lines. Later lines win, which also resolves the
/// `#ifdef`/`#else` background trick some base16 schemes use.
fn parse_xrdb(src: &str) -> Option<Theme> {
    let mut vars: HashMap<&str, &str> = HashMap::new();
    let mut values: HashMap<&str, &str> = HashMap::new();
    for line in src.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#define ") {
            let mut parts = rest.split_whitespace();
            if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                vars.insert(k, v);
            }
        } else if let Some(rest) = line.strip_prefix("*.").or_else(|| line.strip_prefix('*'))
            && let Some((key, value)) = rest.split_once(':')
        {
            let value = value.trim();
            values.insert(key.trim(), vars.get(value).copied().unwrap_or(value));
        }
    }

    let color = |key: &str| values.get(key).and_then(|v| parse_hex(v));
    let fg = color("foreground")?;
    let bg = color("background")?;
    let cursor = color("cursorColor").unwrap_or(fg);
    let mut ansi = [Color32::TRANSPARENT; 16];
    for (i, slot) in ansi.iter_mut().enumerate().take(8) {
        *slot = color(&format!("color{i}"))?;
    }
    for i in 8..16 {
        // Schemes without brights mirror the base colors.
        ansi[i] = color(&format!("color{i}")).unwrap_or(ansi[i - 8]);
    }
    Some(Theme { bg, fg, cursor, ansi })
}

/// `#rgb` or `#rrggbb` -> Color32.
fn parse_hex(s: &str) -> Option<Color32> {
    let hex = s.strip_prefix('#')?;
    let byte = |h: &str| u8::from_str_radix(h, 16).ok();
    match hex.len() {
        3 => {
            let nibble = |i: usize| byte(&hex[i..=i]).map(|n| n * 17);
            Some(Color32::from_rgb(nibble(0)?, nibble(1)?, nibble(2)?))
        }
        6 => Some(Color32::from_rgb(byte(&hex[0..2])?, byte(&hex[2..4])?, byte(&hex[4..6])?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
#define base00 #181818
#define base08 #ab4642
*.foreground:  #dcdfe4
*.background:  base00
*.cursorColor: #fff
*.color0: base00
*.color1: base08
*.color2: #50fa7b
*.color3: #f1fa8c
*.color4: #bd93f9
*.color5: #ff79c6
*.color6: #8be9fd
*.color7: #bbbbbb
*.color8: #555555
*.color9: #ff5555
*.color10: #50fa7b
*.color11: #f1fa8c
*.color12: #bd93f9
*.color13: #ff79c6
*.color14: #8be9fd
*.color15: #ffffff
";

    #[test]
    fn parses_fixture_with_defines() {
        let t = parse_xrdb(FIXTURE).unwrap();
        assert_eq!(t.bg, Color32::from_rgb(0x18, 0x18, 0x18));
        assert_eq!(t.fg, Color32::from_rgb(0xdc, 0xdf, 0xe4));
        assert_eq!(t.ansi[1], Color32::from_rgb(0xab, 0x46, 0x42));
        assert_eq!(t.ansi[15], Color32::from_rgb(0xff, 0xff, 0xff));
    }

    #[test]
    fn three_digit_hex_expands() {
        let t = parse_xrdb(FIXTURE).unwrap();
        assert_eq!(t.cursor, Color32::from_rgb(0xff, 0xff, 0xff));
        assert_eq!(parse_hex("#a1c"), Some(Color32::from_rgb(0xaa, 0x11, 0xcc)));
    }

    #[test]
    fn missing_colors_returns_none() {
        assert!(parse_xrdb("*.foreground: #fff\n*.background: #000\n*.color0: #000\n").is_none());
    }

    #[test]
    fn embedded_pack_lookup() {
        assert!(SCHEMES.len() >= 150);
        for name in ["solarized-dark", "nord"] {
            let t = lookup(name).unwrap();
            assert_ne!(t.ansi[1], Color32::TRANSPARENT);
            // ansi[1] should be a plausible red.
            assert!(t.ansi[1].r() > t.ansi[1].g());
        }
    }
}
