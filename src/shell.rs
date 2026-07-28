//! Shell launch config: spawn a **login + interactive** shell (like Terminal.app / Tabby) so the
//! profile files that set PATH - `/etc/zprofile` (macOS `path_helper`), `~/.zprofile`
//! (`brew shellenv`, etc) - actually run; and, when shell integration is on, inject OSC 133
//! prompt/command marks so a failed command marks its tab.
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

fn dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| Path::new(&h).join(".config/stdusk/shell"))
}

// zsh reads $ZDOTDIR/{.zshenv,.zprofile,.zshrc,.zlogin}; bridge each to the user's real file so
// their PATH (.zprofile), env (.zshenv), and interactive config (.zshrc) all survive our redirect.
const ZSHENV: &str = r#"[ -f "${STDUSK_REAL_ZDOTDIR:-$HOME}/.zshenv" ] && source "${STDUSK_REAL_ZDOTDIR:-$HOME}/.zshenv"
"#;

const ZPROFILE: &str = r#"[ -f "${STDUSK_REAL_ZDOTDIR:-$HOME}/.zprofile" ] && source "${STDUSK_REAL_ZDOTDIR:-$HOME}/.zprofile"
"#;

const ZLOGIN: &str = r#"[ -f "${STDUSK_REAL_ZDOTDIR:-$HOME}/.zlogin" ] && source "${STDUSK_REAL_ZDOTDIR:-$HOME}/.zlogin"
"#;

const ZSHRC: &str = r#"# stdusk shell integration (OSC 133) - regenerated on launch, do not edit.
[ -f "${STDUSK_REAL_ZDOTDIR:-$HOME}/.zshrc" ] && source "${STDUSK_REAL_ZDOTDIR:-$HOME}/.zshrc"
# `D` (exit) is only emitted after a real command ran, so the first/empty prompt stays idle.
_stdusk_preexec() { typeset -g _stdusk_ran=1; print -n '\e]133;C\a' }
_stdusk_precmd()  { local ec=$?; [[ -n ${_stdusk_ran-} ]] && print -n "\e]133;D;${ec}\a"; unset _stdusk_ran; print -n '\e]133;A\a' }
autoload -Uz add-zsh-hook 2>/dev/null
add-zsh-hook preexec _stdusk_preexec 2>/dev/null
add-zsh-hook precmd  _stdusk_precmd  2>/dev/null
"#;

const BASHRC: &str = r#"# stdusk shell integration (OSC 133) - regenerated on launch, do not edit.
# We run bash interactive-but-not-login (--rcfile), which skips the profile files that set PATH
# (Homebrew, etc). Source the login profile chain first so tools like starship are found.
if [ -f "$HOME/.bash_profile" ]; then source "$HOME/.bash_profile"
elif [ -f "$HOME/.profile" ]; then source "$HOME/.profile"; fi
[ -f "$HOME/.bashrc" ] && source "$HOME/.bashrc"
# Skip the exit mark on the very first prompt so a freshly-opened tab stays idle.
__stdusk_prompt() { local ec=$?; [ -n "${__stdusk_started-}" ] && printf '\033]133;D;%d\007' "$ec"; __stdusk_started=1; printf '\033]133;A\007'; }
case "$PROMPT_COMMAND" in
  *__stdusk_prompt*) ;;
  *) PROMPT_COMMAND="__stdusk_prompt${PROMPT_COMMAND:+; $PROMPT_COMMAND}" ;;
esac
"#;

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

/// The user's REAL zsh dotfile dir, which our generated bridges source. Inherited `ZDOTDIR` wins
/// over `$HOME` - EXCEPT when it already points at our own generated dir, which happens whenever
/// stdusk is launched from inside a stdusk shell (the child inherits the ZDOTDIR we exported).
/// Bridging to ourselves makes every rc file source itself: zsh dies with "recursion limit
/// exceeded" and the pane comes up with no PATH, no prompt, no integration.
/// Pure so the nested case is testable without mutating the environment.
fn real_zdotdir(inherited: &str, home: &str, ours: &Path) -> String {
    if !inherited.is_empty() && Path::new(inherited) != ours {
        return inherited.to_owned();
    }
    home.to_owned()
}

fn write_files(dir: &Path, autosuggest: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(".zshenv"), ZSHENV)?;
    std::fs::write(dir.join(".zprofile"), ZPROFILE)?;
    std::fs::write(dir.join(".zlogin"), ZLOGIN)?;
    let mut zshrc = ZSHRC.to_string();
    if autosuggest {
        let plugin = dir.join("zsh-autosuggestions.zsh");
        std::fs::write(&plugin, ZSH_AUTOSUGGEST)?;
        zshrc.push_str(&autosuggest_source_line(&plugin));
    }
    std::fs::write(dir.join(".zshrc"), zshrc)?;
    std::fs::write(dir.join("bashrc"), BASHRC)?;
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
                cmd.env("STDUSK_REAL_ZDOTDIR", real_zdotdir(&inherited, &home, &dir));
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
    fn real_zdotdir_never_bridges_to_our_own_dir() {
        // Nested launch (stdusk opened from a stdusk shell): the inherited ZDOTDIR IS our
        // generated dir, so bridging to it would make each rc file source itself.
        let ours = Path::new("/tmp/stdusk-shell");
        // Self-referential (nested launch): fall back to HOME instead of bridging to ourselves.
        assert_eq!(real_zdotdir("/tmp/stdusk-shell", "/Users/me", ours), "/Users/me");
        // A genuine user ZDOTDIR is still honored.
        assert_eq!(
            real_zdotdir("/Users/me/dotfiles/zsh", "/Users/me", ours),
            "/Users/me/dotfiles/zsh"
        );
        // Unset: HOME.
        assert_eq!(real_zdotdir("", "/Users/me", ours), "/Users/me");
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
}
