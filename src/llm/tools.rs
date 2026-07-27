use serde::Deserialize;
use serde_json::{json, Value};

use crate::risk::Risk;

pub const RUN_COMMAND: &str = "run_command";

/// v1 exposes exactly one tool. `needs_output` is the mode switch that Phase 2
/// reads to enter the captured agent loop; v1 records it and still hands the
/// command to the user.
pub fn definitions() -> Vec<Value> {
    vec![json!({
        "type": "function",
        "function": {
            "name": RUN_COMMAND,
            "description": "Propose a shell command that accomplishes the user's request.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The exact shell command."
                    },
                    "description": {
                        "type": "string",
                        "description": "One concise plain-language sentence: what this does and why."
                    },
                    "risk": {
                        "type": "string",
                        "enum": ["safe", "caution", "danger"],
                        "description": "safe = read-only; caution = recoverable modification; danger = destructive, irreversible, or system-wide."
                    },
                    "needs_output": {
                        "type": "boolean",
                        "description": "True ONLY if you must read this command's output to continue. False for a self-contained answer the user runs themselves."
                    }
                },
                "required": ["command", "description", "risk", "needs_output"]
            }
        }
    })]
}

/// Parsed `run_command` arguments.
///
/// Only `command` is genuinely required. Everything else is defaulted rather than
/// `deny_unknown_fields`-strict: models routinely add stray properties or omit
/// `needs_output`, and discarding a perfectly good command over that would be a
/// self-inflicted failure. `risk` defaults to `Caution`, never `Safe`.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct CommandProposal {
    pub command: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub risk: Risk,
    #[serde(default)]
    pub needs_output: bool,
}

#[derive(Debug)]
pub enum ParseError {
    /// `arguments` was not valid JSON, or not an object.
    Malformed(String),
    /// Parsed, but `command` was absent or blank.
    EmptyCommand,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Malformed(e) => write!(f, "arguments were not valid JSON: {e}"),
            ParseError::EmptyCommand => f.write_str("the `command` field was missing or empty"),
        }
    }
}

/// Parses the JSON-string `arguments` field of a tool call.
///
/// Never panics. On failure the caller feeds the error text back as a tool result
/// so the model can retry once.
pub fn parse(arguments: &str) -> Result<CommandProposal, ParseError> {
    let mut proposal = match serde_json::from_str::<CommandProposal>(arguments) {
        Ok(p) => p,
        Err(first) => {
            // Some providers double-encode: `arguments` is a JSON string whose
            // contents are themselves the JSON object. Unwrap one layer and retry
            // before giving up.
            match serde_json::from_str::<String>(arguments)
                .ok()
                .and_then(|inner| serde_json::from_str::<CommandProposal>(&inner).ok())
            {
                Some(p) => p,
                None => return Err(ParseError::Malformed(first.to_string())),
            }
        }
    };

    proposal.command = proposal.command.trim().to_string();
    if proposal.command.is_empty() {
        return Err(ParseError::EmptyCommand);
    }
    proposal.description = proposal.description.trim().to_string();
    Ok(proposal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_matches_the_documented_contract() {
        let defs = definitions();
        assert_eq!(defs.len(), 1, "v1 exposes exactly one tool");
        let f = &defs[0]["function"];
        assert_eq!(f["name"], RUN_COMMAND);
        let required = f["parameters"]["required"].as_array().unwrap();
        for key in ["command", "description", "risk", "needs_output"] {
            assert!(required.iter().any(|r| r == key), "missing required {key}");
        }
        let enum_values = f["parameters"]["properties"]["risk"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(enum_values.len(), 3);
    }

    #[test]
    fn parses_a_well_formed_call() {
        let p = parse(
            r#"{"command":"ls -la","description":"list files","risk":"safe","needs_output":false}"#,
        )
        .unwrap();
        assert_eq!(p.command, "ls -la");
        assert_eq!(p.risk, Risk::Safe);
        assert!(!p.needs_output);
    }

    #[test]
    fn tolerates_extra_fields_and_missing_optionals() {
        let p = parse(r#"{"command":"pwd","reasoning":"because","confidence":0.9}"#).unwrap();
        assert_eq!(p.command, "pwd");
        assert_eq!(p.description, "");
        // Absent risk must not become Safe.
        assert_eq!(p.risk, Risk::Caution);
        assert!(!p.needs_output);
    }

    #[test]
    fn unwraps_double_encoded_arguments() {
        // `arguments` is a JSON string containing a JSON object.
        let inner = r#"{"command":"whoami","risk":"safe"}"#;
        let double = serde_json::to_string(inner).unwrap();
        assert_eq!(parse(&double).unwrap().command, "whoami");
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        let err = parse(r#"{"command": "ls"#).unwrap_err();
        assert!(matches!(err, ParseError::Malformed(_)));
        assert!(err.to_string().contains("not valid JSON"));
    }

    #[test]
    fn non_object_json_is_malformed() {
        assert!(matches!(
            parse("[1,2,3]").unwrap_err(),
            ParseError::Malformed(_)
        ));
        assert!(matches!(parse("").unwrap_err(), ParseError::Malformed(_)));
    }

    #[test]
    fn blank_command_is_rejected() {
        assert!(matches!(
            parse(r#"{"command":"   "}"#).unwrap_err(),
            ParseError::EmptyCommand
        ));
        assert!(matches!(
            parse(r#"{"description":"no command here"}"#).unwrap_err(),
            ParseError::Malformed(_)
        ));
    }

    #[test]
    fn command_and_description_are_trimmed() {
        let p = parse(r#"{"command":"  ls  ","description":"  lists  "}"#).unwrap();
        assert_eq!(p.command, "ls");
        assert_eq!(p.description, "lists");
    }

    #[test]
    fn garbage_risk_value_still_yields_the_command() {
        let p = parse(r#"{"command":"ls","risk":"totally-fine"}"#).unwrap();
        assert_eq!(p.command, "ls");
        assert_eq!(p.risk, Risk::Caution);
    }
}
