//! Message-array assembly, kept separate from both the transport (`llm::client`)
//! and the output path (`output`).
//!
//! Phase 2's captured agent loop appends turns through this module and nowhere
//! else, so the tool_call/tool_result pairing invariant has exactly one home.

use anyhow::Result;

use crate::error::UtterError;
use crate::llm::types::{Message, ToolCall};

/// Opens a thread. The system prompt is prepended here for the request and is
/// never stored — `session` persists only what `history` returns.
pub fn open(system: String, user_request: &str) -> Vec<Message> {
    vec![Message::system(system), Message::user(user_request)]
}

/// Replaces the leading system message, or inserts one if absent. Called on every
/// invocation because cwd, shell and available tools drift between runs.
pub fn refresh_system(messages: &mut Vec<Message>, system: String) {
    match messages.first_mut() {
        Some(first @ Message::System { .. }) => *first = Message::system(system),
        _ => messages.insert(0, Message::system(system)),
    }
}

/// Everything except the system prompt — what gets written to the session file.
pub fn history(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .filter(|m| !matches!(m, Message::System { .. }))
        .cloned()
        .collect()
}

pub fn push_user(messages: &mut Vec<Message>, request: &str) {
    messages.push(Message::user(request));
}

/// Records the assistant turn that requested a tool call, then the result. Always
/// call both, in this order — an assistant `tool_calls` entry with no matching
/// `tool` message makes the *next* request 400.
pub fn push_tool_call(messages: &mut Vec<Message>, text: Option<String>, call: ToolCall) {
    messages.push(Message::assistant(text, vec![call]));
}

pub fn push_tool_result(
    messages: &mut Vec<Message>,
    call_id: impl Into<String>,
    content: impl Into<String>,
) {
    messages.push(Message::tool_result(call_id, content));
}

pub fn push_assistant_text(messages: &mut Vec<Message>, text: impl Into<String>) {
    messages.push(Message::assistant(Some(text.into()), vec![]));
}

/// Fails locally on an orphaned tool call rather than letting the API reject the
/// request with an opaque 400. Cheap, and it makes the invariant testable.
pub fn validate(messages: &[Message]) -> Result<()> {
    for (index, message) in messages.iter().enumerate() {
        let Message::Assistant { tool_calls, .. } = message else {
            continue;
        };
        for call in tool_calls {
            let answered = messages[index + 1..].iter().any(|later| {
                matches!(later, Message::Tool { tool_call_id, .. } if tool_call_id == &call.id)
            });
            if !answered {
                return Err(UtterError::OrphanedToolCall {
                    id: call.id.clone(),
                }
                .into());
            }
        }
    }
    Ok(())
}

/// Drops the oldest complete turns until the estimated token count fits.
///
/// Cheap 4-bytes-per-token estimate: a real tokenizer would mean shipping vocab
/// files for every model the user might configure, to decide something that only
/// needs to be approximately right.
///
/// Truncation always happens at a turn boundary — a user message — so a surviving
/// assistant tool_call keeps its tool result and the array stays valid.
pub fn trim_to_budget(messages: &mut Vec<Message>, budget_tokens: usize) {
    let system = matches!(messages.first(), Some(Message::System { .. }));
    let first_trimmable = usize::from(system);

    while estimate_tokens(messages) > budget_tokens {
        // Find the next user message after the current head, and cut everything
        // before it.
        let next_turn = messages
            .iter()
            .enumerate()
            .skip(first_trimmable + 1)
            .find(|(_, m)| matches!(m, Message::User { .. }))
            .map(|(i, _)| i);

        match next_turn {
            Some(i) => {
                messages.drain(first_trimmable..i);
            }
            // Only one turn left; it stays even if it exceeds the budget. Dropping
            // the user's actual request would be worse than a large request.
            None => break,
        }
    }
}

pub fn estimate_tokens(messages: &[Message]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

fn estimate_message_tokens(message: &Message) -> usize {
    // ~4 chars per token, plus a small constant for role/structural overhead.
    const OVERHEAD: usize = 4;
    let chars = match message {
        Message::System { content } | Message::User { content } => content.len(),
        Message::Tool { content, .. } => content.len(),
        Message::Assistant {
            content,
            tool_calls,
        } => {
            content.as_ref().map_or(0, |c| c.len())
                + tool_calls
                    .iter()
                    .map(|c| c.function.name.len() + c.function.arguments.len())
                    .sum::<usize>()
        }
    };
    chars / 4 + OVERHEAD
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::FunctionCall;

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "run_command".into(),
                arguments: r#"{"command":"ls"}"#.into(),
            },
        }
    }

    #[test]
    fn open_puts_system_first_then_the_request() {
        let m = open("SYS".into(), "list files");
        assert!(matches!(&m[0], Message::System { content } if content == "SYS"));
        assert!(matches!(&m[1], Message::User { content } if content == "list files"));
    }

    #[test]
    fn history_excludes_the_system_prompt() {
        // The system prompt is rebuilt every run, so persisting it would pin a
        // stale cwd and tool list into the session file.
        let m = open("SYS".into(), "hi");
        let h = history(&m);
        assert_eq!(h.len(), 1);
        assert!(!h.iter().any(|m| matches!(m, Message::System { .. })));
    }

    #[test]
    fn refresh_system_replaces_rather_than_stacking() {
        let mut m = open("OLD".into(), "hi");
        refresh_system(&mut m, "NEW".into());
        assert_eq!(m.len(), 2);
        assert!(matches!(&m[0], Message::System { content } if content == "NEW"));
    }

    #[test]
    fn refresh_system_inserts_when_loading_a_stored_history() {
        let mut m = vec![Message::user("hi")];
        refresh_system(&mut m, "SYS".into());
        assert_eq!(m.len(), 2);
        assert!(matches!(&m[0], Message::System { .. }));
    }

    #[test]
    fn validate_accepts_a_paired_tool_call() {
        let mut m = open("SYS".into(), "hi");
        push_tool_call(&mut m, None, call("c1"));
        push_tool_result(&mut m, "c1", "ok");
        assert!(validate(&m).is_ok());
    }

    #[test]
    fn validate_rejects_an_orphaned_tool_call() {
        let mut m = open("SYS".into(), "hi");
        push_tool_call(&mut m, None, call("c1"));
        let err = validate(&m).unwrap_err().to_string();
        assert!(err.contains("c1"), "{err}");
    }

    #[test]
    fn validate_rejects_a_result_that_answers_the_wrong_call() {
        let mut m = open("SYS".into(), "hi");
        push_tool_call(&mut m, None, call("c1"));
        push_tool_result(&mut m, "c2", "ok");
        assert!(validate(&m).is_err());
    }

    #[test]
    fn validate_requires_the_result_to_follow_the_call() {
        // A result appearing *before* its call does not satisfy the API.
        let m = vec![
            Message::tool_result("c1", "early"),
            Message::assistant(None, vec![call("c1")]),
        ];
        assert!(validate(&m).is_err());
    }

    #[test]
    fn validate_accepts_plain_text_only_threads() {
        let mut m = open("SYS".into(), "hi");
        push_assistant_text(&mut m, "no command applies");
        assert!(validate(&m).is_ok());
    }

    #[test]
    fn trim_keeps_the_system_prompt_and_the_newest_turn() {
        let mut m = open("SYS".into(), "turn one");
        push_assistant_text(&mut m, "x".repeat(4_000));
        push_user(&mut m, "turn two");
        push_assistant_text(&mut m, "y".repeat(4_000));
        push_user(&mut m, "turn three");

        trim_to_budget(&mut m, 500);

        assert!(matches!(&m[0], Message::System { .. }));
        assert!(
            matches!(m.last(), Some(Message::User { content }) if content == "turn three"),
            "newest turn must survive: {m:?}"
        );
        assert!(estimate_tokens(&m) <= 500);
    }

    #[test]
    fn trim_never_splits_a_tool_call_from_its_result() {
        let mut m = open("SYS".into(), "turn one");
        push_tool_call(&mut m, None, call("c1"));
        push_tool_result(&mut m, "c1", "z".repeat(8_000));
        push_user(&mut m, "turn two");
        push_tool_call(&mut m, None, call("c2"));
        push_tool_result(&mut m, "c2", "ok");

        trim_to_budget(&mut m, 200);

        // Whatever survives must still be a legal request body.
        assert!(validate(&m).is_ok(), "{m:?}");
    }

    #[test]
    fn trim_leaves_a_single_oversized_turn_alone() {
        // Better to send one large request than to delete the user's actual ask.
        let mut m = open("SYS".into(), &"q".repeat(20_000));
        trim_to_budget(&mut m, 10);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn trim_is_a_no_op_under_budget() {
        let mut m = open("SYS".into(), "short");
        let before = m.clone();
        trim_to_budget(&mut m, 10_000);
        assert_eq!(m, before);
    }

    #[test]
    fn estimate_grows_with_content() {
        let small = vec![Message::user("hi")];
        let large = vec![Message::user("x".repeat(4_000))];
        assert!(estimate_tokens(&large) > estimate_tokens(&small));
        assert!(estimate_tokens(&large) >= 1_000);
    }
}
