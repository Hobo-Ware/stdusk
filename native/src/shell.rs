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

fn write_files(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(".zshenv"), ZSHENV)?;
    std::fs::write(dir.join(".zprofile"), ZPROFILE)?;
    std::fs::write(dir.join(".zlogin"), ZLOGIN)?;
    std::fs::write(dir.join(".zshrc"), ZSHRC)?;
    std::fs::write(dir.join("bashrc"), BASHRC)?;
    Ok(())
}

/// Configure `cmd` (args + env) to spawn a login+interactive shell, optionally wiring OSC 133
/// integration. Always spawns login+interactive so PATH-setting profile files run (the reason
/// `starship` etc. resolve like they do in Terminal.app). Integration is best-effort: unknown
/// shells or a failed file write just skip the OSC 133 hooks.
pub(crate) fn configure(cmd: &mut CommandBuilder, shell: &str, integration: bool) {
    match shell_kind(shell) {
        ShellKind::Zsh => {
            if integration
                && let Some(dir) = dir()
                && write_files(&dir).is_ok()
            {
                let real =
                    std::env::var("ZDOTDIR").or_else(|_| std::env::var("HOME")).unwrap_or_default();
                cmd.env("STDUSK_REAL_ZDOTDIR", real);
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
                && write_files(&dir).is_ok()
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
}
