//! Font resolution + the egui font set. Turns an `appearance.font` FAMILY name into raw file
//! bytes via the system font source (core-text on macOS), picking the closest-to-regular and
//! closest-to-bold faces by NAME, and assembles the full `FontDefinitions` (icons + user font +
//! emoji/symbol fallbacks). Shared by startup and the settings live-apply so they can't drift.
use eframe::egui;

/// egui font-family name for the terminal's real bold face (registered by `build_fonts` only
/// when the user's font family resolves a bold sibling - the bundled default has none).
pub(crate) const BOLD_FONT_FAMILY: &str = "term-bold";

/// A user font resolved to raw file bytes + the face index inside the file (.ttc collections
/// like Menlo need the index; plain .ttf/.otf use 0).
pub(crate) struct ResolvedFont {
    bytes: Vec<u8>,
    index: u32,
}

/// Distance of a face from "plain regular", judged by its NAME - lowest wins. Slant keywords
/// dominate (an italic must never beat any upright face), then weight, then width.
/// Names, not `Font::properties()`: font-kit's core-text loader reports broken properties
/// (every Menlo face comes back `Italic, w400`) while `full_name()` is accurate.
fn face_name_score(name: &str) -> u32 {
    let n = name.to_ascii_lowercase();
    [
        ("italic", 100),
        ("oblique", 100),
        ("bold", 10),
        ("black", 8),
        ("heavy", 8),
        ("thin", 8),
        ("light", 8),
        ("medium", 4),
        ("condensed", 2),
    ]
    .iter()
    .filter(|(kw, _)| n.contains(kw))
    .map(|(_, pts)| pts)
    .sum()
}

/// Score a face as the family's BOLD sibling, judged by its NAME (same rationale as
/// `face_name_score`: core-text `properties()` lie). `None` disqualifies: the name must say
/// "bold" and must not be a slant. Among qualifiers, the plain Bold beats Semi/Extra/width
/// variants - lowest wins.
fn bold_face_name_score(name: &str) -> Option<u32> {
    let n = name.to_ascii_lowercase();
    if !n.contains("bold") || n.contains("italic") || n.contains("oblique") {
        return None;
    }
    Some(
        [
            ("semibold", 8),
            ("semi bold", 8),
            ("demibold", 8),
            ("demi bold", 8),
            ("extrabold", 4),
            ("extra bold", 4),
            ("ultrabold", 4),
            ("ultra bold", 4),
            ("condensed", 2),
            ("narrow", 2),
        ]
        .iter()
        .filter(|(kw, _)| n.contains(kw))
        .map(|(_, pts)| pts)
        .sum(),
    )
}

/// Pick the family face with the lowest `score(full_name())` and read its bytes + face index.
fn resolve_face(family: &str, score: impl Fn(&str) -> Option<u32>) -> Option<ResolvedFont> {
    use font_kit::source::SystemSource;
    let name = family.trim();
    if name.is_empty() {
        return None;
    }
    let fam = SystemSource::new().select_family_by_name(name).ok()?;
    let best = fam
        .fonts()
        .iter()
        .filter_map(|h| h.load().ok().and_then(|f| score(&f.full_name()).map(|s| (h, s))))
        .min_by_key(|&(_, s)| s)
        .map(|(h, _)| h)?;
    match best.clone() {
        font_kit::handle::Handle::Path { path, font_index } => {
            Some(ResolvedFont { bytes: std::fs::read(path).ok()?, index: font_index })
        }
        font_kit::handle::Handle::Memory { bytes, font_index } => {
            Some(ResolvedFont { bytes: (*bytes).clone(), index: font_index })
        }
    }
}

/// Resolve a font FAMILY name (e.g. "Menlo", "JetBrainsMono Nerd Font") to its font file via
/// the system font source (core-text on macOS). None for an empty or unknown family.
/// NOTE: font-kit's `select_best_match` is NOT face-accurate on macOS (it returned Menlo
/// *Italic* for "Menlo"), so select the family and pick the closest-to-regular face ourselves.
pub(crate) fn resolve_font(family: &str) -> Option<ResolvedFont> {
    resolve_face(family, |n| Some(face_name_score(n)))
}

/// Resolve the family's real BOLD face (upright, closest to plain Bold), or `None` when the
/// family doesn't ship one - bold cells then keep the regular face.
pub(crate) fn resolve_bold_font(family: &str) -> Option<ResolvedFont> {
    resolve_face(family, bold_face_name_score)
}

/// Installed font family names, sorted; cached (the font list doesn't change mid-run).
pub(crate) fn installed_families() -> &'static [String] {
    static FAMILIES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    FAMILIES.get_or_init(|| {
        let mut names = font_kit::source::SystemSource::new().all_families().unwrap_or_default();
        names.sort();
        names.dedup();
        names
    })
}

/// The full font set: egui defaults + Phosphor icons + the user's terminal font (when resolved)
/// at the TOP of the Monospace family + emoji/symbol fallbacks appended to both families.
/// Shared by startup and the settings live-apply so the two paths can't drift.
/// A resolved `bold` face registers a second family (`BOLD_FONT_FAMILY`) that `render_grid`
/// switches to for BOLD cells; without one, bold cells keep the regular face (the bright-ANSI
/// color treatment `terminal.bold_bright` is independent and stands either way).
pub(crate) fn build_fonts(
    custom: Option<ResolvedFont>,
    bold: Option<ResolvedFont>,
) -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    // Phosphor icon font (tab-bar controls + close x) as a fallback in the proportional
    // family, so icon codepoints render in buttons/labels.
    fonts.font_data.insert(
        "phosphor".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/Phosphor.ttf")).into(),
    );
    if let Some(keys) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        keys.insert(1, "phosphor".to_owned());
    }
    // The user's terminal font goes FIRST in Monospace only (chrome text stays the bundled
    // proportional); every fallback below stays behind it so emoji/symbols keep rendering.
    if let Some(f) = custom {
        let mut data = egui::FontData::from_owned(f.bytes);
        data.index = f.index;
        fonts.font_data.insert("user-font".to_owned(), data.into());
        if let Some(keys) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            keys.insert(0, "user-font".to_owned());
        }
    }
    // Full monochrome Noto Emoji (vendored) - egui's bundled emoji font is a subset that
    // misses most SMP emoji (😀 💰 ...), so append this to both families to fill the gap.
    // Monochrome (glyf outlines) so egui can rasterize it; color emoji still won't render.
    fonts.font_data.insert(
        "noto-emoji".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/NotoEmoji-Regular.ttf")).into(),
    );
    for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(keys) = fonts.families.get_mut(&fam) {
            keys.push("noto-emoji".to_owned());
        }
    }
    // Broad monochrome fallbacks (macOS) for arrows / box-drawing / powerline / misc symbols
    // the bundled fonts miss - appended as lowest priority so the primary fonts win. Loaded
    // best-effort; absent files (other OSes) are simply skipped.
    for (name, path) in [
        ("sys-unicode", "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"),
        ("sys-symbols", "/System/Library/Fonts/Apple Symbols.ttf"),
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(name.to_owned(), egui::FontData::from_owned(bytes).into());
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(keys) = fonts.families.get_mut(&fam) {
                    keys.push(name.to_owned());
                }
            }
        }
    }
    // The bold family: the bold face first, then the full Monospace stack behind it (regular
    // face + fallbacks), so a glyph the bold file misses degrades to regular, never tofu.
    // Registered only when a bold face resolved - a FontId naming an absent family panics.
    if let Some(f) = bold {
        let mut data = egui::FontData::from_owned(f.bytes);
        data.index = f.index;
        fonts.font_data.insert("user-font-bold".to_owned(), data.into());
        let mut keys = vec!["user-font-bold".to_owned()];
        keys.extend(
            fonts.families.get(&egui::FontFamily::Monospace).into_iter().flatten().cloned(),
        );
        fonts.families.insert(egui::FontFamily::Name(BOLD_FONT_FAMILY.into()), keys);
    }
    fonts
}

#[cfg(test)]
mod tests {
    use super::{
        BOLD_FONT_FAMILY, bold_face_name_score, build_fonts, face_name_score, installed_families,
        resolve_bold_font, resolve_font,
    };
    use eframe::egui;

    // Font resolution hits the real system source - gate on macOS where Menlo always exists.
    #[test]
    #[cfg(target_os = "macos")]
    fn resolve_font_finds_menlo_regular_face() {
        let f = resolve_font("Menlo").expect("Menlo ships with macOS");
        assert!(!f.bytes.is_empty());
        // The face must be the upright Regular, not Italic/Bold (select_best_match regression:
        // core-text matching handed back Menlo-Italic). Checked by name - core-text loads
        // report broken `properties()` (see face_name_score).
        let font = font_kit::font::Font::from_bytes(std::sync::Arc::new(f.bytes), f.index)
            .expect("resolved bytes load");
        assert_eq!(font.full_name(), "Menlo Regular");
    }

    #[test]
    fn face_name_score_prefers_upright_regular() {
        assert!(face_name_score("Menlo Regular") < face_name_score("Menlo Bold"));
        assert!(face_name_score("Menlo Bold") < face_name_score("Menlo Italic"));
        assert!(face_name_score("Menlo Italic") < face_name_score("Menlo Bold Italic"));
        assert_eq!(face_name_score("JetBrainsMono Nerd Font"), 0);
    }

    #[test]
    fn bold_face_score_requires_upright_bold() {
        // (face name) -> qualifies? The plain Bold must win; slants never qualify.
        assert_eq!(bold_face_name_score("Menlo Bold"), Some(0));
        assert_eq!(bold_face_name_score("Menlo Regular"), None); // not bold
        assert_eq!(bold_face_name_score("Menlo Italic"), None);
        assert_eq!(bold_face_name_score("Menlo Bold Italic"), None); // slant disqualifies
        assert_eq!(bold_face_name_score("Menlo Bold Oblique"), None);
        assert_eq!(bold_face_name_score("Fira Code Black"), None); // heavy != bold
        // Bold variants qualify but rank behind the plain Bold.
        let plain = bold_face_name_score("JetBrainsMono NF Bold").unwrap();
        for variant in
            ["JetBrainsMono NF SemiBold", "JetBrainsMono NF ExtraBold", "Iosevka Bold Condensed"]
        {
            let s = bold_face_name_score(variant).unwrap_or_else(|| panic!("{variant}"));
            assert!(s > plain, "{variant} must rank behind the plain Bold");
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn resolve_bold_font_finds_menlo_bold_face() {
        let f = resolve_bold_font("Menlo").expect("Menlo ships a Bold face");
        let font = font_kit::font::Font::from_bytes(std::sync::Arc::new(f.bytes), f.index)
            .expect("resolved bytes load");
        assert_eq!(font.full_name(), "Menlo Bold");
        assert!(resolve_bold_font("NoSuchFontXyz").is_none());
        assert!(resolve_bold_font("").is_none());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn resolve_font_rejects_unknown_and_empty() {
        assert!(resolve_font("NoSuchFontXyz").is_none());
        assert!(resolve_font("").is_none());
        assert!(resolve_font("   ").is_none());
    }

    #[test]
    fn build_fonts_without_user_font_keeps_fallbacks() {
        let fonts = build_fonts(None, None);
        assert!(!fonts.font_data.contains_key("user-font"));
        let mono = &fonts.families[&egui::FontFamily::Monospace];
        assert!(mono.contains(&"noto-emoji".to_owned()));
        let prop = &fonts.families[&egui::FontFamily::Proportional];
        assert_eq!(prop[1], "phosphor");
        // No bold face resolved -> the bold family must NOT exist (a FontId naming it panics).
        assert!(!fonts.families.contains_key(&egui::FontFamily::Name(BOLD_FONT_FAMILY.into())));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn build_fonts_puts_user_font_first_in_monospace_only() {
        let fonts = build_fonts(resolve_font("Menlo"), None);
        let mono = &fonts.families[&egui::FontFamily::Monospace];
        assert_eq!(mono[0], "user-font"); // top priority for the terminal grid
        assert!(mono.contains(&"noto-emoji".to_owned())); // fallbacks survive behind it
        let prop = &fonts.families[&egui::FontFamily::Proportional];
        assert!(!prop.contains(&"user-font".to_owned())); // chrome text untouched
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn build_fonts_registers_bold_family_with_fallbacks_behind() {
        let fonts = build_fonts(resolve_font("Menlo"), resolve_bold_font("Menlo"));
        let bold = &fonts.families[&egui::FontFamily::Name(BOLD_FONT_FAMILY.into())];
        assert_eq!(bold[0], "user-font-bold"); // the bold face leads
        assert!(bold.contains(&"user-font".to_owned())); // regular behind it
        assert!(bold.contains(&"noto-emoji".to_owned())); // fallbacks survive
        // The regular Monospace stack is untouched by the bold registration.
        assert_eq!(fonts.families[&egui::FontFamily::Monospace][0], "user-font");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn installed_families_lists_menlo_sorted() {
        let all = installed_families();
        assert!(all.iter().any(|n| n == "Menlo"));
        assert!(all.windows(2).all(|w| w[0] <= w[1]));
    }
}
