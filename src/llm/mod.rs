pub mod client;
pub mod tools;
pub mod types;

pub use client::Client;
pub use tools::CommandProposal;
pub use types::{ChatResponse, Message, ToolCall};

/// The only thing the orchestration layer matches on.
///
/// Wire types stop here — `commands::gen` never sees a `finish_reason` or a raw
/// `arguments` string. This is also the seam Phase 2 extends: the captured agent
/// loop switches on `Command { proposal.needs_output }` without touching the
/// transport or the renderer.
#[derive(Debug)]
pub enum ModelOutcome {
    /// Carries the whole `ToolCall`, not just its id: the caller has to persist
    /// the assistant turn verbatim, and Phase 2 needs the arguments again.
    Command {
        call: ToolCall,
        text: Option<String>,
        proposal: CommandProposal,
    },
    /// `finish_reason: "stop"` with prose and no tool call. A valid answer, not an
    /// error — rendered to stderr. Never wait for a tool call that is not coming.
    Text(String),
    /// A tool call arrived but its `arguments` did not parse. The caller feeds
    /// `error` back as a tool result so the model can retry once.
    MalformedArgs { call: ToolCall, error: String },
    /// A tool call for a name we did not advertise.
    UnknownTool { call: ToolCall },
    /// No tool call and no text. Terminal — do not retry, do not hang.
    Empty,
}

/// Normalizes one response into an outcome. Only the first choice and the first
/// tool call are considered: the schema asks for exactly one command, and acting
/// on a second unrequested call would be surprising.
pub fn classify(response: &ChatResponse) -> ModelOutcome {
    let Some(choice) = response.choices.first() else {
        return ModelOutcome::Empty;
    };
    let text = choice
        .message
        .content
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);

    if let Some(call) = choice.message.tool_calls.first() {
        if call.function.name != tools::RUN_COMMAND {
            return ModelOutcome::UnknownTool { call: call.clone() };
        }
        return match tools::parse(&call.function.arguments) {
            Ok(proposal) => ModelOutcome::Command {
                call: call.clone(),
                text,
                proposal,
            },
            Err(e) => ModelOutcome::MalformedArgs {
                call: call.clone(),
                error: e.to_string(),
            },
        };
    }

    match text {
        Some(t) => ModelOutcome::Text(t),
        None => ModelOutcome::Empty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(raw: &str) -> ChatResponse {
        serde_json::from_str(raw).expect("test fixture should parse")
    }

    #[test]
    fn classifies_a_valid_tool_call() {
        let r = response(
            r#"{"choices":[{"finish_reason":"tool_calls","message":{"tool_calls":[{"id":"c1",
               "type":"function","function":{"name":"run_command",
               "arguments":"{\"command\":\"ls -la\",\"risk\":\"safe\"}"}}]}}]}"#,
        );
        match classify(&r) {
            ModelOutcome::Command { call, proposal, .. } => {
                assert_eq!(call.id, "c1");
                assert!(call.function.arguments.contains("ls -la"));
                assert_eq!(proposal.command, "ls -la");
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn classifies_plain_text_as_a_valid_answer() {
        let r = response(
            r#"{"choices":[{"finish_reason":"stop",
               "message":{"content":"That needs a GUI."}}]}"#,
        );
        match classify(&r) {
            ModelOutcome::Text(t) => assert_eq!(t, "That needs a GUI."),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn keeps_narration_that_accompanies_a_tool_call() {
        let r = response(
            r#"{"choices":[{"message":{"content":"Here is why:","tool_calls":[{"id":"c1",
               "function":{"name":"run_command","arguments":"{\"command\":\"pwd\"}"}}]}}]}"#,
        );
        match classify(&r) {
            ModelOutcome::Command { text, .. } => assert_eq!(text.as_deref(), Some("Here is why:")),
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn malformed_arguments_become_a_retryable_outcome() {
        let r = response(
            r#"{"choices":[{"message":{"tool_calls":[{"id":"c1",
               "function":{"name":"run_command","arguments":"{not json"}}]}}]}"#,
        );
        match classify(&r) {
            ModelOutcome::MalformedArgs { call, error } => {
                assert_eq!(call.id, "c1");
                // The raw arguments survive so they can be echoed back verbatim.
                assert_eq!(call.function.arguments, "{not json");
                assert!(!error.is_empty());
            }
            other => panic!("expected MalformedArgs, got {other:?}"),
        }
    }

    #[test]
    fn a_tool_we_never_advertised_is_reported_not_executed() {
        let r = response(
            r#"{"choices":[{"message":{"tool_calls":[{"id":"c1",
               "function":{"name":"rm_everything","arguments":"{}"}}]}}]}"#,
        );
        match classify(&r) {
            ModelOutcome::UnknownTool { call } => {
                assert_eq!(call.function.name, "rm_everything")
            }
            other => panic!("expected UnknownTool, got {other:?}"),
        }
    }

    #[test]
    fn no_content_and_no_tool_call_is_empty_not_a_hang() {
        let r = response(r#"{"choices":[{"message":{"content":null}}]}"#);
        assert!(matches!(classify(&r), ModelOutcome::Empty));
    }

    #[test]
    fn whitespace_only_content_is_empty() {
        let r = response(r#"{"choices":[{"message":{"content":"   \n  "}}]}"#);
        assert!(matches!(classify(&r), ModelOutcome::Empty));
    }

    #[test]
    fn no_choices_is_empty() {
        assert!(matches!(
            classify(&ChatResponse::default()),
            ModelOutcome::Empty
        ));
    }
}
