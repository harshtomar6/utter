use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::error::UtterError;

use super::types::{ChatRequest, ChatResponse, Message};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// One retry only. This tool sits in front of a human waiting at their prompt;
/// spending 30s on backoff is worse than failing fast with a clear message.
const MAX_ATTEMPTS: u32 = 2;
const RETRY_DELAY: Duration = Duration::from_millis(400);
/// Error bodies get truncated before they reach stderr — some gateways return
/// entire HTML pages.
const MAX_ERROR_BODY: usize = 600;

/// Transport only. Knows how to make one chat request and normalize failures;
/// knows nothing about tools, prompts, sessions or rendering.
pub struct Client {
    http: reqwest::Client,
    url: String,
    api_key: String,
    referer: Option<String>,
    title: Option<String>,
}

impl Client {
    pub fn new(cfg: &Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("utter/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building HTTP client")?;

        Ok(Self {
            http,
            url: cfg.chat_url(),
            api_key: cfg.api_key()?.to_string(),
            referer: cfg.referer.clone(),
            title: cfg.title.clone(),
        })
    }

    pub async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: Vec<serde_json::Value>,
        max_tokens: u32,
        temperature: f32,
    ) -> Result<ChatResponse> {
        let body = ChatRequest {
            model,
            messages,
            tools,
            max_tokens,
            temperature,
        };

        let mut last_err = None;
        for attempt in 1..=MAX_ATTEMPTS {
            match self.send_once(&body).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if attempt < MAX_ATTEMPTS && is_retryable(&e) {
                        tokio::time::sleep(RETRY_DELAY).await;
                        last_err = Some(e);
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        // Unreachable while MAX_ATTEMPTS >= 1, but expressed without a panic.
        Err(last_err.unwrap_or_else(|| UtterError::EmptyResponse.into()))
    }

    async fn send_once(&self, body: &ChatRequest<'_>) -> Result<ChatResponse> {
        let mut req = self
            .http
            .post(&self.url)
            .bearer_auth(&self.api_key)
            .json(body);

        // Optional OpenRouter dashboard attribution.
        if let Some(referer) = &self.referer {
            req = req.header("HTTP-Referer", referer);
        }
        if let Some(title) = &self.title {
            req = req.header("X-Title", title);
        }

        let response = req
            .send()
            .await
            .with_context(|| format!("POST {}", self.url))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .with_context(|| format!("reading response body from {}", self.url))?;

        if !status.is_success() {
            // Prefer the API's own error message over the raw body when present.
            let detail = serde_json::from_str::<ChatResponse>(&text)
                .ok()
                .and_then(|r| r.error)
                .map(|e| e.message)
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| truncate(&text, MAX_ERROR_BODY));
            return Err(UtterError::HttpStatus {
                status: status.as_u16(),
                url: self.url.clone(),
                body: detail,
            }
            .into());
        }

        let parsed: ChatResponse = serde_json::from_str(&text).with_context(|| {
            format!(
                "response from {} was not valid JSON: {}",
                self.url,
                truncate(&text, MAX_ERROR_BODY)
            )
        })?;

        // Checked independently of the status code: this envelope arrives with
        // HTTP 200 in several real failure modes (credits, moderation, upstream).
        if let Some(err) = parsed.error {
            return Err(UtterError::Api {
                message: if err.message.is_empty() {
                    "upstream returned an unspecified error".into()
                } else {
                    err.message
                },
                code: err.code.map(|c| c.to_string()),
            }
            .into());
        }

        if parsed.choices.is_empty() {
            return Err(UtterError::EmptyResponse.into());
        }

        Ok(parsed)
    }
}

/// Retry transport hiccups and upstream 5xx/429 only. A 400 or 401 will fail
/// identically the second time.
fn is_retryable(err: &anyhow::Error) -> bool {
    if let Some(UtterError::HttpStatus { status, .. }) = err.downcast_ref::<UtterError>() {
        return *status == 429 || (500..600).contains(status);
    }
    if let Some(e) = err.downcast_ref::<reqwest::Error>() {
        return e.is_timeout() || e.is_connect() || e.is_request();
    }
    false
}

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max).collect();
    format!("{head}… [truncated]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_covers_429_and_5xx_only() {
        let make = |status| -> anyhow::Error {
            UtterError::HttpStatus {
                status,
                url: "u".into(),
                body: "b".into(),
            }
            .into()
        };
        assert!(is_retryable(&make(429)));
        assert!(is_retryable(&make(500)));
        assert!(is_retryable(&make(503)));
        assert!(!is_retryable(&make(400)));
        assert!(!is_retryable(&make(401)));
        assert!(!is_retryable(&make(404)));
    }

    #[test]
    fn non_http_errors_are_not_retried() {
        assert!(!is_retryable(&anyhow::anyhow!("something else")));
    }

    #[test]
    fn truncate_keeps_short_bodies_intact() {
        assert_eq!(truncate("  hello  ", 100), "hello");
    }

    #[test]
    fn truncate_marks_long_bodies() {
        let out = truncate(&"x".repeat(50), 10);
        assert!(out.ends_with("… [truncated]"));
        assert!(out.starts_with(&"x".repeat(10)));
    }

    #[test]
    fn truncate_is_char_safe_on_multibyte_input() {
        // Byte slicing here would panic mid-codepoint.
        let out = truncate(&"é".repeat(50), 10);
        assert!(out.starts_with(&"é".repeat(10)));
    }
}
