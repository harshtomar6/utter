use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;

use crate::error::UtterError;

pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// Verified live against `GET /api/v1/models`. Chosen for function-calling
/// reliability rather than price — every path in this tool depends on a
/// well-formed tool call, so a cheaper model that fumbles `tool_calls` costs
/// more than it saves.
pub const DEFAULT_MODEL: &str = "anthropic/claude-haiku-4.5";
pub const DEFAULT_SMART_MODEL: &str = "anthropic/claude-sonnet-5";

pub const ENV_API_KEY: &str = "OPENROUTER_API_KEY";
pub const ENV_BASE_URL: &str = "UTTER_BASE_URL";
pub const ENV_MODEL: &str = "UTTER_MODEL";

const DEFAULT_MAX_TOKENS: u32 = 1024;
const DEFAULT_TEMPERATURE: f32 = 0.2;
const DEFAULT_SESSION_IDLE_SECS: u64 = 30 * 60;
const DEFAULT_HISTORY_TOKEN_BUDGET: usize = 8_000;
const DEFAULT_CAPTURED_OUTPUT_LIMIT: usize = 4_000;

/// XDG paths, built by hand rather than taken from `directories`.
///
/// `directories` returns Apple-style locations on macOS
/// (`~/Library/Application Support/...`) and has no `state_dir` there at all.
/// A shell-integration tool needs one documented path per file across macOS and
/// Linux — install.sh, the README and the shell hooks all hard-code them — so we
/// use XDG on both, honouring `XDG_CONFIG_HOME` / `XDG_STATE_HOME` when set.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub state_dir: PathBuf,
    /// One file per tty holds that tty's live conversation thread.
    pub sessions_dir: PathBuf,
    /// Sidecars written directly by the shell hooks using builtins only.
    pub shell_dir: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Self> {
        let home = home_dir().ok_or(UtterError::NoHomeDir)?;
        let config_dir = xdg_dir("XDG_CONFIG_HOME", ".config", &home);
        let state_dir = xdg_dir("XDG_STATE_HOME", ".local/state", &home);
        Ok(Self {
            config_file: config_dir.join("config.toml"),
            config_dir,
            sessions_dir: state_dir.join("sessions"),
            shell_dir: state_dir.join("shell"),
            state_dir,
        })
    }
}

fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
}

/// An `XDG_*` value must be absolute per spec; a relative one is ignored rather
/// than resolved against the cwd, which would scatter state across directories.
fn xdg_dir(env_key: &str, fallback: &str, home: &Path) -> PathBuf {
    let base = match std::env::var_os(env_key) {
        Some(v) if !v.is_empty() && Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => home.join(fallback),
    };
    base.join("utter")
}

/// Mirror of `config.toml`, every field optional.
///
/// `deny_unknown_fields` on purpose: a typo'd key should be a loud error, not a
/// setting that silently never applies.
#[derive(Deserialize, Default, Debug)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub smart_model: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub session_idle_secs: Option<u64>,
    pub history_token_budget: Option<usize>,
    pub captured_output_limit: Option<usize>,
    /// Optional OpenRouter dashboard attribution.
    pub referer: Option<String>,
    pub title: Option<String>,
}

impl ConfigFile {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            // A missing config file is the normal case: env-only setup.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(anyhow::Error::new(e)
                    .context(format!("reading config file {}", path.display())))
            }
        };
        toml::from_str(&raw).map_err(|source| {
            UtterError::BadConfigFile {
                path: path.display().to_string(),
                source,
            }
            .into()
        })
    }
}

/// CLI-supplied overrides. Highest precedence.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    pub model: Option<String>,
    pub smart: bool,
}

/// Fully resolved settings. Precedence: CLI flag > env > config file > default.
pub struct Config {
    /// Private, and excluded from the hand-written `Debug` below — `utter config`
    /// prints this struct and must never leak the key.
    api_key: Option<String>,
    config_file: PathBuf,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub session_idle_secs: u64,
    pub history_token_budget: usize,
    pub captured_output_limit: usize,
    pub referer: Option<String>,
    pub title: Option<String>,
}

impl Config {
    pub fn load(paths: &Paths, overrides: &Overrides) -> Result<Self> {
        let file = ConfigFile::load(&paths.config_file)?;

        let smart_model = file
            .smart_model
            .clone()
            .unwrap_or_else(|| DEFAULT_SMART_MODEL.to_string());
        let base_model = env_string(ENV_MODEL)
            .or_else(|| file.model.clone())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());

        // `--model` wins outright; `--smart` only swaps the slug when no explicit
        // model was named.
        let model = match (&overrides.model, overrides.smart) {
            (Some(m), _) => m.clone(),
            (None, true) => smart_model,
            (None, false) => base_model,
        };

        let base_url = env_string(ENV_BASE_URL)
            .or(file.base_url)
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        Ok(Self {
            api_key: env_string(ENV_API_KEY).or(file.api_key),
            config_file: paths.config_file.clone(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            max_tokens: file.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            temperature: file.temperature.unwrap_or(DEFAULT_TEMPERATURE),
            session_idle_secs: file.session_idle_secs.unwrap_or(DEFAULT_SESSION_IDLE_SECS),
            history_token_budget: file
                .history_token_budget
                .unwrap_or(DEFAULT_HISTORY_TOKEN_BUDGET),
            captured_output_limit: file
                .captured_output_limit
                .unwrap_or(DEFAULT_CAPTURED_OUTPUT_LIMIT),
            referer: file.referer,
            title: file.title,
        })
    }

    /// Resolved only where it is actually needed, so `utter config` still works
    /// on a machine with no key set.
    pub fn api_key(&self) -> Result<&str> {
        self.api_key.as_deref().ok_or_else(|| {
            UtterError::MissingApiKey {
                env: ENV_API_KEY,
                path: self.config_file.display().to_string(),
            }
            .into()
        })
    }

    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }

    /// Enough of the key to confirm *which* key is loaded, never enough to use.
    pub fn redacted_api_key(&self) -> String {
        match &self.api_key {
            None => "<not set>".to_string(),
            Some(k) if k.chars().count() <= 8 => "<set>".to_string(),
            Some(k) => {
                let tail: String = k
                    .chars()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                format!("…{tail} ({} chars)", k.chars().count())
            }
        }
    }

    pub fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }
}

fn env_string(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("api_key", &self.redacted_api_key())
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .field("session_idle_secs", &self.session_idle_secs)
            .field("history_token_budget", &self.history_token_budget)
            .field("captured_output_limit", &self.captured_output_limit)
            .field("referer", &self.referer)
            .field("title", &self.title)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// `cargo test` runs these on parallel threads in one process, so any test
    /// that touches the process environment must hold this lock.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_guard() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn xdg_override_must_be_absolute() {
        let _guard = env_guard();
        let home = Path::new("/home/u");
        std::env::set_var("UTTER_TEST_XDG", "relative/path");
        assert_eq!(
            xdg_dir("UTTER_TEST_XDG", ".config", home),
            PathBuf::from("/home/u/.config/utter")
        );
        std::env::set_var("UTTER_TEST_XDG", "/custom/cfg");
        assert_eq!(
            xdg_dir("UTTER_TEST_XDG", ".config", home),
            PathBuf::from("/custom/cfg/utter")
        );
        std::env::remove_var("UTTER_TEST_XDG");
    }

    #[test]
    fn xdg_falls_back_when_unset() {
        let _guard = env_guard();
        std::env::remove_var("UTTER_TEST_XDG_UNSET");
        assert_eq!(
            xdg_dir("UTTER_TEST_XDG_UNSET", ".local/state", Path::new("/home/u")),
            PathBuf::from("/home/u/.local/state/utter")
        );
    }

    #[test]
    fn unknown_config_key_is_an_error() {
        let err = toml::from_str::<ConfigFile>("modle = \"typo\"\n").unwrap_err();
        assert!(err.to_string().contains("modle"), "{err}");
    }

    #[test]
    fn config_file_parses_known_keys() {
        let parsed: ConfigFile = toml::from_str(
            r#"
            model = "x/y"
            temperature = 0.5
            base_url = "http://localhost:11434/v1"
        "#,
        )
        .unwrap();
        assert_eq!(parsed.model.as_deref(), Some("x/y"));
        assert_eq!(parsed.temperature, Some(0.5));
    }

    fn cfg_with(api_key: Option<&str>) -> Config {
        Config {
            api_key: api_key.map(str::to_string),
            config_file: PathBuf::from("/tmp/config.toml"),
            base_url: DEFAULT_BASE_URL.into(),
            model: DEFAULT_MODEL.into(),
            max_tokens: 1,
            temperature: 0.0,
            session_idle_secs: 0,
            history_token_budget: 0,
            captured_output_limit: 0,
            referer: None,
            title: None,
        }
    }

    #[test]
    fn debug_never_prints_the_key() {
        let rendered = format!("{:?}", cfg_with(Some("fake-value-for-redaction-test")));
        assert!(!rendered.contains("fake-value-for-redaction"), "{rendered}");
        assert!(rendered.contains("…test"), "{rendered}");
    }

    #[test]
    fn redaction_handles_short_and_missing_keys() {
        assert_eq!(cfg_with(None).redacted_api_key(), "<not set>");
        assert_eq!(cfg_with(Some("abcd")).redacted_api_key(), "<set>");
    }

    #[test]
    fn missing_key_error_names_env_and_path() {
        let err = cfg_with(None).api_key().unwrap_err().to_string();
        assert!(err.contains(ENV_API_KEY), "{err}");
        assert!(err.contains("/tmp/config.toml"), "{err}");
    }

    #[test]
    fn chat_url_survives_a_trailing_slash_in_base_url() {
        let _guard = env_guard();
        let paths = Paths {
            config_dir: PathBuf::from("/c"),
            config_file: PathBuf::from("/c/config.toml"),
            state_dir: PathBuf::from("/s"),
            sessions_dir: PathBuf::from("/s/sessions"),
            shell_dir: PathBuf::from("/s/shell"),
        };
        std::env::set_var(ENV_BASE_URL, "https://example.test/v1/");
        let cfg = Config::load(&paths, &Overrides::default()).unwrap();
        std::env::remove_var(ENV_BASE_URL);
        assert_eq!(cfg.chat_url(), "https://example.test/v1/chat/completions");
    }
}
