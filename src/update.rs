//! Update plumbing: pending-update detection (bundle on disk differs from the running version),
//! the GitHub release check, and the Homebrew upgrade that installs it.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, bail};

/// Version the running binary was built as.
pub(crate) const RUNNING: &str = env!("CARGO_PKG_VERSION");

/// `CFBundleShortVersionString` from an XML plist. The generated bundle plist is plain XML (checked
/// against a real release), so a scan beats pulling in a plist parser. Returns `None` on a binary
/// plist or a missing/!malformed key rather than guessing.
pub(crate) fn plist_short_version(xml: &str) -> Option<String> {
    let after_key = xml.split_once("<key>CFBundleShortVersionString</key>")?.1;
    let open = after_key.find("<string>")? + "<string>".len();
    let rest = &after_key[open..];
    let close = rest.find("</string>")?;
    let v = rest[..close].trim();
    (!v.is_empty()).then(|| v.to_owned())
}

/// `Contents/Info.plist` for the bundle containing `exe`
/// (`Foo.app/Contents/MacOS/foo` -> `Foo.app/Contents/Info.plist`), or `None` when the binary
/// isn't inside a bundle at all (`cargo run`, a bare `target/release/stdusk`).
pub(crate) fn bundle_plist_path(exe: &Path) -> Option<PathBuf> {
    let contents = exe.parent().filter(|p| p.file_name() == Some("MacOS".as_ref()))?.parent()?;
    contents.parent()?.extension().filter(|e| *e == "app")?;
    Some(contents.join("Info.plist"))
}

/// The `.app` directory containing `exe`, or `None` outside a bundle. What `open` needs to
/// relaunch us, and the reason a dev build silently declines to offer a restart-to-update.
pub(crate) fn bundle_path(exe: &Path) -> Option<PathBuf> {
    let app = exe.parent()?.parent()?.parent()?;
    app.extension().filter(|e| *e == "app").map(|_| app.to_path_buf())
}

/// Version installed on disk, distinct from [`RUNNING`] when the bundle was replaced under us.
fn installed_version(exe: &Path) -> Option<String> {
    let plist = bundle_plist_path(exe)?;
    plist_short_version(&std::fs::read_to_string(plist).ok()?)
}

/// The version a restart would pick up, or `None` when a restart would change nothing.
/// Any difference counts (not just "newer") so a rollback is offered too - the honest claim is
/// "disk differs from running", never "an update exists".
pub(crate) fn pending(exe: &Path) -> Option<String> {
    installed_version(exe).filter(|v| v != RUNNING)
}

/// [`pending`] for the running process; `None` outside a bundle or on a read failure.
pub(crate) fn pending_for_running_exe() -> Option<String> {
    pending(&std::env::current_exe().ok()?)
}

const REPO: &str = "Hobo-Ware/stdusk";
const CASK: &str = "hobo-ware/tap/stdusk";
const TAG_PREFIX: &str = "stdusk-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Release {
    pub(crate) version: String,
    pub(crate) url: String,
}

fn parse_version(text: &str) -> Option<Vec<u64>> {
    let core = text.trim().trim_start_matches('v').split(['-', '+']).next()?;
    core.split('.').map(|part| part.parse().ok()).collect()
}

pub(crate) fn is_newer(latest: &str, current: &str) -> bool {
    let (Some(a), Some(b)) = (parse_version(latest), parse_version(current)) else {
        return false;
    };
    let len = a.len().max(b.len());
    let pad = |v: &[u64]| (0..len).map(|i| v.get(i).copied().unwrap_or(0)).collect::<Vec<_>>();
    pad(&a) > pad(&b)
}

fn version_from_release_url(url: &str) -> Option<String> {
    let tag = url.rsplit_once("/releases/tag/")?.1.split(['?', '#']).next()?;
    let version = tag.strip_prefix(TAG_PREFIX).unwrap_or(tag).trim_start_matches('v');
    parse_version(version).is_some().then(|| version.to_owned())
}

/// The newest release, read from the redirect on `/releases/latest`: no API token, no rate limit.
pub(crate) fn latest_release() -> anyhow::Result<Release> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .user_agent("stdusk")
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let response = agent.get(&format!("https://github.com/{REPO}/releases/latest")).call()?;
    let location =
        response.headers().get("location").and_then(|l| l.to_str().ok()).with_context(|| {
            format!("GitHub answered {} without a release link", response.status())
        })?;
    let version = version_from_release_url(location).context("the latest release has no tag")?;
    Ok(Release { version, url: location.to_owned() })
}

fn brew() -> Option<PathBuf> {
    ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

pub(crate) fn installed_with_brew() -> bool {
    ["/opt/homebrew/Caskroom/stdusk", "/usr/local/Caskroom/stdusk"]
        .iter()
        .any(|p| Path::new(p).exists())
}

pub(crate) fn brew_upgrade() -> anyhow::Result<()> {
    let brew = brew().context("Homebrew is not installed")?;
    let tap = Command::new(&brew).args(["--repository", "hobo-ware/tap"]).output()?;
    let tap_dir = String::from_utf8_lossy(&tap.stdout).trim().to_owned();
    if tap.status.success() && Path::new(&tap_dir).exists() {
        let _ = Command::new("git").args(["-C", &tap_dir, "pull", "--quiet", "--ff-only"]).output();
    }
    let out = Command::new(&brew)
        .args(["upgrade", "--cask", CASK])
        .env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .env("HOMEBREW_NO_INSTALL_CLEANUP", "1")
        .output()?;
    if !out.status.success() {
        bail!("brew upgrade failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        let cases = [
            ("1.10.0", "1.9.3", true),
            ("v2.0.0", "1.99.99", true),
            ("1.9", "1.8.9", true),
            ("1.8.0", "1.8.0", false),
            ("1.7.5", "1.8.0", false),
            ("1.8.0-beta.1", "1.8.0", false),
            ("garbage", "1.8.0", false),
        ];
        for (latest, current, want) in cases {
            assert_eq!(is_newer(latest, current), want, "{latest} vs {current}");
        }
    }

    #[test]
    fn the_release_redirect_names_the_version() {
        let cases = [
            ("https://github.com/Hobo-Ware/stdusk/releases/tag/stdusk-v1.8.0", Some("1.8.0")),
            ("https://github.com/Hobo-Ware/stdusk/releases/tag/v1.8.1?x=1", Some("1.8.1")),
            ("https://github.com/Hobo-Ware/stdusk/releases/tag/nightly", None),
            ("https://github.com/Hobo-Ware/stdusk/releases", None),
            ("https://github.com/login", None),
        ];
        for (url, want) in cases {
            assert_eq!(version_from_release_url(url).as_deref(), want, "url {url}");
        }
    }

    #[test]
    fn reads_the_short_version_from_a_real_bundle_plist() {
        // Shape of the plist our release workflow generates.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>stdusk</string>
    <key>CFBundleShortVersionString</key>
    <string>1.4.9</string>
    <key>CFBundleVersion</key>
    <string>1.4.9</string>
</dict>
</plist>"#;
        assert_eq!(plist_short_version(xml).as_deref(), Some("1.4.9"));
    }

    #[test]
    fn malformed_plists_yield_none_never_a_bogus_version() {
        let cases = [
            "",                                                         // empty
            "<plist><dict></dict></plist>",                             // key absent
            "<key>CFBundleShortVersionString</key>",                    // key, no value
            "<key>CFBundleShortVersionString</key><string>",            // unterminated
            "<key>CFBundleShortVersionString</key><string>  </string>", // blank value
            "bplist00\u{0}\u{1}CFBundleShortVersionString",             // binary plist
        ];
        for xml in cases {
            assert_eq!(plist_short_version(xml), None, "input {xml:?}");
        }
        // CFBundleVersion must not be mistaken for the SHORT version.
        let only_long = "<key>CFBundleVersion</key><string>9.9.9</string>";
        assert_eq!(plist_short_version(only_long), None);
    }

    #[test]
    fn plist_path_resolves_only_inside_an_app_bundle() {
        assert_eq!(
            bundle_plist_path(Path::new("/Applications/stdusk.app/Contents/MacOS/stdusk")),
            Some(PathBuf::from("/Applications/stdusk.app/Contents/Info.plist"))
        );
        // Not a bundle: dev builds must never claim a pending update.
        for exe in [
            "/Users/me/stdusk/target/release/stdusk", // cargo build
            "/usr/local/bin/stdusk",                  // bare binary
            "/Applications/stdusk.app/stdusk",        // no Contents/MacOS
            "/tmp/notanapp/Contents/MacOS/stdusk",    // parent lacks .app
        ] {
            assert_eq!(bundle_plist_path(Path::new(exe)), None, "exe {exe}");
        }
    }

    #[test]
    fn bundle_path_is_the_app_dir_or_nothing() {
        assert_eq!(
            bundle_path(Path::new("/Applications/stdusk.app/Contents/MacOS/stdusk")),
            Some(PathBuf::from("/Applications/stdusk.app"))
        );
        for exe in ["/usr/local/bin/stdusk", "/Users/me/p/target/release/stdusk"] {
            assert_eq!(bundle_path(Path::new(exe)), None, "exe {exe}");
        }
    }

    #[test]
    fn pending_is_none_when_disk_matches_running() {
        let dir = std::env::temp_dir().join(format!("stdusk-upd-{}", std::process::id()));
        let macos = dir.join("stdusk.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let plist = dir.join("stdusk.app/Contents/Info.plist");
        let exe = macos.join("stdusk");
        let write = |v: &str| {
            let xml = format!("<key>CFBundleShortVersionString</key>\n    <string>{v}</string>");
            std::fs::write(&plist, xml).unwrap();
        };

        write(RUNNING);
        assert_eq!(pending(&exe), None, "same version = nothing to offer");

        write("99.0.0");
        assert_eq!(pending(&exe).as_deref(), Some("99.0.0"), "newer = offer it");

        write("0.0.1");
        assert_eq!(pending(&exe).as_deref(), Some("0.0.1"), "rollback also differs");

        std::fs::remove_file(&plist).unwrap();
        assert_eq!(pending(&exe), None, "unreadable plist must not claim an update");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
