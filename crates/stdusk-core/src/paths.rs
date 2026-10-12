//! Where stdusk keeps its files. One definition so the app, the theme loader and the session
//! store can never disagree about the config directory.
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// `~/.config/stdusk`, or `$XDG_CONFIG_HOME/stdusk` off macOS when that variable holds an
/// absolute path (the XDG rule). macOS ignores `XDG_CONFIG_HOME` so an existing install never
/// moves.
pub fn config_dir() -> Option<PathBuf> {
    config_dir_from(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
        cfg!(target_os = "macos"),
    )
}

fn config_dir_from(xdg: Option<&OsStr>, home: Option<&OsStr>, macos: bool) -> Option<PathBuf> {
    let xdg = xdg.map(Path::new).filter(|p| !macos && p.is_absolute());
    match xdg {
        Some(dir) => Some(dir.join("stdusk")),
        None => home.map(|h| Path::new(h).join(".config/stdusk")),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use super::config_dir_from;

    #[test]
    fn config_dir_follows_xdg_off_macos_only() {
        let (xdg, home) = (Some(OsStr::new("/x/cfg")), Some(OsStr::new("/home/u")));
        let under_home = Some(PathBuf::from("/home/u/.config/stdusk"));
        assert_eq!(config_dir_from(xdg, home, false), Some(PathBuf::from("/x/cfg/stdusk")));
        assert_eq!(config_dir_from(xdg, home, true), under_home);
        assert_eq!(config_dir_from(None, home, false), under_home);
        assert_eq!(
            config_dir_from(Some(OsStr::new("cfg")), home, false),
            under_home,
            "a relative XDG_CONFIG_HOME is ignored, as the spec says"
        );
        assert_eq!(config_dir_from(None, None, false), None);
    }
}
