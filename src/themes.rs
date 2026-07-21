//! Community color schemes (XRDB/Xresources format): 204 embedded schemes - Tabby's
//! community pack plus hand-vendored light schemes (embedded at build time by build.rs) -
//! plus any user files in
//! ~/.config/stdusk/schemes/. User files shadow the embedded pack.
use crate::colors::Theme;
use eframe::egui::Color32;
use std::collections::HashMap;
use std::sync::LazyLock;

include!(concat!(env!("OUT_DIR"), "/schemes.rs"));

/// Aggressive normalization for duplicate detection: lowercase, keep only alphanumerics. Collapses
/// "Tokyo Night", "tokyo-night", "TokyoNight", "tokyo_night" all to "tokyonight".
fn dedup_key(name: &str) -> String {
    name.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// Every browsable scheme (4 built-ins + the embedded pack), parsed once and cached so the
/// settings scheme list can draw palette previews every frame without re-parsing. Sorted by
/// normalized name; built-ins shadow same-`dedup_key` pack entries (no duplicate rows).
pub(crate) fn all_schemes() -> &'static [(String, Theme)] {
    static ALL: LazyLock<Vec<(String, Theme)>> = LazyLock::new(|| {
        let mut v: Vec<(String, Theme)> = vec![
            ("dracula".into(), crate::colors::dracula()),
            ("one-half-dark".into(), crate::colors::one_half_dark()),
            ("one-half-light".into(), crate::colors::one_half_light()),
            ("tokyo-night".into(), crate::colors::tokyo_night()),
        ];
        // Dedup by a STRONG key (lowercase, keep only alphanumerics) so pack files that are just
        // a re-spelling of a built-in - "TokyoNight" vs "tokyo-night", "OneHalfDark" vs
        // "one-half-dark", "Dracula" vs "dracula" - collapse to the one canonical (built-in)
        // entry instead of showing as duplicate rows. Built-ins are seeded first so they win.
        let mut seen: std::collections::HashSet<String> =
            v.iter().map(|(n, _)| dedup_key(n)).collect();
        for (name, src) in SCHEMES {
            if !seen.insert(dedup_key(name)) {
                continue;
            }
            if let Some(theme) = parse_xrdb(src) {
                v.push(((*name).to_string(), theme));
            }
        }
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    });
    &ALL
}

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
    // Byte-wise (never slice by byte index into user text: multibyte chars panic on char
    // boundaries). Non-ASCII-hex input just fails the parse.
    let b = hex.as_bytes();
    if !b.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let nib = |c: u8| (c as char).to_digit(16).map(|n| n as u8);
    match b.len() {
        3 => Some(Color32::from_rgb(nib(b[0])? * 17, nib(b[1])? * 17, nib(b[2])? * 17)),
        6 => {
            let byte = |i: usize| Some(nib(b[i])? * 16 + nib(b[i + 1])?);
            Some(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multibyte_hex_values_do_not_panic() {
        // Adversarial user scheme: multibyte chars where hex is expected used to panic on a
        // byte-index slice. Must parse to None, never crash.
        assert_eq!(parse_hex("#\u{fb00}"), None);
        assert_eq!(parse_hex("#\u{fb00}\u{fb00}\u{fb00}"), None);
        assert!(parse_xrdb("*.foreground: #\u{fb00}\n*.background: #000000").is_none());
    }

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
    fn all_schemes_sorted_unique_and_complete() {
        let all = all_schemes();
        assert!(all.len() >= 190, "pack + built-ins expected, got {}", all.len());
        for pair in all.windows(2) {
            assert!(pair[0].0 < pair[1].0, "not sorted/unique: {} vs {}", pair[0].0, pair[1].0);
        }
        for name in ["dracula", "nord", "one-half-dark", "one-half-light", "tokyo-night"] {
            assert!(all.iter().any(|(n, _)| n == name), "missing {name}");
        }
    }

    #[test]
    fn all_schemes_have_no_duplicate_names() {
        // No two rows may share a dedup key - catches "TokyoNight" vs "tokyo-night",
        // "OneHalfDark" vs "one-half-dark", "Solarized Dark" vs "Solarized Dark - Patched", etc.
        let all = all_schemes();
        let mut seen: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
        for (name, _) in all {
            if let Some(prev) = seen.insert(dedup_key(name), name) {
                panic!("duplicate scheme: {prev:?} and {name:?} share key {:?}", dedup_key(name));
            }
        }
        // The built-in canonical spellings must win over any pack re-spelling.
        for c in ["tokyo-night", "one-half-dark", "one-half-light", "dracula"] {
            assert!(all.iter().any(|(n, _)| n == c), "canonical {c} missing");
        }
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
