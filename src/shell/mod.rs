//! Shell integration scripts.
//!
//! A child process cannot write to its parent shell's line editor, so the binary
//! alone can never put a command at the user's prompt. These functions are the
//! other half: the user sources them from their rc file, the zoxide/direnv
//! pattern.
//!
//! # The hooks run on every prompt
//!
//! That budget is the reason they use **shell builtins only** — no `utter record`
//! subprocess, no `date`, no command substitution. A fork on every prompt the user
//! types is a cost they would eventually notice and blame on their shell. The
//! sidecar's mtime serves as the timestamp, and `$$`/`$fish_pid` serves as the
//! session key, so nothing needs to be computed by an external program.

use std::path::Path;

use crate::cli::ShellKind;
use crate::session::ENV_SESSION_ID;

/// Absolute path to this binary, so the emitted function keeps working when
/// `utter` is not on PATH. Falls back to the bare name.
fn binary_path() -> String {
    std::env::current_exe()
        .ok()
        .filter(|p| p.is_absolute())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "utter".to_string())
}

/// Single-quotes a value for POSIX shells, escaping embedded single quotes.
fn sq(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Fish uses a different escape: a backslash works inside single quotes.
fn sq_fish(value: &str) -> String {
    format!("'{}'", value.replace('\\', r"\\").replace('\'', r"\'"))
}

pub fn init_script(shell: ShellKind, alias: &str, shell_dir: &Path) -> String {
    let bin = binary_path();
    let dir = shell_dir.display().to_string();
    match shell {
        ShellKind::Zsh => zsh(alias, &bin, &dir),
        ShellKind::Fish => fish(alias, &bin, &dir),
        ShellKind::Bash => bash(alias, &bin, &dir),
    }
}

/// `print -z` pushes onto zsh's editor buffer stack, so the command appears at the
/// next prompt with the cursor at the end — editable, and not run until Enter.
///
/// `$$` rather than `$PPID` or a random id: it is the interactive shell's own pid
/// and POSIX guarantees it is unchanged inside the `$(...)` subshell the function
/// below creates.
fn zsh(alias: &str, bin: &str, dir: &str) -> String {
    format!(
        r#"# utter shell integration (zsh)
export {env}=$$
[[ -d {dir} ]] || mkdir -p {dir}

{alias}() {{
  local __utter_cmd
  # Command on stdout, everything else on stderr, so this captures only the command.
  __utter_cmd="$({bin} gen "$@")" || return $?
  [[ -n "$__utter_cmd" ]] && print -z -- "$__utter_cmd"
}}

__utter_preexec() {{ __utter_last_cmd=$1 }}

__utter_precmd() {{
  # $? must be captured before anything else runs.
  local __utter_ec=$?
  [[ -n "$__utter_last_cmd" ]] || return 0
  # Builtins only, one redirect, no forks. `cmd=` goes last so a multi-line
  # command needs no escaping.
  {{
    print -r -- "exit=$__utter_ec"
    print -r -- "cwd=$PWD"
    print -r -- "cmd=$__utter_last_cmd"
  }} >! {dir}/$$.state 2>/dev/null
  __utter_last_cmd=
}}

if [[ -z "$__utter_hooks_loaded" ]]; then
  __utter_hooks_loaded=1
  autoload -Uz add-zsh-hook
  add-zsh-hook preexec __utter_preexec
  add-zsh-hook precmd __utter_precmd
fi
"#,
        env = ENV_SESSION_ID,
        bin = sq(bin),
        dir = sq(dir)
    )
}

/// Gated on the output being non-empty rather than on `$status`.
///
/// Whether `set x (cmd)` propagates the command substitution's exit status has
/// varied across fish versions, and this does not need to depend on it: stdout
/// carries only the command, so a failed run leaves the variable empty.
fn fish(alias: &str, bin: &str, dir: &str) -> String {
    format!(
        r#"# utter shell integration (fish)
set -gx {env} $fish_pid
test -d {dir}; or mkdir -p {dir}

function {alias}
    # Command on stdout, everything else on stderr, so this captures only the command.
    set -l __utter_cmd ({bin} gen $argv)
    if test -n "$__utter_cmd"
        commandline -r -- "$__utter_cmd"
    end
end

function __utter_postexec --on-event fish_postexec
    # $status must be captured before anything else runs.
    set -l __utter_ec $status
    test -n "$argv[1]"; or return 0
    # `printf` is a builtin; `cmd=` goes last so a multi-line command needs no
    # escaping.
    printf 'exit=%s\ncwd=%s\ncmd=%s\n' $__utter_ec $PWD $argv[1] >{dir}/$fish_pid.state 2>/dev/null
end
"#,
        env = ENV_SESSION_ID,
        bin = sq_fish(bin),
        dir = sq_fish(dir)
    )
}

/// bash has no `print -z` equivalent, so the UX shape genuinely differs: type the
/// request on the command line, press Ctrl-G, and readline replaces it with the
/// command. `bind -x` is the only mechanism that can write `READLINE_LINE`.
///
/// The last-command hook uses the `DEBUG` trap, bash's nearest thing to `preexec`.
/// Known limitation: `DEBUG` fires once per simple command, so for a pipeline
/// `a | b` the recorded command is the last one bash reports rather than the whole
/// line. Good enough to diagnose a failure, and it costs no fork — the alternative,
/// `$(history 1)`, forks on every prompt.
fn bash(alias: &str, bin: &str, dir: &str) -> String {
    format!(
        r#"# utter shell integration (bash)
#
# Ctrl-G rewrites the current line into a command. Type your request, press Ctrl-G.
# bash has no `print -z`, so this differs from the zsh/fish flow by design.
export {env}=$$
[[ -d {dir} ]] || mkdir -p {dir}

__utter_accept() {{
  local __utter_cmd
  __utter_cmd="$({bin} gen "$READLINE_LINE")" || return $?
  if [[ -n "$__utter_cmd" ]]; then
    READLINE_LINE="$__utter_cmd"
    READLINE_POINT=${{#READLINE_LINE}}
  fi
}}

# Only bind in an interactive shell; `bind` fails noisily otherwise.
case $- in
  *i*) bind -x '"\C-g": __utter_accept' ;;
esac

# Prints the command rather than inserting it — a function cannot reach readline.
{alias}() {{
  {bin} gen "$@"
}}

__utter_preexec() {{
  # Skip completion machinery and our own prompt hook.
  [[ -n "$COMP_LINE" ]] && return
  [[ "$BASH_COMMAND" == __utter_precmd* ]] && return
  __utter_last_cmd=$BASH_COMMAND
}}

__utter_precmd() {{
  # $? must be captured before anything else runs.
  local __utter_ec=$?
  [[ -n "$__utter_last_cmd" ]] || return 0
  {{
    printf 'exit=%s\n' "$__utter_ec"
    printf 'cwd=%s\n' "$PWD"
    printf 'cmd=%s\n' "$__utter_last_cmd"
  }} > {dir}/$$.state 2>/dev/null
  __utter_last_cmd=
}}

if [[ -z "$__utter_hooks_loaded" ]]; then
  __utter_hooks_loaded=1
  trap '__utter_preexec' DEBUG
  # Prepend, so an existing PROMPT_COMMAND still runs.
  PROMPT_COMMAND="__utter_precmd${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}"
fi
"#,
        env = ENV_SESSION_ID,
        bin = sq(bin),
        dir = sq(dir)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn script(shell: ShellKind, alias: &str) -> String {
        init_script(
            shell,
            alias,
            &PathBuf::from("/home/u/.local/state/utter/shell"),
        )
    }

    const ALL: [ShellKind; 3] = [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish];

    /// Just the prompt-hook portion of the script, with comments stripped.
    ///
    /// Scoping matters: the `{alias}` function above the hooks legitimately uses
    /// command substitution and a `__utter_cmd=` variable, and the comments quote
    /// `cmd=` in backticks. Searching the whole script for those substrings finds
    /// decoys, not the write block under test.
    fn hook_region(shell: ShellKind) -> String {
        // fish's hook is `__utter_postexec`, so a shared `__utter_pre` marker
        // would silently match nothing and pass vacuously.
        let marker = match shell {
            ShellKind::Fish => "__utter_postexec",
            _ => "__utter_preexec()",
        };
        let s = script(shell, "ask");
        let region = s
            .split(marker)
            .nth(1)
            .unwrap_or_else(|| panic!("{shell:?}: hook marker {marker} not found"))
            .to_string();
        region
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ---- buffer insertion ------------------------------------------------

    #[test]
    fn zsh_uses_print_z_to_reach_the_editor_buffer() {
        let s = script(ShellKind::Zsh, "ask");
        assert!(s.contains("print -z"));
        assert!(s.contains("ask()"));
        // stdout is captured; a non-zero exit must insert nothing.
        assert!(s.contains("|| return"));
    }

    #[test]
    fn fish_uses_commandline_replace() {
        let s = script(ShellKind::Fish, "ask");
        assert!(s.contains("commandline -r --"));
        assert!(s.contains("function ask"));
        // Must not depend on `$status` after a command substitution.
        assert!(!s.contains("return $status"));
        assert!(s.contains(r#"test -n "$__utter_cmd""#));
    }

    #[test]
    fn bash_uses_bind_x_and_readline_variables() {
        let s = script(ShellKind::Bash, "ask");
        assert!(s.contains("READLINE_LINE"));
        assert!(s.contains("READLINE_POINT"));
        assert!(s.contains("bind -x"));
        // Binding in a non-interactive shell errors, so it must be guarded.
        assert!(s.contains("case $- in"));
    }

    #[test]
    fn the_alias_name_is_honoured_in_every_shell() {
        for shell in [ShellKind::Zsh, ShellKind::Bash] {
            let s = script(shell, "ut");
            assert!(s.contains("ut()"), "{shell:?} ignored the alias");
            assert!(
                !s.contains("ask()"),
                "{shell:?} hardcoded the default alias"
            );
        }
        let f = script(ShellKind::Fish, "ut");
        assert!(f.contains("function ut\n"));
        assert!(!f.contains("function ask"));
    }

    // ---- session key -----------------------------------------------------

    #[test]
    fn every_shell_exports_the_session_id() {
        for shell in ALL {
            let s = script(shell, "ask");
            assert!(s.contains(ENV_SESSION_ID), "{shell:?}");
        }
    }

    #[test]
    fn the_session_key_is_the_interactive_shells_own_pid() {
        // Not $PPID and not a random id: the function runs inside `$(...)`, which
        // forks, and `$$` is the only value that survives that unchanged.
        assert!(script(ShellKind::Zsh, "ask").contains("=$$"));
        assert!(script(ShellKind::Bash, "ask").contains("=$$"));
        assert!(script(ShellKind::Fish, "ask").contains("$fish_pid"));
        for shell in [ShellKind::Zsh, ShellKind::Bash] {
            assert!(!script(shell, "ask").contains("$PPID"), "{shell:?}");
        }
    }

    // ---- hooks -----------------------------------------------------------

    #[test]
    fn hooks_never_spawn_a_process() {
        // The whole reason these are builtins: they run on every prompt.
        for shell in ALL {
            let hook_region = hook_region(shell);
            assert!(
                !hook_region.contains("$(") && !hook_region.contains('`'),
                "{shell:?} hook uses command substitution"
            );
            assert!(
                !hook_region.contains("date "),
                "{shell:?} hook spawns date; mtime is the timestamp"
            );
            assert!(
                !hook_region.contains(" gen "),
                "{shell:?} hook invokes the binary on every prompt"
            );
        }
    }

    #[test]
    fn hooks_record_exit_code_cwd_and_command_with_cmd_last() {
        for shell in ALL {
            // `__utter_last_cmd=` also contains the substring `cmd=` and is
            // assigned before the write block, so neutralise it.
            let s = hook_region(shell).replace("__utter_last_cmd", "__utter_saved");
            let exit_at = s.find("exit=").expect("exit=");
            let cwd_at = s.find("cwd=").expect("cwd=");
            let cmd_at = s.find("cmd=").expect("cmd=");
            // `cmd` last is what lets a multi-line command go unescaped.
            assert!(
                exit_at < cmd_at && cwd_at < cmd_at,
                "{shell:?}: expected cmd= last, got exit={exit_at} cwd={cwd_at} cmd={cmd_at}"
            );
        }
    }

    #[test]
    fn hooks_capture_the_status_before_anything_else_clobbers_it() {
        let zsh_script = script(ShellKind::Zsh, "ask");
        let precmd = zsh_script.split("__utter_precmd() {").nth(1).unwrap();
        let first_line = precmd
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .unwrap();
        assert!(first_line.contains("$?"), "got: {first_line}");

        let fish_script = script(ShellKind::Fish, "ask");
        let postexec = fish_script
            .split("--on-event fish_postexec")
            .nth(1)
            .unwrap();
        let first_line = postexec
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .unwrap();
        assert!(first_line.contains("$status"), "got: {first_line}");
    }

    #[test]
    fn hook_registration_is_guarded_against_double_sourcing() {
        for shell in [ShellKind::Zsh, ShellKind::Bash] {
            assert!(
                script(shell, "ask").contains("__utter_hooks_loaded"),
                "{shell:?}"
            );
        }
    }

    #[test]
    fn bash_prepends_rather_than_replacing_prompt_command() {
        let s = script(ShellKind::Bash, "ask");
        assert!(s.contains(r#"PROMPT_COMMAND="__utter_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}""#));
    }

    #[test]
    fn the_state_directory_is_absolute_and_created_once_at_startup() {
        for shell in ALL {
            let s = script(shell, "ask");
            assert!(s.contains("/home/u/.local/state/utter/shell"), "{shell:?}");
            assert!(s.contains("mkdir -p"), "{shell:?}");
        }
    }

    // ---- quoting ---------------------------------------------------------

    #[test]
    fn single_quotes_in_the_binary_path_cannot_break_out() {
        // The embedded quote closes the literal, escapes itself, and reopens.
        let quoted = sq("/tmp/it's here/utter");
        assert_eq!(quoted, r"'/tmp/it'\''s here/utter'");
        assert!(quoted.starts_with('\'') && quoted.ends_with('\''));
    }

    #[test]
    fn fish_quoting_uses_backslash_escapes() {
        // Fish does not support the POSIX '\'' idiom inside single quotes.
        assert_eq!(sq_fish("/tmp/it's here"), r"'/tmp/it\'s here'");
        assert_eq!(sq_fish(r"/tmp/back\slash"), r"'/tmp/back\\slash'");
    }

    #[test]
    fn binary_path_is_absolute_or_the_bare_name() {
        let p = binary_path();
        assert!(p.starts_with('/') || p == "utter", "got {p}");
    }
}
