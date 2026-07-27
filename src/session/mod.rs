//! Conversation continuity across processes.
//!
//! Every invocation is a fresh process, so the thread lives on disk: one file per
//! shell, at `<state>/sessions/<key>.json`. No index and no lock — the key alone
//! identifies the thread, which is why `--new` is a truncate rather than a
//! bookkeeping operation.

pub mod shell_state;
pub mod store;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::Paths;
use crate::llm::types::Message;

pub use shell_state::ShellState;

/// Set by the shell integration to the interactive shell's own pid.
pub const ENV_SESSION_ID: &str = "UTTER_SESSION_ID";

/// Identifies one shell instance.
///
/// The obvious choice — `parent_id()` — does not work: the integration invokes us
/// inside `$(...)`, which forks, so our parent is a short-lived subshell with a
/// different pid on every call. `$$` in zsh/bash and `$fish_pid` in fish are the
/// interactive shell's pid and are explicitly unchanged inside subshells, so the
/// init script exports one of those and we read it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionKey(String);

impl SessionKey {
    pub fn resolve() -> Self {
        match std::env::var(ENV_SESSION_ID) {
            Ok(raw) => {
                let key = sanitize(&raw);
                if key.is_empty() {
                    Self::detached()
                } else {
                    SessionKey(key)
                }
            }
            // No integration loaded, or a direct `utter gen` from a script. One
            // shared bucket is right here: there is no shell thread to belong to.
            Err(_) => Self::detached(),
        }
    }

    fn detached() -> Self {
        SessionKey("detached".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_detached(&self) -> bool {
        self.0 == "detached"
    }
}

/// The key becomes a filename, so anything outside `[A-Za-z0-9._-]` is dropped
/// rather than escaped — a `/` would silently write outside the sessions
/// directory.
fn sanitize(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(64)
        .collect()
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionMeta {
    pub created_at: u64,
    pub updated_at: u64,
    /// Where the thread started. Kept for context; the user is free to `cd`.
    pub cwd: Option<String>,
    pub model: String,
}

/// Persisted thread.
///
/// `messages` never contains the system prompt — cwd, shell and available tools
/// change between runs, so it is rebuilt every invocation.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Session {
    pub meta: SessionMeta,
    pub messages: Vec<Message>,
}

impl Session {
    fn new(model: &str) -> Self {
        let now = store::now_unix();
        Self {
            meta: SessionMeta {
                created_at: now,
                updated_at: now,
                cwd: std::env::current_dir()
                    .ok()
                    .map(|p| p.display().to_string()),
                model: model.to_string(),
            },
            messages: Vec::new(),
        }
    }

    pub fn turn_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|m| matches!(m, Message::User { .. }))
            .count()
    }
}

/// Why the thread we are about to use looks the way it does. Reported to the user
/// so a silently-dropped history is never a surprise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continuity {
    /// No stored thread existed.
    New,
    /// Stored thread reused.
    Resumed,
    /// `--new`, so the stored thread was ignored.
    Restarted,
    /// Past the idle window.
    ExpiredAndRestarted,
    /// The file existed but could not be read as a session.
    UnreadableAndRestarted,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LoadOptions {
    /// `--new`
    pub force_new: bool,
    /// `--continue`: reuse regardless of the idle window.
    pub force_continue: bool,
}

pub fn file_path(paths: &Paths, key: &SessionKey) -> PathBuf {
    paths.sessions_dir.join(format!("{}.json", key.as_str()))
}

pub fn load(
    paths: &Paths,
    key: &SessionKey,
    model: &str,
    idle: Duration,
    options: LoadOptions,
) -> Result<(Session, Continuity)> {
    if options.force_new {
        return Ok((Session::new(model), Continuity::Restarted));
    }

    let path = file_path(paths, key);
    let Some(raw) = store::read_to_string(&path)? else {
        return Ok((Session::new(model), Continuity::New));
    };

    let stored: Session = match serde_json::from_str(&raw) {
        Ok(session) => session,
        // A session file from an older format, or a truncated write. Starting over
        // beats failing the request.
        Err(_) => return Ok((Session::new(model), Continuity::UnreadableAndRestarted)),
    };

    if !options.force_continue {
        let elapsed = store::now_unix().saturating_sub(stored.meta.updated_at);
        // `>=`, not `>`: `session_idle_secs = 0` is a legitimate way to disable
        // threading entirely, and a strict `>` would resume any thread touched
        // within the same second.
        if elapsed >= idle.as_secs() {
            return Ok((Session::new(model), Continuity::ExpiredAndRestarted));
        }
    }

    Ok((stored, Continuity::Resumed))
}

/// Persists the thread. `messages` must already have the system prompt stripped —
/// see `conversation::history`.
pub fn save(
    paths: &Paths,
    key: &SessionKey,
    session: &mut Session,
    messages: Vec<Message>,
) -> Result<()> {
    session.messages = messages;
    session.meta.updated_at = store::now_unix();
    let encoded = serde_json::to_string(session)?;
    store::write_atomic(&file_path(paths, key), &encoded)
}

/// `--clear`: drops both the thread and the recorded shell state for this shell.
pub fn clear(paths: &Paths, key: &SessionKey) -> Result<(bool, bool)> {
    let session_removed = store::remove_if_exists(&file_path(paths, key))?;
    let state_removed = shell_state::clear(&paths.shell_dir, key.as_str())?;
    Ok((session_removed, state_removed))
}

/// Reads what the shell hooks recorded for this shell.
pub fn last_command(
    paths: &Paths,
    key: &SessionKey,
    max_age: Duration,
) -> Result<Option<ShellState>> {
    shell_state::load(&paths.shell_dir, key.as_str(), max_age)
}

pub fn shell_dir(paths: &Paths) -> &Path {
    &paths.shell_dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::Message;

    fn paths_in(tag: &str) -> Paths {
        let root = std::env::temp_dir().join(format!("utter-sess-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&root);
        Paths {
            config_dir: root.join("config"),
            config_file: root.join("config/config.toml"),
            state_dir: root.clone(),
            sessions_dir: root.join("sessions"),
            shell_dir: root.join("shell"),
        }
    }

    fn key() -> SessionKey {
        SessionKey("12345".to_string())
    }

    const IDLE: Duration = Duration::from_secs(1800);

    #[test]
    fn sanitize_strips_path_separators() {
        // A `/` here would write outside the sessions directory.
        assert_eq!(sanitize("../../etc/passwd"), "....etcpasswd");
        assert_eq!(sanitize("12345"), "12345");
        assert_eq!(sanitize("tty-s003.1"), "tty-s003.1");
        assert_eq!(sanitize("!!!"), "");
        assert_eq!(sanitize(&"9".repeat(200)).len(), 64);
    }

    #[test]
    fn a_key_never_escapes_the_sessions_directory() {
        let paths = paths_in("escape");
        let evil = SessionKey(sanitize("../../../tmp/pwned"));
        let path = file_path(&paths, &evil);
        assert_eq!(path.parent().unwrap(), paths.sessions_dir);
    }

    #[test]
    fn first_run_yields_an_empty_new_session() {
        let paths = paths_in("first");
        let (session, continuity) =
            load(&paths, &key(), "m", IDLE, LoadOptions::default()).unwrap();
        assert_eq!(continuity, Continuity::New);
        assert!(session.messages.is_empty());
        assert_eq!(session.turn_count(), 0);
    }

    #[test]
    fn a_saved_session_round_trips() {
        let paths = paths_in("roundtrip");
        let (mut session, _) = load(&paths, &key(), "m", IDLE, LoadOptions::default()).unwrap();

        let messages = vec![
            Message::user("find big files"),
            Message::assistant(Some("here".into()), vec![]),
        ];
        save(&paths, &key(), &mut session, messages.clone()).unwrap();

        let (reloaded, continuity) =
            load(&paths, &key(), "m", IDLE, LoadOptions::default()).unwrap();
        assert_eq!(continuity, Continuity::Resumed);
        assert_eq!(reloaded.messages, messages);
        assert_eq!(reloaded.turn_count(), 1);
    }

    #[test]
    fn the_system_prompt_is_never_what_gets_stored() {
        // Guards the invariant at the layer that writes the file: a stored system
        // prompt would pin a stale cwd and tool list into future requests.
        let paths = paths_in("nosystem");
        let (mut session, _) = load(&paths, &key(), "m", IDLE, LoadOptions::default()).unwrap();
        let full = crate::conversation::open("SYSTEM".into(), "hi");
        save(
            &paths,
            &key(),
            &mut session,
            crate::conversation::history(&full),
        )
        .unwrap();

        let raw = store::read_to_string(&file_path(&paths, &key()))
            .unwrap()
            .unwrap();
        assert!(!raw.contains("SYSTEM"), "{raw}");
    }

    #[test]
    fn force_new_ignores_a_stored_thread() {
        let paths = paths_in("forcenew");
        let (mut session, _) = load(&paths, &key(), "m", IDLE, LoadOptions::default()).unwrap();
        save(&paths, &key(), &mut session, vec![Message::user("old")]).unwrap();

        let (fresh, continuity) = load(
            &paths,
            &key(),
            "m",
            IDLE,
            LoadOptions {
                force_new: true,
                force_continue: false,
            },
        )
        .unwrap();
        assert_eq!(continuity, Continuity::Restarted);
        assert!(fresh.messages.is_empty());
    }

    #[test]
    fn an_idle_thread_expires_but_force_continue_revives_it() {
        let paths = paths_in("expiry");
        let (mut session, _) = load(&paths, &key(), "m", IDLE, LoadOptions::default()).unwrap();
        save(&paths, &key(), &mut session, vec![Message::user("old")]).unwrap();

        // Zero idle window: anything already written is past it.
        let (expired, continuity) =
            load(&paths, &key(), "m", Duration::ZERO, LoadOptions::default()).unwrap();
        assert_eq!(continuity, Continuity::ExpiredAndRestarted);
        assert!(expired.messages.is_empty());

        let (revived, continuity) = load(
            &paths,
            &key(),
            "m",
            Duration::ZERO,
            LoadOptions {
                force_new: false,
                force_continue: true,
            },
        )
        .unwrap();
        assert_eq!(continuity, Continuity::Resumed);
        assert_eq!(revived.messages.len(), 1);
    }

    #[test]
    fn a_corrupt_session_file_restarts_instead_of_failing_the_request() {
        let paths = paths_in("corrupt");
        store::write_atomic(&file_path(&paths, &key()), "{not json").unwrap();
        let (session, continuity) =
            load(&paths, &key(), "m", IDLE, LoadOptions::default()).unwrap();
        assert_eq!(continuity, Continuity::UnreadableAndRestarted);
        assert!(session.messages.is_empty());
    }

    #[test]
    fn threads_are_isolated_per_shell() {
        let paths = paths_in("isolated");
        let a = SessionKey("111".into());
        let b = SessionKey("222".into());

        let (mut sa, _) = load(&paths, &a, "m", IDLE, LoadOptions::default()).unwrap();
        save(&paths, &a, &mut sa, vec![Message::user("in shell a")]).unwrap();

        let (sb, continuity) = load(&paths, &b, "m", IDLE, LoadOptions::default()).unwrap();
        assert_eq!(continuity, Continuity::New);
        assert!(sb.messages.is_empty());
    }

    #[test]
    fn clear_removes_both_the_thread_and_the_recorded_shell_state() {
        let paths = paths_in("clear");
        let (mut session, _) = load(&paths, &key(), "m", IDLE, LoadOptions::default()).unwrap();
        save(&paths, &key(), &mut session, vec![Message::user("x")]).unwrap();
        store::write_atomic(
            &shell_state::path(&paths.shell_dir, key().as_str()),
            "exit=1\ncmd=ls\n",
        )
        .unwrap();

        assert_eq!(clear(&paths, &key()).unwrap(), (true, true));
        assert_eq!(clear(&paths, &key()).unwrap(), (false, false));
    }

    #[test]
    fn last_command_reads_what_the_hooks_wrote() {
        let paths = paths_in("lastcmd");
        store::write_atomic(
            &shell_state::path(&paths.shell_dir, key().as_str()),
            "exit=1\ncwd=/tmp\ncmd=tar -xf a.tar.gz\n",
        )
        .unwrap();

        let state = last_command(&paths, &key(), IDLE).unwrap().unwrap();
        assert_eq!(state.command, "tar -xf a.tar.gz");
        assert!(state.failed());
    }

    #[test]
    fn saving_advances_updated_at() {
        let paths = paths_in("touch");
        let (mut session, _) = load(&paths, &key(), "m", IDLE, LoadOptions::default()).unwrap();
        session.meta.updated_at = 0;
        save(&paths, &key(), &mut session, vec![]).unwrap();
        assert!(session.meta.updated_at > 0);
    }
}
