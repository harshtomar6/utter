use thiserror::Error;

#[derive(Debug, Error)]
pub enum UtterError {
    /// The message names the signup URL on purpose: this is exactly where a new
    /// user stops, and "no API key found" on its own sends them off to search.
    #[error(
        "no API key found\n  \
         get one at https://openrouter.ai/keys, then either:\n    \
         export {env}=sk-or-v1-...\n    \
         or add  api_key = \"sk-or-v1-...\"  to {path}"
    )]
    MissingApiKey { env: &'static str, path: String },

    #[error("HTTP {status} from {url}\n  {body}")]
    HttpStatus {
        status: u16,
        url: String,
        body: String,
    },

    /// OpenRouter can return an error envelope with HTTP 200, so this is checked
    /// independently of the status code.
    #[error("API error: {message}")]
    Api {
        message: String,
        code: Option<String>,
    },

    #[error("model returned no choices")]
    EmptyResponse,

    #[error("model did not produce a usable command after {attempts} attempt(s)")]
    NoCommand { attempts: u32 },

    #[error("could not determine your home directory")]
    NoHomeDir,

    #[error("config file {path} is not valid TOML\n  {source}")]
    BadConfigFile {
        path: String,
        #[source]
        source: toml::de::Error,
    },

    /// The API rejects a request whose assistant tool_call has no matching tool
    /// result. Caught locally so we fail with a clear message instead of a 400.
    #[error("internal: tool_call {id} has no matching tool result")]
    OrphanedToolCall { id: String },
}
