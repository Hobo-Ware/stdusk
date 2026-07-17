//! Shell integration: inject OSC 133 prompt/command marks so tabs get an exit-state dot without
//! the user editing their shell config. We source the user's real rc files first, then add hooks.
//!   - zsh:  set ZDOTDIR to our dir (which sources the real ZDOTDIR, then adds hooks)
//!   - bash: pass `--rcfile` to our file (which sources ~/.bashrc, then adds hooks)
//!   - other shells: left untouched (the dot just stays idle).
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

// zsh reads $ZDOTDIR/.zshenv for every shell; source the user's real one so PATH etc. survive.
const ZSHENV: &str = r#"[ -f "${STDUSK_REAL_ZDOTDIR:-$HOME}/.zshenv" ] && source "${STDUSK_REAL_ZDOTDIR:-$HOME}/.zshenv"
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
    std::fs::write(dir.join(".zshrc"), ZSHRC)?;
    std::fs::write(dir.join("bashrc"), BASHRC)?;
    Ok(())
}

/// Configure `cmd` (env + args) to load stdusk's OSC 133 hooks for `shell`. Best-effort: unknown
/// shells or a failed file write leave `cmd` untouched (integration silently off).
pub(crate) fn integrate(cmd: &mut CommandBuilder, shell: &str) {
    let kind = shell_kind(shell);
    if kind == ShellKind::Other {
        return;
    }
    let Some(dir) = dir() else { return };
    if write_files(&dir).is_err() {
        return;
    }
    match kind {
        ShellKind::Zsh => {
            let real =
                std::env::var("ZDOTDIR").or_else(|_| std::env::var("HOME")).unwrap_or_default();
            cmd.env("STDUSK_REAL_ZDOTDIR", real);
            cmd.env("ZDOTDIR", dir.to_string_lossy().to_string());
        }
        ShellKind::Bash => {
            cmd.arg("--rcfile");
            cmd.arg(dir.join("bashrc").to_string_lossy().to_string());
            cmd.arg("-i");
        }
        ShellKind::Other => {}
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
        // Every generated rc must actually emit the 133 marks the dot depends on.
        assert!(ZSHRC.contains("133;C") && ZSHRC.contains("133;D") && ZSHRC.contains("133;A"));
        assert!(BASHRC.contains("133;D") && BASHRC.contains("133;A"));
        // And must source the user's real config so their setup survives.
        assert!(ZSHRC.contains(".zshrc") && BASHRC.contains(".bashrc"));
    }
}
