// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Extracts generated text, finish reasons and token logprobs from LLM response bodies
//! (OpenAI Chat Completions / Completions / Responses, Anthropic Messages) and SSE events.

use serde_json::Value;

/// The model output reduced to what the detectors need.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output {
    pub id: Option<String>,
    pub model: Option<String>,
    pub text: String,
    pub has_tool_calls: bool,
    pub finish_reasons: Vec<String>,
    pub logprob_sum: f64,
    pub logprob_count: usize,
    /// True when text beyond the inspection limit was dropped.
    pub truncated: bool,
}

impl Output {
    pub fn mean_logprob(&self) -> Option<f64> {
        if self.logprob_count == 0 {
            None
        } else {
            Some(self.logprob_sum / self.logprob_count as f64)
        }
    }

    fn push_text(&mut self, text: &str, limit: usize) {
        if self.text.len() >= limit {
            self.truncated |= !text.is_empty();
            return;
        }
        let room = limit - self.text.len();
        if text.len() <= room {
            self.text.push_str(text);
        } else {
            let mut cut = room;
            while cut > 0 && !text.is_char_boundary(cut) {
                cut -= 1;
            }
            self.text.push_str(text.get(..cut).unwrap_or_default());
            self.truncated = true;
        }
    }

    fn push_finish(&mut self, reason: &str) {
        let normalized = match reason {
            "max_tokens" | "max_output_tokens" => "length",
            "end_turn" | "stop_sequence" => "stop",
            "tool_use" => "tool_calls",
            "TASK_STATE_COMPLETED"
            | "completed"
            | "TASK_STATE_INPUT_REQUIRED"
            | "input-required" => "stop",
            "TASK_STATE_FAILED" | "failed" | "TASK_STATE_REJECTED" | "rejected" => "failed",
            other => other,
        };
        if !self.finish_reasons.iter().any(|r| r == normalized) {
            self.finish_reasons.push(normalized.to_string());
        }
    }

    fn push_logprobs(&mut self, logprobs: &Value) {
        // Chat Completions / Responses: [{"logprob": f64, ...}]
        let entries = logprobs
            .get("content")
            .and_then(Value::as_array)
            .or_else(|| logprobs.as_array());
        if let Some(entries) = entries {
            for entry in entries {
                if let Some(lp) = entry.get("logprob").and_then(Value::as_f64) {
                    self.logprob_sum += lp;
                    self.logprob_count += 1;
                }
            }
        }
        // Legacy Completions: {"token_logprobs": [f64 | null]}
        if let Some(tokens) = logprobs.get("token_logprobs").and_then(Value::as_array) {
            for lp in tokens.iter().filter_map(Value::as_f64) {
                self.logprob_sum += lp;
                self.logprob_count += 1;
            }
        }
    }

    fn capture_identity(&mut self, value: &Value) {
        if self.id.is_none() {
            self.id = value.get("id").and_then(Value::as_str).map(str::to_string);
        }
        if self.model.is_none() {
            self.model = value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }
}

/// Parses a buffered JSON response. Returns `None` when the body is not a recognised LLM shape.
pub fn from_json(body: &[u8], limit: usize) -> Option<Output> {
    let value: Value = serde_json::from_slice(body).ok()?;
    from_value(&value, limit)
}

pub fn from_value(value: &Value, limit: usize) -> Option<Output> {
    if let Some(payload) = a2a_payload(value) {
        let mut out = Output::default();
        return push_a2a(&mut out, payload, limit).then_some(out);
    }
    let mut out = Output::default();
    out.capture_identity(value);
    let mut recognised = false;

    // OpenAI Chat Completions / legacy Completions.
    if let Some(choices) = value.get("choices").and_then(Value::as_array) {
        recognised = true;
        for (i, choice) in choices.iter().enumerate() {
            if i > 0 {
                out.push_text("\n\n", limit);
            }
            if let Some(message) = choice.get("message") {
                push_content(&mut out, message.get("content"), limit);
                if message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|calls| !calls.is_empty())
                    || message.get("function_call").is_some_and(|f| !f.is_null())
                {
                    out.has_tool_calls = true;
                }
            }
            if let Some(text) = choice.get("text").and_then(Value::as_str) {
                out.push_text(text, limit);
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                out.push_finish(reason);
            }
            if let Some(logprobs) = choice.get("logprobs").filter(|l| !l.is_null()) {
                out.push_logprobs(logprobs);
            }
        }
    }

    // OpenAI Responses API.
    if let Some(items) = value.get("output").and_then(Value::as_array) {
        recognised = true;
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    if let Some(parts) = item.get("content").and_then(Value::as_array) {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                out.push_text(text, limit);
                            }
                            if let Some(logprobs) = part.get("logprobs") {
                                out.push_logprobs(logprobs);
                            }
                        }
                    }
                }
                Some("function_call") | Some("tool_call") => out.has_tool_calls = true,
                _ => {}
            }
        }
        if let Some(reason) = value
            .get("incomplete_details")
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str)
        {
            out.push_finish(reason);
        }
    }

    // Anthropic Messages.
    if value.get("type").and_then(Value::as_str) == Some("message") {
        if let Some(parts) = value.get("content").and_then(Value::as_array) {
            recognised = true;
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            out.push_text(text, limit);
                        }
                    }
                    Some("tool_use") => out.has_tool_calls = true,
                    _ => {}
                }
            }
        }
        if let Some(reason) = value.get("stop_reason").and_then(Value::as_str) {
            out.push_finish(reason);
        }
    }

    recognised.then_some(out)
}

fn push_content(out: &mut Output, content: Option<&Value>, limit: usize) {
    match content {
        Some(Value::String(text)) => out.push_text(text, limit),
        Some(Value::Array(parts)) => {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    out.push_text(text, limit);
                }
            }
        }
        _ => {}
    }
}

// ---- A2A (Agent-to-Agent) -------------------------------------------------------------------

/// A2A methods whose replies carry agent-authored content (Legacy and v1 vocabularies).
const A2A_SEND_METHODS: &[&str] = &[
    "message/send",
    "message/stream",
    "SendMessage",
    "SendStreamingMessage",
];

/// The A2A object to read from a response body or SSE event: the JSON-RPC `result`, or the bare
/// v1 HTTP+JSON body, unwrapped from `task` / `message` / `statusUpdate` / `artifactUpdate`.
/// `None` when the value is not an A2A payload.
pub fn a2a_payload(value: &Value) -> Option<&Value> {
    let root = if value.get("jsonrpc").is_some() {
        value.get("result")?
    } else if [
        "task",
        "message",
        "statusUpdate",
        "artifactUpdate",
        "artifacts",
    ]
    .iter()
    .any(|key| {
        value
            .get(*key)
            .is_some_and(|v| v.is_object() || v.is_array())
    }) && value.get("choices").is_none()
        && value.get("output").is_none()
    {
        value
    } else {
        return None;
    };
    Some(unwrap_a2a(root))
}

fn unwrap_a2a(value: &Value) -> &Value {
    for key in [
        "task",
        "message",
        "statusUpdate",
        "artifactUpdate",
        "status_update",
        "artifact_update",
    ] {
        if let Some(inner) = value.get(key).filter(|v| v.is_object()) {
            return inner;
        }
    }
    value
}

/// Collects text from a Task, Message, or streaming update. Returns whether anything A2A-shaped
/// (parts, artifacts, status) was present.
fn push_a2a(out: &mut Output, payload: &Value, limit: usize) -> bool {
    if out.id.is_none() {
        out.id = ["id", "taskId", "messageId"]
            .iter()
            .find_map(|key| payload.get(*key).and_then(Value::as_str))
            .map(str::to_string);
    }
    let mut recognised = false;
    let before = out.text.len();
    if let Some(artifacts) = payload.get("artifacts").and_then(Value::as_array) {
        recognised = true;
        for artifact in artifacts {
            push_parts(out, artifact.get("parts"), limit);
        }
    }
    if let Some(artifact) = payload.get("artifact") {
        recognised = true;
        push_parts(out, artifact.get("parts"), limit);
    }
    let has_artifact_text = out.text.len() > before;
    if let Some(status) = payload.get("status").filter(|s| s.is_object()) {
        recognised = true;
        // Artifacts are the task's output; the status message only counts when there are none
        // (e.g. an input-required prompt or a failure explanation).
        if let Some(message) = status.get("message").filter(|_| !has_artifact_text) {
            push_parts(out, message.get("parts"), limit);
        }
        if let Some(state) = status.get("state").and_then(Value::as_str) {
            out.push_finish(state);
        }
    }
    if payload.get("parts").is_some_and(Value::is_array) {
        recognised = true;
        push_parts(out, payload.get("parts"), limit);
    }
    recognised
}

/// Appends the text of each text part (`kind`/`type` = "text", or a bare v1 `{ "text": … }`).
fn push_parts(out: &mut Output, parts: Option<&Value>, limit: usize) {
    let Some(parts) = parts.and_then(Value::as_array) else {
        return;
    };
    for part in parts {
        let kind = part
            .get("kind")
            .or_else(|| part.get("type"))
            .and_then(Value::as_str);
        if !matches!(kind, None | Some("text")) {
            continue;
        }
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            if !out.text.is_empty() {
                out.push_text("\n", limit);
            }
            out.push_text(text, limit);
        }
    }
}

/// What the request filter learns from an A2A request.
#[derive(Clone, Debug, PartialEq)]
pub struct A2aRequest {
    /// The method returns agent-authored content worth scoring.
    pub inspect: bool,
    /// Text of the user's message, for the judge's context.
    pub text: String,
}

/// Classifies an A2A request: a JSON-RPC envelope (Legacy or v1) or a v1 HTTP+JSON
/// `…/message:send|stream` call. `None` when the request is not A2A (e.g. an LLM API call).
pub fn a2a_request(body: &Value, path: &str) -> Option<A2aRequest> {
    let (inspect, message) = if let Some(method) = body.get("method").and_then(Value::as_str) {
        body.get("jsonrpc")?;
        (
            A2A_SEND_METHODS.contains(&method),
            body.get("params").and_then(|p| p.get("message")),
        )
    } else {
        let bare = path.split('?').next().unwrap_or_default();
        if !(bare.ends_with("/message:send") || bare.ends_with("/message:stream")) {
            return None;
        }
        (true, body.get("message"))
    };
    let mut out = Output::default();
    if let Some(message) = message {
        push_parts(&mut out, message.get("parts"), usize::MAX);
    }
    Some(A2aRequest {
        inspect,
        text: out.text,
    })
}

/// Builds a compact transcript of the request conversation (system prompt first, then the most
/// recent turns) for the judge, at most `budget` characters. `None` for unrecognised bodies.
pub fn prompt_from_request(body: &[u8], budget: usize) -> Option<String> {
    if budget == 0 {
        return None;
    }
    let value: Value = serde_json::from_slice(body).ok()?;
    let mut system: Vec<String> = Vec::new();
    let mut turns: Vec<String> = Vec::new();

    // Anthropic `system`, Responses `instructions`.
    if let Some(text) = value
        .get("system")
        .map(content_text)
        .filter(|t| !t.is_empty())
    {
        system.push(text);
    }
    if let Some(text) = value.get("instructions").and_then(Value::as_str) {
        system.push(text.to_string());
    }
    // Chat Completions / Anthropic `messages`.
    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        for message in messages {
            push_turn(message, &mut system, &mut turns);
        }
    }
    // Responses API `input`.
    match value.get("input") {
        Some(Value::String(text)) => turns.push(format!("USER: {text}")),
        Some(Value::Array(items)) => {
            for item in items {
                match item {
                    Value::String(text) => turns.push(format!("USER: {text}")),
                    _ => push_turn(item, &mut system, &mut turns),
                }
            }
        }
        _ => {}
    }
    // Legacy Completions `prompt`.
    match value.get("prompt") {
        Some(Value::String(text)) => turns.push(format!("USER: {text}")),
        Some(Value::Array(items)) => turns.extend(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|t| format!("USER: {t}")),
        ),
        _ => {}
    }

    if system.is_empty() && turns.is_empty() {
        return None;
    }
    let system = system.join("\n");
    let system_part = if system.is_empty() {
        String::new()
    } else {
        format!("SYSTEM: {}\n", head_chars(&system, budget / 4))
    };
    let remaining = budget.saturating_sub(system_part.chars().count());
    Some(format!(
        "{system_part}{}",
        tail_chars(&turns.join("\n"), remaining)
    ))
}

fn push_turn(message: &Value, system: &mut Vec<String>, turns: &mut Vec<String>) {
    let text = message.get("content").map(content_text).unwrap_or_default();
    if text.is_empty() {
        return;
    }
    match message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user")
    {
        "system" | "developer" => system.push(text),
        role => turns.push(format!("{}: {text}", role.to_uppercase())),
    }
}

/// Text of a message `content`: a string, or the `text` of each part in an array.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn head_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut head: String = text.chars().take(max.saturating_sub(1)).collect();
    head.push('…');
    head
}

pub fn tail_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let mut tail = String::from("…");
    tail.extend(text.chars().skip(count - max.saturating_sub(1)));
    tail
}

/// Accumulates an `Output` from streamed SSE `data:` payloads.
#[derive(Debug)]
pub struct StreamAccumulator {
    output: Output,
    limit: usize,
    recognised: bool,
}

impl StreamAccumulator {
    pub fn new(limit: usize) -> Self {
        StreamAccumulator {
            output: Output::default(),
            limit,
            recognised: false,
        }
    }

    /// Feeds one event payload. Non-JSON payloads such as `[DONE]` are ignored.
    pub fn push_event(&mut self, data: &str) {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return;
        };
        if let Some(payload) = a2a_payload(&value) {
            self.recognised |= push_a2a(&mut self.output, payload, self.limit);
            return;
        }
        let out = &mut self.output;
        out.capture_identity(&value);
        // Responses API and Anthropic wrap identity inside the first event.
        if let Some(inner) = value.get("response").or_else(|| value.get("message")) {
            out.capture_identity(inner);
        }

        if let Some(choices) = value.get("choices").and_then(Value::as_array) {
            self.recognised = true;
            for choice in choices {
                if let Some(delta) = choice.get("delta") {
                    push_content(out, delta.get("content"), self.limit);
                    if delta.get("tool_calls").is_some_and(|t| !t.is_null()) {
                        out.has_tool_calls = true;
                    }
                }
                if let Some(text) = choice.get("text").and_then(Value::as_str) {
                    out.push_text(text, self.limit);
                }
                if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                    out.push_finish(reason);
                }
                if let Some(logprobs) = choice.get("logprobs").filter(|l| !l.is_null()) {
                    out.push_logprobs(logprobs);
                }
            }
            return;
        }

        match value.get("type").and_then(Value::as_str) {
            // OpenAI Responses API stream.
            Some("response.output_text.delta") => {
                self.recognised = true;
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    out.push_text(delta, self.limit);
                }
                if let Some(logprobs) = value.get("logprobs") {
                    out.push_logprobs(logprobs);
                }
            }
            Some("response.function_call_arguments.delta") => {
                self.recognised = true;
                out.has_tool_calls = true;
            }
            Some("response.incomplete") => {
                if let Some(reason) = value
                    .get("response")
                    .and_then(|r| r.get("incomplete_details"))
                    .and_then(|d| d.get("reason"))
                    .and_then(Value::as_str)
                {
                    out.push_finish(reason);
                }
            }
            // Anthropic Messages stream.
            Some("content_block_delta") => {
                self.recognised = true;
                if let Some(text) = value
                    .get("delta")
                    .and_then(|d| d.get("text"))
                    .and_then(Value::as_str)
                {
                    out.push_text(text, self.limit);
                }
            }
            Some("content_block_start") => {
                if value
                    .get("content_block")
                    .and_then(|b| b.get("type"))
                    .and_then(Value::as_str)
                    == Some("tool_use")
                {
                    self.recognised = true;
                    out.has_tool_calls = true;
                }
            }
            Some("message_delta") => {
                if let Some(reason) = value
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    out.push_finish(reason);
                }
            }
            _ => {}
        }
    }

    /// Returns the accumulated output, or `None` when no recognised event was seen.
    pub fn finish(self) -> Option<Output> {
        self.recognised.then_some(self.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: usize = 1 << 16;

    #[test]
    fn test_chat_completion_text_finish_and_logprobs() {
        let body = br#"{"id":"chatcmpl-1","model":"gpt-x","choices":[{"message":{"role":"assistant","content":"Hello"},"finish_reason":"stop","logprobs":{"content":[{"token":"Hel","logprob":-0.5},{"token":"lo","logprob":-1.5}]}}]}"#;
        let out = from_json(body, LIMIT).unwrap();
        assert_eq!(out.id.as_deref(), Some("chatcmpl-1"));
        assert_eq!(out.model.as_deref(), Some("gpt-x"));
        assert_eq!(out.text, "Hello");
        assert_eq!(out.finish_reasons, vec!["stop"]);
        assert_eq!(out.mean_logprob(), Some(-1.0));
    }

    #[test]
    fn test_chat_completion_content_parts_and_tool_calls() {
        let body = br#"{"choices":[{"message":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}],"tool_calls":[{"id":"t"}]},"finish_reason":"tool_calls"}]}"#;
        let out = from_json(body, LIMIT).unwrap();
        assert_eq!(out.text, "ab");
        assert!(out.has_tool_calls);
    }

    #[test]
    fn test_multiple_choices_are_joined() {
        let body = br#"{"choices":[{"message":{"content":"one"}},{"message":{"content":"two"}}]}"#;
        assert_eq!(from_json(body, LIMIT).unwrap().text, "one\n\ntwo");
    }

    #[test]
    fn test_legacy_completion() {
        let body = br#"{"choices":[{"text":"hi","finish_reason":"length","logprobs":{"token_logprobs":[null,-2.0]}}]}"#;
        let out = from_json(body, LIMIT).unwrap();
        assert_eq!(out.text, "hi");
        assert_eq!(out.finish_reasons, vec!["length"]);
        assert_eq!(out.mean_logprob(), Some(-2.0));
    }

    #[test]
    fn test_responses_api() {
        let body = br#"{"id":"resp_1","output":[{"type":"message","content":[{"type":"output_text","text":"Hi there"}]}],"incomplete_details":{"reason":"max_output_tokens"}}"#;
        let out = from_json(body, LIMIT).unwrap();
        assert_eq!(out.text, "Hi there");
        assert_eq!(out.finish_reasons, vec!["length"]);
    }

    #[test]
    fn test_anthropic_messages() {
        let body = br#"{"id":"msg_1","type":"message","content":[{"type":"text","text":"Bonjour"}],"stop_reason":"max_tokens"}"#;
        let out = from_json(body, LIMIT).unwrap();
        assert_eq!(out.text, "Bonjour");
        assert_eq!(out.finish_reasons, vec!["length"]);
    }

    #[test]
    fn test_unrecognised_json_returns_none() {
        assert!(from_json(br#"{"hello":"world"}"#, LIMIT).is_none());
        assert!(from_json(b"not json", LIMIT).is_none());
    }

    #[test]
    fn test_text_is_capped_on_char_boundary() {
        let body = r#"{"choices":[{"message":{"content":"ééééé"}}]}"#;
        let out = from_json(body.as_bytes(), 5).unwrap();
        assert_eq!(out.text, "éé");
        assert!(out.truncated);
    }

    #[test]
    fn test_stream_openai_chat_chunks() {
        let mut acc = StreamAccumulator::new(LIMIT);
        acc.push_event(
            r#"{"id":"c1","model":"m","choices":[{"delta":{"role":"assistant","content":"Hel"}}]}"#,
        );
        acc.push_event(r#"{"id":"c1","choices":[{"delta":{"content":"lo"},"logprobs":{"content":[{"logprob":-3.0}]}}]}"#);
        acc.push_event(r#"{"id":"c1","choices":[{"delta":{},"finish_reason":"stop"}]}"#);
        acc.push_event("[DONE]");
        let out = acc.finish().unwrap();
        assert_eq!(out.id.as_deref(), Some("c1"));
        assert_eq!(out.text, "Hello");
        assert_eq!(out.finish_reasons, vec!["stop"]);
        assert_eq!(out.mean_logprob(), Some(-3.0));
    }

    #[test]
    fn test_stream_anthropic_and_responses() {
        let mut acc = StreamAccumulator::new(LIMIT);
        acc.push_event(r#"{"type":"message_start","message":{"id":"msg_1","model":"claude"}}"#);
        acc.push_event(
            r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}"#,
        );
        acc.push_event(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#);
        let out = acc.finish().unwrap();
        assert_eq!(out.id.as_deref(), Some("msg_1"));
        assert_eq!(out.text, "Hi");
        assert_eq!(out.finish_reasons, vec!["stop"]);

        let mut acc = StreamAccumulator::new(LIMIT);
        acc.push_event(r#"{"type":"response.created","response":{"id":"resp_9"}}"#);
        acc.push_event(r#"{"type":"response.output_text.delta","delta":"Yo"}"#);
        let out = acc.finish().unwrap();
        assert_eq!(out.id.as_deref(), Some("resp_9"));
        assert_eq!(out.text, "Yo");
    }

    #[test]
    fn test_stream_without_recognised_events_returns_none() {
        let mut acc = StreamAccumulator::new(LIMIT);
        acc.push_event(r#"{"ping":true}"#);
        assert!(acc.finish().is_none());
    }

    #[test]
    fn test_prompt_from_chat_request() {
        let body = br#"{"model":"m","messages":[{"role":"system","content":"Be concise."},{"role":"user","content":"Capital of France?"},{"role":"assistant","content":"Paris."},{"role":"user","content":[{"type":"text","text":"And Spain?"}]}]}"#;
        assert_eq!(
            prompt_from_request(body, 1000).unwrap(),
            "SYSTEM: Be concise.\nUSER: Capital of France?\nASSISTANT: Paris.\nUSER: And Spain?"
        );
    }

    #[test]
    fn test_prompt_from_responses_anthropic_and_legacy_requests() {
        let responses = br#"{"instructions":"Cite sources.","input":"Who wrote Hamlet?"}"#;
        assert_eq!(
            prompt_from_request(responses, 1000).unwrap(),
            "SYSTEM: Cite sources.\nUSER: Who wrote Hamlet?"
        );
        let anthropic = br#"{"system":[{"type":"text","text":"You are a tutor."}],"messages":[{"role":"user","content":"2+2?"}]}"#;
        assert_eq!(
            prompt_from_request(anthropic, 1000).unwrap(),
            "SYSTEM: You are a tutor.\nUSER: 2+2?"
        );
        assert_eq!(
            prompt_from_request(br#"{"prompt":"Say hi"}"#, 1000).unwrap(),
            "USER: Say hi"
        );
    }

    #[test]
    fn test_prompt_budget_keeps_system_head_and_latest_turns() {
        let body = format!(
            r#"{{"messages":[{{"role":"system","content":"{}"}},{{"role":"user","content":"{}LATEST"}}]}}"#,
            "S".repeat(100),
            "u".repeat(100)
        );
        let prompt = prompt_from_request(body.as_bytes(), 40).unwrap();
        assert!(prompt.chars().count() <= 40, "{}", prompt);
        assert!(prompt.starts_with("SYSTEM: S"));
        assert!(prompt.ends_with("LATEST"));
    }

    #[test]
    fn test_prompt_absent_for_zero_budget_or_unknown_body() {
        assert!(
            prompt_from_request(br#"{"messages":[{"role":"user","content":"x"}]}"#, 0).is_none()
        );
        assert!(prompt_from_request(br#"{"foo":1}"#, 100).is_none());
        assert!(prompt_from_request(b"not json", 100).is_none());
    }

    #[test]
    fn test_a2a_v1_task_result() {
        let body = br#"{"jsonrpc":"2.0","id":1,"result":{"id":"task-1","contextId":"c","status":{"state":"TASK_STATE_COMPLETED"},"artifacts":[{"artifactId":"a","parts":[{"text":"Hello"},{"text":"world"}]}]}}"#;
        let out = from_json(body, LIMIT).unwrap();
        assert_eq!(out.id.as_deref(), Some("task-1"));
        assert_eq!(out.text, "Hello\nworld");
        assert_eq!(out.finish_reasons, vec!["stop"]);
    }

    #[test]
    fn test_a2a_wrapped_task_legacy_message_and_failure() {
        let wrapped = br#"{"jsonrpc":"2.0","id":1,"result":{"task":{"id":"t","status":{"state":"TASK_STATE_FAILED","message":{"role":"ROLE_AGENT","parts":[{"text":"Oops"}]}}}}}"#;
        let out = from_json(wrapped, LIMIT).unwrap();
        assert_eq!(out.text, "Oops");
        assert_eq!(out.finish_reasons, vec!["failed"]);

        let legacy = br#"{"jsonrpc":"2.0","id":"x","result":{"kind":"message","messageId":"m1","role":"agent","parts":[{"kind":"text","text":"Hi"},{"kind":"file","file":{}}]}}"#;
        let out = from_json(legacy, LIMIT).unwrap();
        assert_eq!(out.id.as_deref(), Some("m1"));
        assert_eq!(out.text, "Hi");
    }

    #[test]
    fn test_a2a_errors_cards_and_http_json() {
        assert!(from_json(
            br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no"}}"#,
            LIMIT
        )
        .is_none());
        assert!(from_json(
            br#"{"name":"Agent","skills":[],"supportedInterfaces":[]}"#,
            LIMIT
        )
        .is_none());
        let bare = br#"{"task":{"id":"t","artifacts":[{"parts":[{"text":"rest"}]}]}}"#;
        assert_eq!(from_json(bare, LIMIT).unwrap().text, "rest");
    }

    #[test]
    fn test_a2a_stream_events() {
        let mut acc = StreamAccumulator::new(LIMIT);
        acc.push_event(r#"{"jsonrpc":"2.0","id":1,"result":{"statusUpdate":{"taskId":"t9","status":{"state":"TASK_STATE_WORKING"}}}}"#);
        acc.push_event(r#"{"jsonrpc":"2.0","id":1,"result":{"artifactUpdate":{"taskId":"t9","artifact":{"parts":[{"text":"chunk one"}]}}}}"#);
        acc.push_event(r#"{"jsonrpc":"2.0","id":1,"result":{"kind":"artifact-update","taskId":"t9","artifact":{"parts":[{"kind":"text","text":"chunk two"}]}}}"#);
        acc.push_event(r#"{"jsonrpc":"2.0","id":1,"result":{"statusUpdate":{"taskId":"t9","status":{"state":"TASK_STATE_COMPLETED"}}}}"#);
        let out = acc.finish().unwrap();
        assert_eq!(out.id.as_deref(), Some("t9"));
        assert_eq!(out.text, "chunk one\nchunk two");
        assert!(out.finish_reasons.contains(&"stop".to_string()));
    }

    #[test]
    fn test_a2a_request_classification() {
        let v1: Value = serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"message":{"role":"ROLE_USER","parts":[{"text":"Who invented the phone?"}]}}}"#).unwrap();
        assert_eq!(
            a2a_request(&v1, "/agent/jsonrpc"),
            Some(A2aRequest {
                inspect: true,
                text: "Who invented the phone?".into()
            })
        );
        let legacy: Value = serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"message/send","params":{"message":{"parts":[{"kind":"text","text":"hi"}]}}}"#).unwrap();
        assert!(a2a_request(&legacy, "/").unwrap().inspect);
        let get_task: Value = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":2,"method":"GetTask","params":{"id":"t"}}"#,
        )
        .unwrap();
        assert!(!a2a_request(&get_task, "/").unwrap().inspect);
        let rest: Value =
            serde_json::from_str(r#"{"message":{"parts":[{"text":"rest call"}]}}"#).unwrap();
        assert_eq!(
            a2a_request(&rest, "/agent/rest/message:send?x=1")
                .unwrap()
                .text,
            "rest call"
        );
        let chat: Value = serde_json::from_str(r#"{"model":"m","messages":[]}"#).unwrap();
        assert!(a2a_request(&chat, "/v1/chat/completions").is_none());
    }
    #[test]
    fn test_a2a_status_message_ignored_when_artifacts_answer() {
        let body = br#"{"jsonrpc":"2.0","id":1,"result":{"task":{"id":"t","status":{"state":"TASK_STATE_COMPLETED","message":{"parts":[{"text":"Answer ready."}]}},"artifacts":[{"parts":[{"text":"The answer."}]}]}}}"#;
        assert_eq!(from_json(body, LIMIT).unwrap().text, "The answer.");
    }
}
