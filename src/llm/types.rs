use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Outbound and persisted message shape.
///
/// A tagged enum rather than one struct-with-Options because it makes the API's
/// hard invariant checkable in the type system's neighbourhood: every
/// `Assistant` carrying `tool_calls` must be followed by one `Tool` per call id,
/// or the next request 400s. See `conversation::validate`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Message::System {
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Message::User {
            content: content.into(),
        }
    }

    pub fn assistant(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Message::Assistant {
            content,
            tool_calls,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Message::Tool {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    /// Always `"function"` today. Defaulted so a provider that omits it still
    /// deserializes.
    #[serde(rename = "type", default = "function_kind")]
    pub kind: String,
    pub function: FunctionCall,
}

fn function_kind() -> String {
    "function".to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    /// A JSON **string**, not a JSON object — the API double-encodes this field.
    /// It must be parsed separately, and may be malformed. See `llm::tools::parse`.
    pub arguments: String,
}

#[derive(Serialize, Debug)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [Message],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Value>,
    pub max_tokens: u32,
    pub temperature: f32,
}

// ---------------------------------------------------------------------------
// Inbound. Deliberately lenient and separate from the outbound types: every
// field optional, unknown fields ignored. A provider adding a field, or omitting
// one we do not need, must not fail the parse.
// ---------------------------------------------------------------------------

#[derive(Deserialize, Debug, Default)]
pub struct ChatResponse {
    #[serde(default)]
    pub choices: Vec<Choice>,
    /// OpenRouter returns this envelope with **HTTP 200** in some failure modes,
    /// so it is checked independently of the status code.
    #[serde(default)]
    pub error: Option<ApiError>,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct Choice {
    #[serde(default)]
    pub message: ResponseMessage,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
pub struct ResponseMessage {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Deserialize, Debug)]
pub struct ApiError {
    #[serde(default)]
    pub message: String,
    /// Sometimes a string, sometimes an integer, sometimes absent.
    #[serde(default)]
    pub code: Option<Value>,
}

#[derive(Deserialize, Debug, Clone, Copy, Default)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_serialize_with_a_role_tag() {
        let json = serde_json::to_value(Message::user("hi")).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "hi");
    }

    #[test]
    fn assistant_with_no_tool_calls_omits_the_field() {
        let json = serde_json::to_value(Message::assistant(Some("x".into()), vec![])).unwrap();
        assert_eq!(json["role"], "assistant");
        assert!(json.get("tool_calls").is_none());
    }

    #[test]
    fn assistant_with_null_content_omits_content_but_keeps_tool_calls() {
        let call = ToolCall {
            id: "c1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "run_command".into(),
                arguments: "{}".into(),
            },
        };
        let json = serde_json::to_value(Message::assistant(None, vec![call])).unwrap();
        assert!(json.get("content").is_none());
        assert_eq!(json["tool_calls"][0]["id"], "c1");
        assert_eq!(json["tool_calls"][0]["type"], "function");
    }

    #[test]
    fn tool_result_carries_the_call_id() {
        let json = serde_json::to_value(Message::tool_result("c1", "out")).unwrap();
        assert_eq!(json["role"], "tool");
        assert_eq!(json["tool_call_id"], "c1");
    }

    #[test]
    fn messages_round_trip_through_the_session_file_format() {
        let original = vec![
            Message::user("find big files"),
            Message::assistant(
                None,
                vec![ToolCall {
                    id: "c1".into(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: "run_command".into(),
                        arguments: r#"{"command":"ls"}"#.into(),
                    },
                }],
            ),
            Message::tool_result("c1", "ok"),
        ];
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: Vec<Message> = serde_json::from_str(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn response_parses_a_tool_call_and_ignores_unknown_fields() {
        let raw = r#"{
            "id": "gen-1",
            "provider": "anthropic",
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "native_finish_reason": "tool_use",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "run_command", "arguments": "{\"command\":\"ls -la\"}"}
                    }]
                }
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        }"#;
        let parsed: ChatResponse = serde_json::from_str(raw).unwrap();
        let call = &parsed.choices[0].message.tool_calls[0];
        assert_eq!(call.function.name, "run_command");
        assert_eq!(call.function.arguments, r#"{"command":"ls -la"}"#);
        assert_eq!(parsed.usage.unwrap().total_tokens, 15);
    }

    #[test]
    fn response_parses_a_plain_text_answer() {
        let raw = r#"{"choices":[{"finish_reason":"stop",
            "message":{"role":"assistant","content":"No shell command can do that."}}]}"#;
        let parsed: ChatResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.choices[0].finish_reason.as_deref(), Some("stop"));
        assert!(parsed.choices[0].message.tool_calls.is_empty());
    }

    #[test]
    fn error_envelope_parses_with_either_code_type() {
        let as_int: ChatResponse =
            serde_json::from_str(r#"{"error":{"message":"bad","code":402}}"#).unwrap();
        assert_eq!(as_int.error.unwrap().message, "bad");
        let as_str: ChatResponse =
            serde_json::from_str(r#"{"error":{"message":"bad","code":"insufficient_credits"}}"#)
                .unwrap();
        assert_eq!(as_str.error.unwrap().message, "bad");
    }

    #[test]
    fn empty_response_body_does_not_fail_the_parse() {
        let parsed: ChatResponse = serde_json::from_str("{}").unwrap();
        assert!(parsed.choices.is_empty());
        assert!(parsed.error.is_none());
    }
}
