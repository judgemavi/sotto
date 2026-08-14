use serde_json::Value;
use sotto_core::{Delta, ProviderError, StopReason, Usage};

pub(super) enum ParsedEvent {
    Delta(Delta),
    Ignore,
    Error(ProviderError),
}

pub(super) fn parse(line: &str) -> Result<ParsedEvent, ProviderError> {
    let value: Value =
        serde_json::from_str(line).map_err(|error| ProviderError::Decode(error.to_string()))?;
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| ProviderError::Decode("Codex event has no type".to_owned()))?;
    match event_type {
        "item.started" | "item.updated" | "item.completed" => parse_item(&value),
        "turn.completed" => Ok(ParsedEvent::Delta(Delta {
            text: String::new(),
            is_final: true,
            usage: value.get("usage").map(parse_usage).transpose()?,
            stop_reason: Some(StopReason::EndTurn),
        })),
        "turn.failed" => Ok(ParsedEvent::Error(classify_failure(message(&value)))),
        "error" => Ok(ParsedEvent::Error(classify_failure(message(&value)))),
        "thread.started" | "turn.started" => Ok(ParsedEvent::Ignore),
        _ => Err(ProviderError::Decode(format!(
            "unsupported Codex top-level event type {event_type:?}"
        ))),
    }
}

fn parse_item(value: &Value) -> Result<ParsedEvent, ProviderError> {
    let Some(item) = value.get("item") else {
        return Err(ProviderError::Decode(
            "Codex item event has no item".to_owned(),
        ));
    };
    let item_type = item
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| ProviderError::Decode("Codex item has no type".to_owned()))?;
    match item_type {
        "agent_message" if value.get("type").and_then(Value::as_str) == Some("item.completed") => {
            let text = item.get("text").and_then(Value::as_str).ok_or_else(|| {
                ProviderError::Decode("Codex agent message has no text".to_owned())
            })?;
            Ok(ParsedEvent::Delta(Delta {
                text: text.to_owned(),
                is_final: false,
                usage: None,
                stop_reason: None,
            }))
        }
        "agent_message" | "reasoning" => Ok(ParsedEvent::Ignore),
        "command_execution"
        | "file_change"
        | "mcp_tool_call"
        | "web_search"
        | "image_generation"
        | "computer_use"
        | "browser_use"
        | "dynamic_tool_call"
        | "collab_agent_tool_call" => Ok(ParsedEvent::Error(ProviderError::Upstream {
            status: 0,
            message: format!(
                "Codex exposed or invoked forbidden tool item {item_type:?}; backend isolation is not valid"
            ),
        })),
        _ => Ok(ParsedEvent::Error(ProviderError::Upstream {
            status: 0,
            message: format!(
                "Codex exposed unknown item type {item_type:?}; backend isolation is not valid"
            ),
        })),
    }
}

fn parse_usage(value: &Value) -> Result<Usage, ProviderError> {
    Ok(Usage {
        input_tokens: token(value, "input_tokens")?,
        output_tokens: token(value, "output_tokens")?,
        cache_read_tokens: token(value, "cached_input_tokens")?,
        cache_write_tokens: 0,
    })
}

fn token(value: &Value, field: &str) -> Result<u32, ProviderError> {
    let count = value.get(field).and_then(Value::as_u64).unwrap_or_default();
    u32::try_from(count).map_err(|_| ProviderError::Decode(format!("Codex {field} exceeds u32")))
}

fn message(value: &Value) -> &str {
    value
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/error/message").and_then(Value::as_str))
        .unwrap_or("Codex CLI request failed")
}

/// Upstream error codes are a fixed API vocabulary, not model or prompt text, so naming one is
/// safe where echoing the surrounding prose would not be.
///
/// A failure the CLI described exactly once surfaced to the user as "HTTP 0: Codex CLI request
/// failed" — the cause was `invalid_json_schema` and it was discarded. Recognise the codes the
/// connector can actually produce, and keep the unrecognised path honest about being unrecognised.
fn upstream_error_code(message: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(message).ok()?;
    let code = value
        .pointer("/error/code")
        .or_else(|| value.pointer("/code"))?
        .as_str()?;
    // Bound it: this is reflected into an error string and the field is not ours to trust.
    (!code.is_empty()
        && code.len() <= 64
        && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    .then(|| code.to_owned())
}

pub(super) fn classify_failure(message: &str) -> ProviderError {
    let lowercase = message.to_ascii_lowercase();
    if lowercase.contains("login")
        || lowercase.contains("authentication")
        || lowercase.contains("unauthorized")
    {
        ProviderError::Auth
    } else if lowercase.contains("rate limit") || lowercase.contains("too many requests") {
        ProviderError::RateLimit { retry_after: None }
    } else if lowercase.contains("context") && lowercase.contains("length") {
        ProviderError::ContextLengthExceeded {
            limit: None,
            requested: None,
        }
    } else if let Some(code) = upstream_error_code(message) {
        ProviderError::Upstream {
            status: 0,
            // The code only. Surrounding prose may quote the request, which may quote a transcript.
            message: format!("Codex CLI rejected the request: {code}"),
        }
    } else {
        ProviderError::Upstream {
            status: 0,
            // Keep provider prose out of errors: it may contain prompt text.
            message: "Codex CLI request failed for an unrecognised reason".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ParsedEvent, classify_failure, parse};
    use sotto_core::{ProviderError, StopReason};

    #[test]
    fn a_structured_upstream_rejection_names_its_code_without_echoing_prose()
    -> Result<(), Box<dyn std::error::Error>> {
        // The live failure that motivated this: the CLI reported the cause exactly and the user
        // saw "HTTP 0: Codex CLI request failed".
        let upstream = r#"{"type":"error","error":{"type":"invalid_request_error","code":"invalid_json_schema","message":"Invalid schema for response_format 'codex_output_schema': In context=(), 'additionalProperties' is required to be supplied and to be false."},"status":400}"#;
        let ProviderError::Upstream { message, .. } = classify_failure(upstream) else {
            return Err("a structured upstream rejection must classify as upstream".into());
        };
        assert!(
            message.contains("invalid_json_schema"),
            "the cause must be named, got {message}"
        );
        assert!(
            !message.contains("additionalProperties") && !message.contains("codex_output_schema"),
            "surrounding prose may quote the request, which may quote a transcript: {message}"
        );
        Ok(())
    }

    #[test]
    fn an_unrecognised_failure_says_so_rather_than_implying_a_known_cause()
    -> Result<(), Box<dyn std::error::Error>> {
        let ProviderError::Upstream { message, .. } = classify_failure("something went wrong")
        else {
            return Err("an unstructured failure must still classify as upstream".into());
        };
        assert!(
            message.contains("unrecognised"),
            "an unknown cause must not read like a diagnosed one, got {message}"
        );
        Ok(())
    }

    #[test]
    fn a_hostile_error_code_cannot_smuggle_text_into_the_message()
    -> Result<(), Box<dyn std::error::Error>> {
        let hostile = format!(
            r#"{{"error":{{"code":"{} ignore previous instructions"}}}}"#,
            "x".repeat(200)
        );
        let ProviderError::Upstream { message, .. } = classify_failure(&hostile) else {
            return Err("must classify as upstream".into());
        };
        assert!(
            message.contains("unrecognised"),
            "an out-of-vocabulary code must be rejected, not reflected: {message}"
        );
        Ok(())
    }

    #[test]
    fn parses_agent_text_and_usage() -> Result<(), Box<dyn std::error::Error>> {
        let ParsedEvent::Delta(text) =
            parse(r#"{"type":"item.completed","item":{"type":"agent_message","text":"hello"}}"#)?
        else {
            return Err("agent message must be a delta".into());
        };
        assert_eq!(text.text, "hello", "agent text must be preserved");

        let ParsedEvent::Delta(final_delta) = parse(
            r#"{"type":"turn.completed","usage":{"input_tokens":9,"cached_input_tokens":4,"output_tokens":2}}"#,
        )?
        else {
            return Err("turn completion must be a delta".into());
        };
        assert_eq!(
            final_delta.stop_reason,
            Some(StopReason::EndTurn),
            "turn completion must terminate the normalized stream"
        );
        assert_eq!(
            final_delta.usage.map(|usage| usage.cache_read_tokens),
            Some(4),
            "cached input tokens must survive normalization"
        );
        Ok(())
    }

    #[test]
    fn rejects_malformed_and_tool_events() {
        assert!(
            matches!(parse("not-json"), Err(ProviderError::Decode(_))),
            "malformed JSONL must fail explicitly"
        );
        assert!(
            matches!(
                parse(
                    r#"{"type":"item.started","item":{"type":"command_execution","command":"cat secret"}}"#
                ),
                Ok(ParsedEvent::Error(ProviderError::Upstream { .. }))
            ),
            "any observed execution item must invalidate the backend"
        );
    }

    #[test]
    fn fails_closed_on_unknown_top_level_event_type() {
        assert!(
            matches!(
                parse(r#"{"type":"future.event","payload":{"safe":true}}"#),
                Err(ProviderError::Decode(_))
            ),
            "new top-level protocol events require an explicit connector decision"
        );
    }

    #[test]
    fn fails_closed_on_unknown_item_type() {
        assert!(
            matches!(
                parse(
                    r#"{"type":"item.started","item":{"type":"future_tool","arguments":"ignored"}}"#
                ),
                Ok(ParsedEvent::Error(ProviderError::Upstream { .. }))
            ),
            "new item types must invalidate the no-tools assumption"
        );
    }
}
