// SPDX-License-Identifier: Apache-2.0
//! Parse only what routeD needs from `OpenAI` / Anthropic style request bodies.

use routed_classify::ClassifyInput;
use routed_decision::{DecisionInput, RequestHints};
use routed_security::RequestHeaders;
use serde_json::Value;

/// Body could not be parsed into something routeD understands.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Not valid JSON.
    #[error("body is not valid JSON: {0}")]
    Json(String),
    /// JSON but not an object.
    #[error("request body must be a JSON object")]
    NotObject,
    /// `model` missing or not a string.
    #[error("request has no string `model` field")]
    NoModel,
}

/// What routeD extracted from a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedRequest {
    /// `model`.
    pub model: String,
    /// Tools / functions present.
    pub tools_present: bool,
    /// `stream: true`.
    pub stream: bool,
    /// `max_tokens` / `max_completion_tokens` / `max_output_tokens`.
    pub max_tokens: Option<u64>,
    /// Estimated input tokens (`ceil(bytes / 4)` per text plus per-message overhead).
    pub estimated_input_tokens: u64,
    /// Text selected for classification.
    pub classify_input: ClassifyInput,
}

impl ParsedRequest {
    /// Build the engine input.
    #[must_use]
    pub fn to_input(
        &self,
        path: &str,
        headers: &RequestHeaders,
        default_output_tokens: u64,
    ) -> DecisionInput {
        DecisionInput {
            tenant: headers.tenant.clone(),
            agent: headers.agent.clone(),
            path: path.to_owned(),
            requested_model: self.model.clone(),
            tools_present: self.tools_present,
            estimated_input_tokens: self.estimated_input_tokens,
            estimated_output_tokens: self.max_tokens.unwrap_or(default_output_tokens),
            hints: RequestHints {
                data_classes: headers.hints.data_classes.clone(),
                policy: headers.hints.policy.clone(),
                dry_run: headers.hints.dry_run,
            },
        }
    }
}

/// Estimate tokens for a text: `ceil(utf8_bytes / 4)`.
#[must_use]
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

const PER_MESSAGE_OVERHEAD: u64 = 4;
const SYSTEM_PROMPT_MAX_CHARS: usize = 2_000;

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| o.get("content").map(text_of)),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(o) => o
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Parse a request body for the given path.
///
/// # Errors
/// See [`ParseError`].
pub fn parse_request(path: &str, body: &[u8]) -> Result<ParsedRequest, ParseError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| ParseError::Json(e.to_string()))?;
    let obj = v.as_object().ok_or(ParseError::NotObject)?;
    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .ok_or(ParseError::NoModel)?
        .to_owned();
    let tools_present = ["tools", "functions"].iter().any(|k| {
        obj.get(*k)
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
    });
    let tool_def_tokens: u64 = ["tools", "functions"]
        .iter()
        .filter_map(|k| obj.get(*k))
        .map(|v| estimate_tokens(&v.to_string()))
        .sum();
    let stream = obj.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let max_tokens = ["max_tokens", "max_completion_tokens", "max_output_tokens"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_u64))
        .filter(|t| *t > 0);

    let mut system_prompt: Option<String> =
        obj.get("system").map(text_of).filter(|s| !s.is_empty());
    let mut user_text = String::new();
    let mut history: Vec<String> = Vec::new();
    let mut tool_outputs = Vec::new();
    let mut tokens: u64 = 0;

    // chat / messages style
    if let Some(messages) = obj.get("messages").and_then(Value::as_array) {
        for m in messages {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
            let text = m.get("content").map(text_of).unwrap_or_default();
            tokens += estimate_tokens(&text) + PER_MESSAGE_OVERHEAD;
            match role {
                "system" | "developer" => system_prompt = Some(text),
                "tool" | "function" => tool_outputs.push(text),
                "user" => {
                    if !user_text.is_empty() {
                        history.push(std::mem::take(&mut user_text));
                    }
                    user_text = text;
                }
                _ => {}
            }
            if let Some(calls) = m.get("tool_calls").and_then(Value::as_array) {
                tokens += calls
                    .iter()
                    .map(|c| estimate_tokens(&c.to_string()))
                    .sum::<u64>();
            }
        }
    }
    // responses API / completions / embeddings
    if user_text.is_empty() {
        if let Some(input) = obj.get("input").or_else(|| obj.get("prompt")) {
            let text = match input {
                Value::Array(items) if items.iter().all(Value::is_object) => items
                    .iter()
                    .map(|i| {
                        let role = i.get("role").and_then(Value::as_str).unwrap_or("user");
                        let t = i.get("content").map(text_of).unwrap_or_default();
                        if role == "system" || role == "developer" {
                            system_prompt = Some(t.clone());
                        }
                        t
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                other => text_of(other),
            };
            tokens += estimate_tokens(&text) + PER_MESSAGE_OVERHEAD;
            user_text = text;
        }
    }
    if let Some(sp) = &system_prompt {
        if !obj.contains_key("messages") {
            tokens += estimate_tokens(sp);
        }
    }
    let _ = path;
    Ok(ParsedRequest {
        model,
        tools_present,
        stream,
        max_tokens,
        estimated_input_tokens: (tokens + tool_def_tokens).max(1),
        classify_input: ClassifyInput {
            system_prompt: system_prompt.map(|s| truncate_chars(&s, SYSTEM_PROMPT_MAX_CHARS)),
            user_text,
            history,
            tool_outputs,
        },
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn chat_request() {
        let body = br#"{"model":"auto","stream":true,"max_tokens":100,"tools":[{"type":"function"}],
            "messages":[{"role":"system","content":"be brief"},{"role":"user","content":"hello"},
            {"role":"assistant","content":"hi"},{"role":"tool","content":"result"},{"role":"user","content":[{"type":"text","text":"final question"}]}]}"#;
        let p = parse_request("/v1/chat/completions", body).unwrap();
        assert_eq!(p.model, "auto");
        assert!(p.tools_present && p.stream);
        assert_eq!(p.max_tokens, Some(100));
        assert_eq!(p.classify_input.user_text, "final question");
        assert_eq!(p.classify_input.history, vec!["hello"]);
        assert_eq!(p.classify_input.system_prompt.as_deref(), Some("be brief"));
        assert_eq!(p.classify_input.tool_outputs, vec!["result"]);
        assert!(p.estimated_input_tokens > 20);
    }

    #[test]
    fn anthropic_and_responses_and_completions() {
        let p = parse_request("/v1/messages", br#"{"model":"auto","system":"sys","max_tokens":5,"messages":[{"role":"user","content":[{"type":"text","text":"q"}]}]}"#).unwrap();
        assert_eq!(p.classify_input.system_prompt.as_deref(), Some("sys"));
        assert_eq!(p.classify_input.user_text, "q");
        let p = parse_request("/v1/responses", br#"{"model":"auto","input":[{"role":"developer","content":"d"},{"role":"user","content":"u"}]}"#).unwrap();
        assert_eq!(p.classify_input.user_text, "d\nu");
        let p = parse_request(
            "/v1/completions",
            br#"{"model":"auto","prompt":"complete me"}"#,
        )
        .unwrap();
        assert_eq!(p.classify_input.user_text, "complete me");
        let p = parse_request("/v1/embeddings", br#"{"model":"auto","input":["a","b"]}"#).unwrap();
        assert_eq!(p.classify_input.user_text, "a\nb");
    }

    #[test]
    fn errors() {
        assert!(matches!(
            parse_request("/", b"not json"),
            Err(ParseError::Json(_))
        ));
        assert!(matches!(
            parse_request("/", b"[1]"),
            Err(ParseError::NotObject)
        ));
        assert!(matches!(
            parse_request("/", br#"{"messages":[]}"#),
            Err(ParseError::NoModel)
        ));
        assert!(matches!(
            parse_request("/", br#"{"model":5}"#),
            Err(ParseError::NoModel)
        ));
    }
}
