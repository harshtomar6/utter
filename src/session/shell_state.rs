//! What the shell hooks recorded about the previous command.
//!
//! # Format
//!
//! ```text
//! exit=1
//! cwd=/Users/u/dev
//! kind=run
//! cmd=tar -xf archive.tar.gz
//! ```
//!
//! `cmd` is **last** and everything after it is the command verbatim, so a
//! multi-line command needs no escaping — the hooks are pure shell builtins and
//! cannot be asked to quote anything.
//!
//! There is no timestamp field: the file's mtime is the timestamp, which is what
//! lets the hooks avoid spawning `date` on every prompt.
//!
//! # Why there is no captured output
//!
//! `preexec`/`precmd` never see a command's stdout — it went to the terminal, not
//! to the shell. Capturing it would mean wrapping the whole session in a pty,
//! which breaks interactive programs. The command text plus the exit code is
//! enough to diagnose the overwhelming majority of failures, and it is what
//! `thefuck` has worked from for years.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use super::store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellState {
    pub command: String,
    pub exit_code: i32,
    pub cwd: Option<String>,
    /// True when the shell rejected the line before running it — a syntax
    /// error. Worth distinguishing: the fix is in the text itself, and the
    /// model should not hunt for a runtime cause that does not exist.
    pub parse_error: bool,
}

impl ShellState {
    pub fn failed(&self) -> bool {
        self.exit_code != 0
    }
}

pub fn path(shell_dir: &Path, key: &str) -> PathBuf {
    shell_dir.join(format!("{key}.state"))
}

/// Reads the sidecar for this shell.
///
/// `max_age` guards against a recycled pid pointing at some long-dead shell's
/// file. Within a live shell the file is rewritten on every prompt, so whatever it
/// holds genuinely *is* the last command.
pub fn load(shell_dir: &Path, key: &str, max_age: Duration) -> Result<Option<ShellState>> {
    let file = path(shell_dir, key);
    let Some(raw) = store::read_to_string(&file)? else {
        return Ok(None);
    };
    match store::age(&file) {
        Some(age) if age > max_age => return Ok(None),
        _ => {}
    }
    Ok(parse(&raw))
}

pub fn clear(shell_dir: &Path, key: &str) -> Result<bool> {
    store::remove_if_exists(&path(shell_dir, key))
}

/// Returns `None` for anything unparseable rather than erroring: a malformed
/// sidecar should degrade to "no failure recorded", never break a request.
pub fn parse(raw: &str) -> Option<ShellState> {
    let mut exit_code = None;
    let mut cwd = None;
    let mut parse_error = false;

    let mut rest = raw;
    loop {
        // `cmd=` consumes the remainder, newlines included.
        if let Some(command) = rest.strip_prefix("cmd=") {
            let command = command.trim_end_matches('\n').to_string();
            if command.trim().is_empty() {
                return None;
            }
            return Some(ShellState {
                command,
                exit_code: exit_code?,
                cwd,
                parse_error,
            });
        }

        let (line, remainder) = match rest.split_once('\n') {
            Some((line, remainder)) => (line, remainder),
            // Ran out of input without ever reaching `cmd=`.
            None => return None,
        };

        if let Some(value) = line.strip_prefix("exit=") {
            exit_code = value.trim().parse::<i32>().ok();
        } else if let Some(value) = line.strip_prefix("kind=") {
            parse_error = value.trim() == "parse";
        } else if let Some(value) = line.strip_prefix("cwd=") {
            let value = value.trim();
            if !value.is_empty() {
                cwd = Some(value.to_string());
            }
        }
        rest = remainder;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_recorded_failure() {
        let state = parse("exit=1\ncwd=/tmp\ncmd=tar -xf archive.tar.gz\n").unwrap();
        assert_eq!(state.command, "tar -xf archive.tar.gz");
        assert_eq!(state.exit_code, 1);
        assert_eq!(state.cwd.as_deref(), Some("/tmp"));
        assert!(state.failed());
    }

    #[test]
    fn parses_a_success_and_reports_it_as_such() {
        // The hooks record every command, so the caller — not the parser — decides
        // that a zero exit means there is nothing to fix.
        let state = parse("exit=0\ncwd=/tmp\ncmd=ls\n").unwrap();
        assert!(!state.failed());
    }

    #[test]
    fn cmd_last_lets_a_multiline_command_survive_unescaped() {
        let state = parse("exit=2\ncwd=/x\ncmd=for f in *; do\n  echo $f\ndone\n").unwrap();
        assert_eq!(state.command, "for f in *; do\n  echo $f\ndone");
        assert_eq!(state.exit_code, 2);
    }

    #[test]
    fn a_command_containing_equals_signs_is_not_mangled() {
        let state = parse("exit=1\ncwd=/x\ncmd=FOO=bar make target=all\n").unwrap();
        assert_eq!(state.command, "FOO=bar make target=all");
    }

    #[test]
    fn cwd_is_optional() {
        let state = parse("exit=1\ncmd=ls\n").unwrap();
        assert_eq!(state.cwd, None);
    }

    #[test]
    fn unknown_keys_are_ignored_for_forward_compatibility() {
        let state = parse("exit=1\nfuture=whatever\ncwd=/x\ncmd=ls\n").unwrap();
        assert_eq!(state.command, "ls");
    }

    #[test]
    fn a_high_exit_code_parses() {
        assert_eq!(parse("exit=130\ncmd=sleep 100\n").unwrap().exit_code, 130);
    }

    #[test]
    fn malformed_input_degrades_to_none_rather_than_erroring() {
        // No trailing newline after the last header, so `cmd=` is never reached.
        assert!(parse("exit=1\ncwd=/x").is_none());
        // No exit code recorded.
        assert!(parse("cwd=/x\ncmd=ls\n").is_none());
        // Non-numeric exit code.
        assert!(parse("exit=oops\ncmd=ls\n").is_none());
        // Empty command.
        assert!(parse("exit=1\ncmd=   \n").is_none());
        assert!(parse("").is_none());
        assert!(parse("garbage").is_none());
    }

    #[test]
    fn load_ignores_a_sidecar_older_than_max_age() {
        let dir = std::env::temp_dir().join(format!("utter-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        store::write_atomic(&path(&dir, "k"), "exit=1\ncwd=/x\ncmd=ls\n").unwrap();

        // Just written, so a generous window sees it...
        assert!(load(&dir, "k", Duration::from_secs(600)).unwrap().is_some());
        // ...and a zero window treats it as stale.
        assert!(load(&dir, "k", Duration::ZERO).unwrap().is_none());
    }

    #[test]
    fn load_on_a_missing_sidecar_is_none() {
        let dir = std::env::temp_dir().join(format!("utter-absent-{}", std::process::id()));
        assert!(load(&dir, "nokey", Duration::from_secs(600))
            .unwrap()
            .is_none());
    }
}
