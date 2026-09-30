//! Shell launch config: spawn a **login + interactive** shell (like Terminal.app / Tabby) so the
//! profile files that set PATH - `/etc/zprofile` (macOS `path_helper`), `~/.zprofile`
//! (`brew shellenv`, etc) - actually run; and, when shell integration is on, inject OSC 133
//! prompt/command marks so a failed command marks its tab, plus an OSC 7 cwd report so repo tabs
//! can follow the shell (zsh and bash only).
//!
//! Integration works by pointing the shell at our own startup files that first source the user's
//! real ones, then add hooks:
//! - zsh: set ZDOTDIR to our dir. zsh reads `$ZDOTDIR/{.zshenv,.zprofile,.zshrc,.zlogin}`, so we
//!   bridge ALL of them (bridging only .zshrc/.zshenv would drop .zprofile -> PATH breaks).
//! - bash: pass `--rcfile` our file (interactive, non-login) which sources the login profile chain
//!   (for PATH) then `~/.bashrc`, then adds hooks.
//! - other shells: launched login+interactive, no OSC 133 injection.
use std::path::{Path, PathBuf};

use portable_pty::CommandBuilder;

#[derive(Debug, PartialEq, Eq)]
enum ShellKind {
    Zsh,
    Bash,
    Other,
}

fn shell_kind(shell: &str) -> ShellKind {
    let name = shell.rsplit('/').next().unwrap_or(shell);
    if name.contains("zsh") {
        ShellKind::Zsh
    } else if name.contains("bash") {
        ShellKind::Bash
    } else {
        ShellKind::Other
    }
}

/// Where the generated rc files live, under `HOME`.
const BRIDGE_SUBDIR: &str = ".config/stdusk/shell";

fn dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| Path::new(&h).join(BRIDGE_SUBDIR))
}

// zsh reads $ZDOTDIR/{.zshenv,.zprofile,.zshrc,.zlogin}; bridge each to the user's real file so
// their PATH (.zprofile), env (.zshenv), and interactive config (.zshrc) all survive our redirect.
// A real dir that is itself a stdusk bridge (`*/.config/stdusk/shell`, with or without a trailing slash) is never sourced: its files
// would source the real dir again, forever. That state comes from a stdusk started inside a pane
// of another stdusk, and the shell then starts with no user config instead of looping.
macro_rules! bridge {
    ($file:literal) => {
        concat!(
            "case \"${STDUSK_REAL_ZDOTDIR:-$HOME}\" in\n",
            "  */.config/stdusk/shell|*/.config/stdusk/shell/) ;;\n",
            "  *) [ -f \"${STDUSK_REAL_ZDOTDIR:-$HOME}/",
            $file,
            "\" ] && source \"${STDUSK_REAL_ZDOTDIR:-$HOME}/",
            $file,
            "\" ;;\n",
            "esac\n"
        )
    };
}

const ZSHENV: &str = bridge!(".zshenv");

const ZPROFILE: &str = bridge!(".zprofile");

const ZLOGIN: &str = bridge!(".zlogin");

// macOS zsh emits OSC 7 only for Apple's own Terminal (`TERM_PROGRAM == Apple_Terminal`), so a
// stdusk shell needs this hook to report its cwd.
const ZSHRC: &str = concat!(
    "# stdusk shell integration (OSC 133 marks + OSC 7 cwd) - regenerated on launch, do not edit.\n",
    bridge!(".zshrc"),
    r#"# `D` (exit) is only emitted after a real command ran, so the first/empty prompt stays idle.
_stdusk_preexec() { typeset -g _stdusk_ran=1; print -n '\e]133;C\a' }
# Report the cwd, percent-encoded byte by byte (LC_CTYPE=C), keeping only [/._~A-Za-z0-9-].
_stdusk_osc7() {
  local i ch hex out= LC_CTYPE=C LC_COLLATE=C LC_ALL= LANG=
  for (( i = 1; i <= ${#PWD}; ++i )); do
    ch=$PWD[i]
    case $ch in
      [/._~A-Za-z0-9-]) out+=$ch ;;
      *) printf -v hex '%02X' "'$ch"; out+=%${hex: -2} ;;
    esac
  done
  print -n "\e]7;file://localhost$out\a"
}
_stdusk_precmd()  { local ec=$?; [[ -n ${_stdusk_ran-} ]] && print -n "\e]133;D;${ec}\a"; unset _stdusk_ran; print -n '\e]133;A\a'; _stdusk_osc7 }
autoload -Uz add-zsh-hook 2>/dev/null
add-zsh-hook preexec _stdusk_preexec 2>/dev/null
add-zsh-hook precmd  _stdusk_precmd  2>/dev/null
"#
);

/// The bash hook text alone, so `BASHRC` and the tests use the very same script.
macro_rules! bash_hook {
    () => {
        r#"# Report the cwd over OSC 7, percent-encoded byte by byte (LC_ALL=C), keeping only [/._~A-Za-z0-9-].
__stdusk_osc7() {
  local i ch hex out= LC_ALL=C
  for (( i = 0; i < ${#PWD}; i++ )); do
    ch=${PWD:i:1}
    case $ch in
      [/._~A-Za-z0-9-]) out+=$ch ;;
      *) printf -v hex '%02X' "'$ch"; out+=%${hex: -2} ;;
    esac
  done
  printf '\033]7;file://localhost%s\007' "$out"
}
# Skip the exit mark on the very first prompt so a freshly-opened tab stays idle.
__stdusk_prompt() { local ec=$?; [ -n "${__stdusk_started-}" ] && printf '\033]133;D;%d\007' "$ec"; __stdusk_started=1; printf '\033]133;A\007'; __stdusk_osc7; }
case "$PROMPT_COMMAND" in
  *__stdusk_prompt*) ;;
  *) PROMPT_COMMAND="__stdusk_prompt${PROMPT_COMMAND:+; $PROMPT_COMMAND}" ;;
esac
"#
    };
}

#[cfg(test)]
const BASH_HOOK: &str = bash_hook!();

const BASHRC: &str = concat!(
    r#"# stdusk shell integration (OSC 133 marks + OSC 7 cwd) - regenerated on launch, do not edit.
# We run bash interactive-but-not-login (--rcfile), which skips the profile files that set PATH
# (Homebrew, etc). Source the login profile chain first so tools like starship are found.
if [ -f "$HOME/.bash_profile" ]; then source "$HOME/.bash_profile"
elif [ -f "$HOME/.profile" ]; then source "$HOME/.profile"; fi
[ -f "$HOME/.bashrc" ] && source "$HOME/.bashrc"
"#,
    bash_hook!()
);

// Vendored fish-style history autosuggestions (zsh-users/zsh-autosuggestions v0.7.1, MIT - the
// license header is kept inside the file). Opt-in; sourced from our .zshrc only when the config
// flag is on. Right-arrow / End accept the suggestion (its default ACCEPT_WIDGETS).
const ZSH_AUTOSUGGEST: &str = include_str!("assets/zsh-autosuggestions.zsh");

/// The line appended to our `.zshrc` that loads the vendored plugin. Guarded so a user who
/// already sources their own copy (oh-my-zsh, brew) doesn't double-load it.
fn autosuggest_source_line(plugin: &Path) -> String {
    format!(
        "\n# stdusk: fish-style history autosuggestions ([terminal] autosuggestions = true).\n\
         (( ${{+functions[_zsh_autosuggest_start]}} )) || source {:?}\n",
        plugin.to_string_lossy()
    )
}

/// Whether `dir` is a stdusk-generated bridge dir, by its path (`<HOME>/.config/stdusk/shell`).
fn is_bridge_dir(dir: &str) -> bool {
    Path::new(dir).ends_with(BRIDGE_SUBDIR)
}

/// The user's REAL zsh dotfile dir, which our generated bridges source. An inherited `ZDOTDIR`
/// wins over `$HOME`, except when it is a stdusk bridge dir. That happens whenever stdusk starts
/// inside a stdusk shell, ours or another instance's (a `--state-dir` run has its own `HOME`).
/// Bridging to a bridge makes the rc files source each other in a loop. Then the real dir is the
/// one the outer stdusk recorded in `STDUSK_REAL_ZDOTDIR`, or `$HOME` when that is not usable.
/// Pure so the nested cases are testable without mutating the environment.
fn real_zdotdir(inherited: &str, inherited_real: &str, home: &str) -> String {
    if inherited.is_empty() {
        return home.to_owned();
    }
    if !is_bridge_dir(inherited) {
        return inherited.to_owned();
    }
    if !inherited_real.is_empty() && !is_bridge_dir(inherited_real) {
        return inherited_real.to_owned();
    }
    home.to_owned()
}

/// Write `content` to `path` only when it differs, through a temp file and a rename. Every pane
/// spawn calls this, and a restore spawns many panes at once: a shell must never source a file
/// that another spawn is halfway through writing.
fn write_if_changed(path: &Path, content: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Write the generated rc files.
pub(crate) fn write_files(dir: &Path, autosuggest: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    write_if_changed(&dir.join(".zshenv"), ZSHENV)?;
    write_if_changed(&dir.join(".zprofile"), ZPROFILE)?;
    write_if_changed(&dir.join(".zlogin"), ZLOGIN)?;
    let mut zshrc = ZSHRC.to_owned();
    if autosuggest {
        let plugin = dir.join("zsh-autosuggestions.zsh");
        write_if_changed(&plugin, ZSH_AUTOSUGGEST)?;
        zshrc.push_str(&autosuggest_source_line(&plugin));
    }
    write_if_changed(&dir.join(".zshrc"), &zshrc)?;
    write_if_changed(&dir.join("bashrc"), BASHRC)?;
    Ok(())
}

/// Configure `cmd` (args + env) to spawn a login+interactive shell, optionally wiring OSC 133
/// integration. Always spawns login+interactive so PATH-setting profile files run (the reason
/// `starship` etc. resolve like they do in Terminal.app). Integration is best-effort: unknown
/// shells or a failed file write just skip the OSC 133 hooks. `autosuggest` (zsh-only, and only
/// when `integration` is on since it reuses the ZDOTDIR redirect) sources the vendored
/// fish-style history-suggestion plugin from our generated `.zshrc`.
pub(crate) fn configure(
    cmd: &mut CommandBuilder,
    shell: &str,
    integration: bool,
    autosuggest: bool,
) {
    match shell_kind(shell) {
        ShellKind::Zsh => {
            if integration
                && let Some(dir) = dir()
                && write_files(&dir, autosuggest).is_ok()
            {
                let inherited = std::env::var("ZDOTDIR").unwrap_or_default();
                let home = std::env::var("HOME").unwrap_or_default();
                cmd.env(
                    "STDUSK_REAL_ZDOTDIR",
                    real_zdotdir(
                        &inherited,
                        &std::env::var("STDUSK_REAL_ZDOTDIR").unwrap_or_default(),
                        &home,
                    ),
                );
                cmd.env("ZDOTDIR", dir.to_string_lossy().to_string());
            }
            // ZDOTDIR (if set) redirects the rc files; -l/-i still make zsh read the *profile*
            // chain ($ZDOTDIR/.zprofile -> bridged) so PATH is set.
            cmd.arg("-l");
            cmd.arg("-i");
        }
        ShellKind::Bash => {
            let mut rc_injected = false;
            if integration
                && let Some(dir) = dir()
                && write_files(&dir, false).is_ok()
            {
                cmd.arg("--rcfile");
                cmd.arg(dir.join("bashrc").to_string_lossy().to_string());
                rc_injected = true;
            }
            if rc_injected {
                // --rcfile only applies to an interactive, non-login shell; our bashrc sources the
                // profile chain itself for PATH.
                cmd.arg("-i");
            } else {
                cmd.arg("-l");
                cmd.arg("-i");
            }
        }
        ShellKind::Other => {
            // No OSC 133 injection, but still login+interactive for PATH (best-effort).
            cmd.arg("-l");
            cmd.arg("-i");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_kind_detection() {
        assert_eq!(shell_kind("/bin/zsh"), ShellKind::Zsh);
        assert_eq!(shell_kind("zsh"), ShellKind::Zsh);
        assert_eq!(shell_kind("/usr/local/bin/bash"), ShellKind::Bash);
        assert_eq!(shell_kind("/usr/bin/fish"), ShellKind::Other);
        assert_eq!(shell_kind("/bin/sh"), ShellKind::Other);
    }

    #[test]
    fn hook_scripts_emit_osc_133() {
        // Every generated rc must actually emit the 133 marks the tab indicator depends on.
        assert!(ZSHRC.contains("133;C") && ZSHRC.contains("133;D") && ZSHRC.contains("133;A"));
        assert!(BASHRC.contains("133;D") && BASHRC.contains("133;A"));
    }

    #[test]
    fn hook_scripts_report_the_cwd_over_osc_7() {
        // Repo grouping follows the cwd, and macOS zsh only reports it for Apple's own Terminal.
        for rc in [ZSHRC, BASHRC] {
            assert!(rc.contains("]7;file://localhost"), "the hook must emit OSC 7");
            assert!(rc.contains("%02X"), "the path must be percent-encoded, never printed raw");
        }
    }

    /// Awkward but legal: a space, UTF-8, `;`, `%41`, `\`. It must come back from a shell intact.
    const FUSSY_DIR: &str = "a b \u{e9};x%41\\ z";

    /// Named like an attack: a BEL, an ESC, and a whole OSC 52 clipboard write. Printed raw, the
    /// BEL would end the OSC 7 early and the rest would run as a sequence of its own.
    const INJECTION_DIR: &str = "x\u{7}\u{1b}]52;c;Zm9v\u{7}y";

    /// Spawn a REAL login+interactive `shell` in `start`, with `HOME` and the bridge in scratch dirs
    /// (no user rc, and the real `~/.config/stdusk/shell` is never written), and wait for its first
    /// prompt to report a cwd over OSC 7. `None` if none arrives in 10 s.
    fn first_prompt_cwd(
        shell: &str,
        env: std::collections::BTreeMap<String, String>,
        start: &Path,
    ) -> Option<String> {
        use crate::config::Profile;
        use crate::terminal::{PtyTerm, SpawnOpts};
        let opts = SpawnOpts {
            detect_progress: false,
            shell_integration: false, // the test wires the bridge itself; see `env`
            autosuggestions: false,
            scrollback_lines: 100,
            word_separators: " ".into(),
            bold_bright: false,
            cwd: Some(start.to_string_lossy().into_owned()),
            profile: Some(Profile {
                name: "osc7".into(),
                shell: Some(shell.into()),
                args: vec![],
                cwd: None,
                env,
                color: None,
            }),
        };
        let term = PtyTerm::spawn(80, 24, eframe::egui::Context::default(), &opts);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut cwd = None;
        while cwd.is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(25));
            cwd = term.cwd();
        }
        cwd
    }

    /// Run `script` in a plain (no pty) `shell` with a clean env and return its stdout. `$1` and
    /// `$2` are `hook` and `dir`. Byte-exact output is what proves nothing was injected.
    fn run_hook(shell: &str, script: &str, hook: &Path, home: &Path, dir: &Path) -> Vec<u8> {
        let out = std::process::Command::new(shell)
            .args(["-c", script, "hook-test"])
            .args([hook, dir])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("STDUSK_REAL_ZDOTDIR", home)
            .output()
            .expect("the shell under test must launch");
        out.stdout
    }

    fn count_byte(bytes: &[u8], byte: u8) -> usize {
        bytes.split(|&b| b == byte).count() - 1
    }

    /// What the hook must print for `dir`: one OSC 7, the whole path percent-encoded.
    fn expected_osc7(dir: &Path) -> Vec<u8> {
        let path = std::fs::canonicalize(dir).unwrap();
        format!("\u{1b}]7;file://localhost{}\u{7}", crate::osc::hook_encode(path.to_str().unwrap()))
            .into_bytes()
    }

    fn scratch(kind: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("stdusk-osc7-{}-{kind}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn real_zsh_reports_a_fussy_directory_name_intact() {
        let base = scratch("zsh");
        let (bridge, home, start) = (base.join("bridge"), base.join("home"), base.join(FUSSY_DIR));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&start).unwrap();
        write_files(&bridge, false).unwrap();
        let env = [
            ("ZDOTDIR", bridge.to_string_lossy().into_owned()),
            ("STDUSK_REAL_ZDOTDIR", home.to_string_lossy().into_owned()),
            ("HOME", home.to_string_lossy().into_owned()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();

        let cwd = first_prompt_cwd("/bin/zsh", env, &start);

        let want = std::fs::canonicalize(&start).unwrap();
        assert_eq!(cwd.as_deref(), want.to_str(), "zsh must report the exact directory");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn real_bash_reports_a_fussy_directory_name_intact() {
        let base = scratch("bash");
        let (home, start) = (base.join("home"), base.join(FUSSY_DIR));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&start).unwrap();
        // The bridge's bashrc also sources ~/.bash_profile, so a login shell here gets the hook
        // text alone, via a profile that sources it.
        let hook = base.join("hook.bash");
        std::fs::write(&hook, BASH_HOOK).unwrap();
        std::fs::write(home.join(".bash_profile"), format!("source {hook:?}\n")).unwrap();
        let env = [("HOME".to_string(), home.to_string_lossy().into_owned())].into_iter().collect();

        let cwd = first_prompt_cwd("/bin/bash", env, &start);

        let want = std::fs::canonicalize(&start).unwrap();
        assert_eq!(cwd.as_deref(), want.to_str(), "bash must report the exact directory");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_directory_name_cannot_inject_a_sequence_through_the_zsh_hook() {
        let base = scratch("zsh-inject");
        let (bridge, home, dir) =
            (base.join("bridge"), base.join("home"), base.join(INJECTION_DIR));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        write_files(&bridge, false).unwrap();

        let out = run_hook(
            "/bin/zsh",
            r#"source "$1"; cd -- "$2"; _stdusk_osc7"#,
            &bridge.join(".zshrc"),
            &home,
            &std::fs::canonicalize(&dir).unwrap(),
        );

        assert_eq!(out, expected_osc7(&dir), "one OSC 7, the whole path encoded");
        assert_eq!(count_byte(&out, 0x1b), 1, "exactly one ESC");
        assert_eq!(count_byte(&out, 0x07), 1, "exactly one BEL");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_directory_name_cannot_inject_a_sequence_through_the_bash_hook() {
        let base = scratch("bash-inject");
        let (home, dir) = (base.join("home"), base.join(INJECTION_DIR));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let hook = base.join("hook.bash");
        std::fs::write(&hook, BASH_HOOK).unwrap();

        let out = run_hook(
            "/bin/bash",
            r#"source "$1"; cd -- "$2"; __stdusk_osc7"#,
            &hook,
            &home,
            &std::fs::canonicalize(&dir).unwrap(),
        );

        assert_eq!(out, expected_osc7(&dir), "one OSC 7, the whole path encoded");
        assert_eq!(count_byte(&out, 0x1b), 1, "exactly one ESC");
        assert_eq!(count_byte(&out, 0x07), 1, "exactly one BEL");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_control_character_path_is_never_taken_as_the_cwd() {
        // Hook output for the injection directory is well-formed OSC 7, but the parser must still
        // refuse the decoded path: it holds a BEL and an ESC, so it is not somewhere a shell is.
        let path = format!("/tmp/{INJECTION_DIR}");
        let osc = format!("\u{1b}]7;file://localhost{}\u{7}", crate::osc::hook_encode(&path));
        assert_eq!(crate::osc::OscScanner::new().feed(osc.as_bytes()), vec![]);
    }

    #[test]
    fn bridges_source_the_users_real_startup_files() {
        // The whole PATH fix: our zsh files source the real .zprofile (PATH) + friends, and bash
        // sources the login profile chain. Missing any of these reintroduces the starship bug.
        assert!(ZSHENV.contains(".zshenv"));
        assert!(ZPROFILE.contains(".zprofile"));
        assert!(ZLOGIN.contains(".zlogin"));
        assert!(ZSHRC.contains(".zshrc"));
        assert!(BASHRC.contains(".bash_profile") && BASHRC.contains(".profile"));
        assert!(BASHRC.contains(".bashrc"));
    }

    #[test]
    fn real_zdotdir_never_bridges_to_a_stdusk_bridge() {
        const ME: &str = "/Users/me";
        const DOTS: &str = "/Users/me/dotfiles/zsh";
        const OURS: &str = "/Users/me/.config/stdusk/shell";
        const OTHER: &str = "/tmp/state/.config/stdusk/shell";
        // (inherited ZDOTDIR, inherited STDUSK_REAL_ZDOTDIR, HOME, want, why)
        let cases = [
            ("", "", ME, ME, "unset: HOME"),
            ("", DOTS, ME, ME, "unset ZDOTDIR wins over a stale real dir"),
            (DOTS, "", ME, DOTS, "a genuine user ZDOTDIR is honored"),
            (DOTS, OTHER, ME, DOTS, "a user ZDOTDIR is not a bridge, whatever real says"),
            (OURS, "", ME, ME, "nested in our own bridge, no record: HOME"),
            (OURS, ME, ME, ME, "nested in our own bridge: the outer real dir"),
            (OURS, DOTS, ME, DOTS, "nested in our own bridge: the user's custom ZDOTDIR"),
            (
                OTHER,
                DOTS,
                "/tmp/b",
                DOTS,
                "another instance's bridge: its real dir, not its bridge",
            ),
            (OTHER, OTHER, "/tmp/b", "/tmp/b", "a record that names a bridge is not usable"),
            (OTHER, "", "/tmp/b", "/tmp/b", "another instance's bridge, no record: HOME"),
        ];
        for (inherited, real, home, want, why) in cases {
            assert_eq!(real_zdotdir(inherited, real, home), want, "{why}");
        }
    }

    #[test]
    fn a_nested_stdusk_shell_sources_the_users_real_files_once() {
        // Instance A (HOME = home_a) runs a pane. Instance B has another HOME and starts inside it,
        // so it inherits A's ZDOTDIR and STDUSK_REAL_ZDOTDIR. The real startup file must run, once.
        let base = scratch("zsh-nested-real");
        let (user, home_a, home_b) = (base.join("user"), base.join("a"), base.join("b"));
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join(".zshenv"), "print real-zshenv-ran\n").unwrap();
        let (a, b) = (bridge_in(&home_a), bridge_in(&home_b));
        let real =
            real_zdotdir(a.to_str().unwrap(), user.to_str().unwrap(), home_b.to_str().unwrap());
        assert_eq!(real, user.to_str().unwrap());
        let run = run_zsh_startup(&[
            ("HOME", &home_b),
            ("ZDOTDIR", &b),
            ("STDUSK_REAL_ZDOTDIR", Path::new(&real)),
        ]);
        let (ok, out, err) = run.expect("zsh looped and hit the timeout");
        assert!(ok && err.trim().is_empty(), "ok={ok} err={err:?}");
        assert_eq!(out.matches("real-zshenv-ran").count(), 1, "{out:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Run `zsh -l -i -c 'print done'` with a clean env, under a hard timeout. A rc loop must not
    /// hang the test. Returns the exit success flag, stdout and stderr.
    fn run_zsh_startup(env: &[(&str, &Path)]) -> Option<(bool, String, String)> {
        use std::io::Read as _;
        let mut child = std::process::Command::new("/bin/zsh")
            .args(["-l", "-i", "-c", "print done"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .envs(env.iter().map(|&(k, v)| (k, v)))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("zsh must launch");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let (mut out, mut err) = (String::new(), String::new());
        child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
        child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
        Some((status.success(), out, err))
    }

    /// The bridge dir of a stdusk instance whose `HOME` is `home`.
    fn bridge_in(home: &Path) -> PathBuf {
        let bridge = home.join(BRIDGE_SUBDIR);
        write_files(&bridge, false).unwrap();
        bridge
    }

    #[test]
    fn zsh_starts_when_the_real_dir_is_a_bridge_with_a_trailing_slash() {
        let base = scratch("zsh-nested-slash");
        let (home_a, home_b) = (base.join("a"), base.join("b"));
        let (a, b) = (bridge_in(&home_a), bridge_in(&home_b));
        let slashed = PathBuf::from(format!("{}/", a.display()));
        let run = run_zsh_startup(&[
            ("HOME", &home_b),
            ("ZDOTDIR", &b),
            ("STDUSK_REAL_ZDOTDIR", &slashed),
        ]);
        let (ok, out, err) = run.expect("zsh looped and hit the timeout");
        assert!(ok && out.contains("done"), "ok={ok} out={out:?} err={err:?}");
        assert!(err.trim().is_empty(), "a loop prints an error: {err:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn zsh_starts_when_the_real_dir_is_another_stdusks_bridge() {
        // The state a nested launch left behind before the fix: the real dir names a bridge, and
        // that bridge's files source the real dir again. The bridge must refuse, not loop.
        let base = scratch("zsh-nested-loop");
        let (home_a, home_b) = (base.join("a"), base.join("b"));
        let (a, b) = (bridge_in(&home_a), bridge_in(&home_b));
        let run =
            run_zsh_startup(&[("HOME", &home_b), ("ZDOTDIR", &b), ("STDUSK_REAL_ZDOTDIR", &a)]);
        let (ok, out, err) = run.expect("zsh looped and hit the timeout");
        assert!(ok && out.contains("done"), "ok={ok} out={out:?} err={err:?}");
        assert!(err.trim().is_empty(), "a loop prints an error: {err:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn vendored_autosuggestions_is_the_real_plugin() {
        // include_str! actually pulled the plugin in, and the guard references its start function.
        assert!(ZSH_AUTOSUGGEST.contains("_zsh_autosuggest_start"));
        assert!(ZSH_AUTOSUGGEST.contains("forward-char")); // Right-arrow accepts the suggestion
        assert!(autosuggest_source_line(Path::new("/x/p.zsh")).contains("_zsh_autosuggest_start"));
    }

    #[test]
    fn write_files_sources_plugin_only_when_autosuggest_on() {
        let base = std::env::temp_dir().join(format!("stdusk-shtest-{}", std::process::id()));
        let on = base.join("on");
        let off = base.join("off");

        write_files(&on, true).unwrap();
        let zshrc_on = std::fs::read_to_string(on.join(".zshrc")).unwrap();
        assert!(zshrc_on.contains("zsh-autosuggestions.zsh"));
        assert!(on.join("zsh-autosuggestions.zsh").exists());

        write_files(&off, false).unwrap();
        let zshrc_off = std::fs::read_to_string(off.join(".zshrc")).unwrap();
        assert!(!zshrc_off.contains("zsh-autosuggestions.zsh"));
        assert!(!off.join("zsh-autosuggestions.zsh").exists());
        // OSC 133 marks survive in both.
        assert!(zshrc_on.contains("133;A") && zshrc_off.contains("133;A"));

        let _ = std::fs::remove_dir_all(&base);
    }
    #[test]
    fn shared_files_are_rewritten_only_when_the_content_changes() {
        use std::os::unix::fs::MetadataExt as _;
        let base = scratch("write-if-changed");
        let file = base.join("rc");
        write_if_changed(&file, "one\n").unwrap();
        let first = std::fs::metadata(&file).unwrap().ino();
        write_if_changed(&file, "one\n").unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().ino(), first, "same content, same file");
        write_if_changed(&file, "two\n").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "two\n");
        // The swap is a rename, so no temp file stays behind.
        let names: Vec<_> =
            std::fs::read_dir(&base).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
