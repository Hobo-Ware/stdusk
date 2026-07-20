//! The single color module. Holds the active `Theme` (set once at startup from config) and
//! all color accessors: terminal cell mapping + derived UI-chrome colors. Everything reads
//! the active theme, so swapping themes recolors the whole app.
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use eframe::egui::Color32;
use std::sync::{LazyLock, RwLock};

/// A full theme: default bg/fg/cursor + the 16 ANSI colors. UI-chrome colors are derived.
#[derive(Clone, Copy)]
pub(crate) struct Theme {
    pub(crate) bg: Color32,
    pub(crate) fg: Color32,
    pub(crate) cursor: Color32,
    pub(crate) ansi: [Color32; 16],
}

// Swappable at runtime (e.g. follow-OS light/dark), so read/write behind a lock. `Theme` is `Copy`
// and tiny, so accessors copy it out - reads never hold the lock across work.
static THEME: LazyLock<RwLock<Theme>> = LazyLock::new(|| RwLock::new(one_half_dark()));

/// Set the active theme at startup.
pub(crate) fn init(theme: Theme) {
    set(theme);
}

/// Swap the active theme (recolors the whole app on the next repaint).
pub(crate) fn set(theme: Theme) {
    *THEME.write().unwrap() = theme;
}

fn theme() -> Theme {
    *THEME.read().unwrap()
}

// ---- terminal cell mapping ----

pub(crate) fn is_default_bg(c: Color) -> bool {
    matches!(c, Color::Named(NamedColor::Background))
}

pub(crate) fn to_color32(c: Color) -> Color32 {
    match c {
        Color::Spec(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
        Color::Indexed(i) => indexed(i),
        Color::Named(n) => named(n),
    }
}

/// Foreground for a cell: when `bold` (drawBoldTextInBrightColors), promote the 8 base ANSI
/// colors to their bright counterparts; everything else is unchanged.
pub(crate) fn cell_fg(c: Color, bold: bool) -> Color32 {
    use NamedColor::{Black, Blue, Cyan, Green, Magenta, Red, White, Yellow};
    if !bold {
        return to_color32(c);
    }
    match c {
        Color::Indexed(i @ 0..=7) => indexed(i + 8),
        Color::Named(n) => match n {
            Black => theme().ansi[8],
            Red => theme().ansi[9],
            Green => theme().ansi[10],
            Yellow => theme().ansi[11],
            Blue => theme().ansi[12],
            Magenta => theme().ansi[13],
            Cyan => theme().ansi[14],
            White => theme().ansi[15],
            other => named(other),
        },
        other => to_color32(other),
    }
}

fn named(n: NamedColor) -> Color32 {
    use NamedColor::*;
    let a = theme().ansi;
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

/// Color reported for an OSC 4;n / 10 / 11 / 12 palette QUERY, from the given theme. Index
/// space is alacritty's: 0-255 palette, then `NamedColor::Foreground/Background/Cursor`
/// (256/257/258).
pub(crate) fn query_color_in(t: &Theme, index: usize) -> Color32 {
    match index {
        0..=15 => t.ansi[index],
        16..=255 => indexed(index as u8),
        257 => t.bg,
        258 => t.cursor,
        _ => t.fg, // 256 (Foreground) + anything exotic
    }
}

/// Live-theme `query_color_in`: queries answer with whatever theme is active at reply time.
pub(crate) fn query_color(index: usize) -> Color32 {
    query_color_in(&theme(), index)
}

// ---- derived UI-chrome colors (everything keyed off the theme) ----

pub(crate) fn bg() -> Color32 {
    theme().bg
}
pub(crate) fn fg() -> Color32 {
    theme().fg
}
pub(crate) fn cursor() -> Color32 {
    theme().cursor
}
/// Dim chrome/UI text. NOT raw `ansi[8]`: the 0.5.0 a11y audit found 37% of the community
/// pack has ansi[8] nearly invisible against bg (64% have some ANSI color at ratio ~1.0), so
/// chrome text gets a 3:1 WCAG floor. Terminal CELLS stay theme-exact - that fidelity knob is
/// `terminal.minimum_contrast`.
pub(crate) fn dim() -> Color32 {
    legible_dim(&theme())
}
/// A theme's dim text color, nudged (only when needed) to at least 3:1 against its bg.
pub(crate) fn legible_dim(t: &Theme) -> Color32 {
    ensure_contrast(t.ansi[8], t.bg, 3.0)
}
pub(crate) fn accent() -> Color32 {
    theme().ansi[4]
}
pub(crate) fn red() -> Color32 {
    theme().ansi[1]
}
pub(crate) fn green() -> Color32 {
    theme().ansi[2]
}
pub(crate) fn yellow() -> Color32 {
    theme().ansi[3]
}
/// True when the theme background is dark (drives the direction of the derived chrome shades, so
/// the tab strip / active tab / borders read correctly on both light and dark themes).
pub(crate) fn is_dark() -> bool {
    theme_is_dark(&theme())
}
/// True when a THEME's background is dark (perceived luminance, same rule the chrome shades
/// use). Also classifies schemes for the browser's All/Light/Dark filter.
pub(crate) fn theme_is_dark(t: &Theme) -> bool {
    let c = t.bg;
    0.299 * f32::from(c.r()) + 0.587 * f32::from(c.g()) + 0.114 * f32::from(c.b()) < 128.0
}
/// `COLORFGBG` env advertisement (`fg;bg` ANSI indices): the pre-OSC-11 way CLIs guess light
/// vs dark. Reads the live theme; spawn snapshots it into the child's env.
pub(crate) fn colorfgbg() -> &'static str {
    colorfgbg_for(is_dark())
}
pub(crate) fn colorfgbg_for(dark: bool) -> &'static str {
    if dark { "15;0" } else { "0;15" }
}
pub(crate) fn elevated() -> Color32 {
    shade(theme().bg, if is_dark() { 1.28 } else { 0.90 }) // active-tab bg, set off from the bar
}
pub(crate) fn titlebar() -> Color32 {
    shade(theme().bg, if is_dark() { 0.72 } else { 0.93 }) // tab-bar strip, separated from the body
}
pub(crate) fn border() -> Color32 {
    shade(theme().bg, if is_dark() { 1.6 } else { 0.78 }) // hairline divider
}
/// Subtle hover-highlight fill for icon buttons.
pub(crate) fn hover() -> Color32 {
    let e = elevated();
    Color32::from_rgba_unmultiplied(e.r(), e.g(), e.b(), 160)
}
/// Hover fill for rows on the `elevated` menu/popup surface. `hover()` is elevated-at-alpha,
/// which disappears when painted OVER elevated - this is one visible step further instead.
pub(crate) fn hover_elevated() -> Color32 {
    shade(theme().bg, if is_dark() { 1.75 } else { 0.84 })
}
/// Translucent fill painted over selected cells.
pub(crate) fn selection() -> Color32 {
    let a = accent();
    Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), 90)
}
/// Dim fill for every (non-current) search match; the current one keeps the brighter
/// selection fill on top.
pub(crate) fn search_match() -> Color32 {
    let a = accent();
    Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), 45)
}

// ---- minimum contrast (terminal.minimum_contrast) ----

/// WCAG relative luminance of an sRGB color (0 = black, 1 = white).
fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let s = f32::from(v) / 255.0;
        if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG contrast ratio between two colors: 1 (identical) ..= 21 (black on white).
pub(crate) fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Nudge `fg` toward black or white (whichever side of `bg` has more headroom) until it meets
/// the WCAG `ratio` against `bg`. Ratio <= 1 (or an already-passing pair) returns `fg`
/// unchanged; an unreachable ratio returns the pure target. Stepped blend (not a bisection):
/// contrast isn't monotonic when the blend crosses the background's luminance.
pub(crate) fn ensure_contrast(fg: Color32, bg: Color32, ratio: f32) -> Color32 {
    let ratio = ratio.clamp(1.0, 21.0);
    if contrast_ratio(fg, bg) >= ratio {
        return fg;
    }
    let target = if luminance(bg) < 0.1791 { Color32::WHITE } else { Color32::BLACK };
    let mix = |t: f32| {
        let l = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        Color32::from_rgb(l(fg.r(), target.r()), l(fg.g(), target.g()), l(fg.b(), target.b()))
    };
    for i in 1..=20 {
        let c = mix(i as f32 / 20.0);
        if contrast_ratio(c, bg) >= ratio {
            return c;
        }
    }
    target
}
/// Swatches offered by the right-click Color menu - a curated vivid palette (2 rows of 6),
/// theme-independent so tab underlines read cleanly on any background.
pub(crate) fn tab_colors() -> [Color32; 12] {
    [
        rgb(0xe0, 0x6c, 0x75), // red
        rgb(0xff, 0x9e, 0x64), // orange
        rgb(0xe5, 0xc0, 0x7b), // amber
        rgb(0xf1, 0xfa, 0x8c), // yellow
        rgb(0x9e, 0xce, 0x6a), // green
        rgb(0x50, 0xfa, 0x7b), // mint
        rgb(0x56, 0xb6, 0xc2), // teal
        rgb(0x7d, 0xcf, 0xff), // sky
        rgb(0x61, 0xaf, 0xef), // blue
        rgb(0x7a, 0xa2, 0xf7), // indigo
        rgb(0xc6, 0x78, 0xdd), // purple
        rgb(0xff, 0x79, 0xc6), // pink
    ]
}

fn shade(c: Color32, factor: f32) -> Color32 {
    let f = |v: u8| ((f32::from(v) * factor).round().clamp(0.0, 255.0)) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

// ---- built-in themes ----

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub(crate) fn one_half_dark() -> Theme {
    Theme {
        bg: rgb(0x28, 0x2c, 0x34),
        fg: rgb(0xdc, 0xdf, 0xe4),
        cursor: rgb(0x61, 0xaf, 0xef),
        ansi: [
            rgb(0x28, 0x2c, 0x34),
            rgb(0xe0, 0x6c, 0x75),
            rgb(0x98, 0xc3, 0x79),
            rgb(0xe5, 0xc0, 0x7b),
            rgb(0x61, 0xaf, 0xef),
            rgb(0xc6, 0x78, 0xdd),
            rgb(0x56, 0xb6, 0xc2),
            rgb(0xdc, 0xdf, 0xe4),
            rgb(0x5c, 0x63, 0x70),
            rgb(0xe0, 0x6c, 0x75),
            rgb(0x98, 0xc3, 0x79),
            rgb(0xe5, 0xc0, 0x7b),
            rgb(0x61, 0xaf, 0xef),
            rgb(0xc6, 0x78, 0xdd),
            rgb(0x56, 0xb6, 0xc2),
            rgb(0xff, 0xff, 0xff),
        ],
    }
}

pub(crate) fn one_half_light() -> Theme {
    Theme {
        bg: rgb(0xfa, 0xfa, 0xfa),
        fg: rgb(0x38, 0x3a, 0x42),
        cursor: rgb(0x01, 0x84, 0xbc),
        ansi: [
            rgb(0x38, 0x3a, 0x42),
            rgb(0xe4, 0x56, 0x49),
            rgb(0x50, 0xa1, 0x4f),
            rgb(0xc1, 0x84, 0x01),
            rgb(0x01, 0x84, 0xbc),
            rgb(0xa6, 0x26, 0xa4),
            rgb(0x09, 0x97, 0xb3),
            rgb(0xfa, 0xfa, 0xfa),
            rgb(0x4f, 0x52, 0x5e),
            rgb(0xe4, 0x56, 0x49),
            rgb(0x50, 0xa1, 0x4f),
            rgb(0xc1, 0x84, 0x01),
            rgb(0x01, 0x84, 0xbc),
            rgb(0xa6, 0x26, 0xa4),
            rgb(0x09, 0x97, 0xb3),
            rgb(0xff, 0xff, 0xff),
        ],
    }
}

pub(crate) fn dracula() -> Theme {
    Theme {
        bg: rgb(0x28, 0x2a, 0x36),
        fg: rgb(0xf8, 0xf8, 0xf2),
        cursor: rgb(0xbd, 0x93, 0xf9),
        ansi: [
            rgb(0x21, 0x22, 0x2c),
            rgb(0xff, 0x55, 0x55),
            rgb(0x50, 0xfa, 0x7b),
            rgb(0xf1, 0xfa, 0x8c),
            rgb(0xbd, 0x93, 0xf9),
            rgb(0xff, 0x79, 0xc6),
            rgb(0x8b, 0xe9, 0xfd),
            rgb(0xf8, 0xf8, 0xf2),
            rgb(0x62, 0x72, 0xa4),
            rgb(0xff, 0x6e, 0x6e),
            rgb(0x69, 0xff, 0x94),
            rgb(0xff, 0xff, 0xa5),
            rgb(0xd6, 0xac, 0xff),
            rgb(0xff, 0x92, 0xdf),
            rgb(0xa4, 0xff, 0xff),
            rgb(0xff, 0xff, 0xff),
        ],
    }
}

pub(crate) fn tokyo_night() -> Theme {
    Theme {
        bg: rgb(0x1a, 0x1b, 0x26),
        fg: rgb(0xc0, 0xca, 0xf5),
        cursor: rgb(0x7a, 0xa2, 0xf7),
        ansi: [
            rgb(0x15, 0x16, 0x1e),
            rgb(0xf7, 0x76, 0x8e),
            rgb(0x9e, 0xce, 0x6a),
            rgb(0xe0, 0xaf, 0x68),
            rgb(0x7a, 0xa2, 0xf7),
            rgb(0xbb, 0x9a, 0xf7),
            rgb(0x7d, 0xcf, 0xff),
            rgb(0xa9, 0xb1, 0xd6),
            rgb(0x41, 0x48, 0x68),
            rgb(0xf7, 0x76, 0x8e),
            rgb(0x9e, 0xce, 0x6a),
            rgb(0xe0, 0xaf, 0x68),
            rgb(0x7a, 0xa2, 0xf7),
            rgb(0xbb, 0x9a, 0xf7),
            rgb(0x7d, 0xcf, 0xff),
            rgb(0xc0, 0xca, 0xf5),
        ],
    }
}

/// Look up a built-in theme by config name (falls back to OneHalfDark).
pub(crate) fn by_name(name: &str) -> Theme {
    let norm = name.to_ascii_lowercase().replace([' ', '_'], "-");
    match norm.as_str() {
        "dracula" => dracula(),
        // Canonical spellings only: the pack ships distinct "tokyonight"/"onehalflight"
        // variants whose rows must apply the PACK palette, not the built-in namesake.
        "tokyo-night" => tokyo_night(),
        "one-half-light" | "light" => one_half_light(),
        // Rename alias (1.0.3): the pack's "Parasio Dark" was an identical typo-dupe of
        // "Paraiso Dark" and was dropped; saved configs must keep resolving.
        "parasio-dark" => crate::themes::lookup("paraiso-dark").unwrap_or_else(one_half_dark),
        // Fall back to the community XRDB pack + user schemes, then the default.
        _ => crate::themes::lookup(&norm).unwrap_or_else(one_half_dark),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_promotes_base_ansi_to_bright() {
        init(one_half_dark());
        let t = one_half_dark();
        // Named base colors -> bright counterparts when bold.
        assert_eq!(cell_fg(Color::Named(NamedColor::Red), true), t.ansi[9]);
        assert_eq!(cell_fg(Color::Named(NamedColor::Red), false), t.ansi[1]);
        // Indexed 0-7 -> 8-15.
        assert_eq!(cell_fg(Color::Indexed(2), true), t.ansi[10]);
        // Truecolor unchanged by bold.
        let spec = Color::Spec(alacritty_terminal::vte::ansi::Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(cell_fg(spec, true), Color32::from_rgb(1, 2, 3));
        // Bright colors stay bright.
        assert_eq!(cell_fg(Color::Named(NamedColor::BrightRed), true), t.ansi[9]);
    }

    #[test]
    fn dim_text_meets_the_floor_on_every_scheme() {
        // 0.5.0 a11y audit: 37% of the pack ships ansi[8] nearly invisible against bg (some
        // at ratio 1.0). Chrome dim text must clear 3:1 on EVERY scheme, built-ins included.
        for (name, t) in crate::themes::all_schemes() {
            let ratio = contrast_ratio(legible_dim(t), t.bg);
            assert!(ratio >= 2.99, "{name}: dim text ratio {ratio}");
        }
        for t in [one_half_dark(), one_half_light(), dracula(), tokyo_night()] {
            assert!(contrast_ratio(legible_dim(&t), t.bg) >= 2.99);
        }
    }

    /// Find a scheme in the embedded pack + built-ins by normalized name.
    fn scheme(name: &str) -> Theme {
        crate::themes::all_schemes()
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("missing scheme {name}"))
            .1
    }

    #[test]
    fn audit_critical_schemes_were_patched_to_aa() {
        // The 0.5.0 a11y audit's 4 broken-fg schemes (C64 2.26, Royal 2.34, Shaman 2.44,
        // CrayonPonyFish 2.76): fg is patched in the scheme DATA (1.0.3) with a minimal
        // ensure_contrast nudge to 4.5:1 instead of dropping the schemes.
        for name in ["c64", "royal", "shaman", "crayonponyfish"] {
            let t = scheme(name);
            let r = contrast_ratio(t.fg, t.bg);
            assert!(r >= 4.5, "{name}: fg/bg ratio {r}");
        }
    }

    #[test]
    fn vendored_light_schemes_classify_light_and_meet_aa() {
        // The 1.0.3 light-scheme expansion: every hand-vendored scheme must read as light
        // in the browser's brightness filter and pass WCAG AA for its base fg/bg pair.
        for name in [
            "alabaster",
            "catppuccin-latte",
            "dayfox",
            "edge-light",
            "everforest-light",
            "flexoki-light",
            "github-light",
            "gruvbox-light",
            "gruvbox-material-light",
            "iceberg-light",
            "papercolor-light",
            "selenized-light",
            "selenized-white",
            "tango-light",
            "zenbones-light",
        ] {
            let t = scheme(name);
            assert!(!theme_is_dark(&t), "{name} must classify as light");
            let r = contrast_ratio(t.fg, t.bg);
            assert!(r >= 4.5, "{name}: fg/bg ratio {r}");
        }
    }

    #[test]
    fn parasio_dark_alias_resolves_to_paraiso() {
        // Dropped typo-dupe keeps resolving for saved configs, and not via the fallback.
        let a = by_name("Parasio Dark");
        let b = scheme("paraiso-dark");
        assert_eq!(a.bg, b.bg);
        assert_eq!(a.fg, b.fg);
        assert_eq!(a.ansi, b.ansi);
        assert_ne!(a.bg, one_half_dark().bg); // would match if the alias fell through
    }

    #[test]
    fn pack_variant_spellings_apply_the_pack_not_the_builtin() {
        // Browser rows for the pack's TokyoNight/OneHalfLight write those names to config;
        // the built-in namesakes must not shadow them. Canonical spellings stay built-in.
        // Compare every palette field: the pack TokyoNight differs from the built-in only
        // in the cursor color.
        let key = |t: &Theme| (t.bg, t.fg, t.cursor, t.ansi);
        for (pack, builtin) in [("tokyonight", tokyo_night()), ("onehalflight", one_half_light())] {
            let picked = by_name(pack);
            let shipped = scheme(pack);
            assert_eq!(key(&picked), key(&shipped), "{pack} must resolve to the pack palette");
            assert_ne!(key(&picked), key(&builtin), "{pack} shadowed by the built-in");
        }
        assert_eq!(key(&by_name("tokyo-night")), key(&tokyo_night()));
        assert_eq!(key(&by_name("one-half-light")), key(&one_half_light()));
    }

    #[test]
    fn theme_darkness_classifies_builtins() {
        assert!(theme_is_dark(&one_half_dark()));
        assert!(theme_is_dark(&dracula()));
        assert!(theme_is_dark(&tokyo_night()));
        assert!(!theme_is_dark(&one_half_light()));
    }

    #[test]
    fn query_color_maps_palette_and_dynamic_indices() {
        // OSC 4;n / 10 / 11 / 12 query answers, per theme: ANSI 16, the 256-cube/grayscale,
        // then alacritty's dynamic indices (256 fg / 257 bg / 258 cursor).
        for t in [one_half_dark(), one_half_light()] {
            assert_eq!(query_color_in(&t, 1), t.ansi[1]);
            assert_eq!(query_color_in(&t, 15), t.ansi[15]);
            assert_eq!(query_color_in(&t, 196), Color32::from_rgb(255, 0, 0)); // cube red
            assert_eq!(query_color_in(&t, 232), Color32::from_rgb(8, 8, 8)); // grayscale start
            assert_eq!(query_color_in(&t, 256), t.fg);
            assert_eq!(query_color_in(&t, 257), t.bg);
            assert_eq!(query_color_in(&t, 258), t.cursor);
            assert_eq!(query_color_in(&t, 400), t.fg); // out of range -> fg fallback
        }
    }

    #[test]
    fn colorfgbg_advertises_theme_darkness() {
        assert_eq!(colorfgbg_for(true), "15;0"); // dark theme: light fg on black bg
        assert_eq!(colorfgbg_for(false), "0;15"); // light theme: dark fg on white bg
    }

    #[test]
    fn contrast_ratio_known_pairs() {
        // Black on white is the WCAG maximum; identical colors the minimum.
        assert!((contrast_ratio(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 0.001);
        // Symmetric.
        let (a, b) = (rgb(0x61, 0xaf, 0xef), rgb(0x28, 0x2c, 0x34));
        assert!((contrast_ratio(a, b) - contrast_ratio(b, a)).abs() < 1e-6);
        // #767676 on white is the canonical ~4.54:1 AA-boundary grey.
        let g = rgb(0x76, 0x76, 0x76);
        let r = contrast_ratio(g, Color32::WHITE);
        assert!((r - 4.54).abs() < 0.02, "got {r}");
    }

    #[test]
    fn ensure_contrast_meets_ratio_at_4_5() {
        let cases = [
            // (fg, bg) pairs that FAIL 4.5 and must be pushed to meet it.
            (rgb(0x88, 0x88, 0x88), Color32::WHITE), // grey on white -> darker
            (rgb(0x55, 0x55, 0x55), Color32::BLACK), // grey on black -> lighter
            (rgb(0x30, 0x30, 0x40), rgb(0x28, 0x2c, 0x34)), // near-bg fg crosses bg luminance
            (rgb(0xe0, 0x6c, 0x75), rgb(0xfa, 0xfa, 0xfa)), // theme red on light bg
        ];
        for (fg, bg) in cases {
            let out = ensure_contrast(fg, bg, 4.5);
            assert!(
                contrast_ratio(out, bg) >= 4.5,
                "{fg:?} on {bg:?} -> {out:?} = {}",
                contrast_ratio(out, bg)
            );
        }
    }

    #[test]
    fn ensure_contrast_leaves_passing_pairs_alone() {
        // Already-passing pair: untouched (the hot path early-return).
        let fg = rgb(0xdc, 0xdf, 0xe4);
        let bg = rgb(0x28, 0x2c, 0x34);
        assert!(contrast_ratio(fg, bg) >= 4.5);
        assert_eq!(ensure_contrast(fg, bg, 4.5), fg);
        // Ratio 1.0 = feature off: identity for any pair.
        let dim = rgb(0x30, 0x30, 0x30);
        assert_eq!(ensure_contrast(dim, rgb(0x28, 0x28, 0x28), 1.0), dim);
        // Unreachable ratio caps at the pure target (mid-grey bg: black has the headroom).
        let mid = rgb(0x80, 0x80, 0x80);
        assert_eq!(ensure_contrast(mid, mid, 21.0), Color32::BLACK);
    }
}
