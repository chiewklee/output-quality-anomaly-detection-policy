// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Threshold evaluation, action precedence and annotate / block body rewriting (spec Rule 6).

use serde_json::{json, Map, Value};

use crate::detect::{Category, Scores};
use crate::settings::{Action, CategoryRule, Settings};

pub const REPORT_FIELD: &str = "x_output_quality";
pub const WITHHELD_MESSAGE: &str =
    "This response was withheld because it did not meet output quality requirements.";

#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub flagged: Vec<Category>,
    pub action: Action,
    /// The category that decided a `block`, used as the error code.
    pub blocking: Option<Category>,
}

impl Verdict {
    pub fn is_flagged(&self) -> bool {
        !self.flagged.is_empty()
    }
}

fn rule(settings: &Settings, category: Category) -> CategoryRule {
    match category {
        Category::Hallucination => settings.hallucination,
        Category::Toxicity => settings.toxicity,
        Category::Bias => settings.bias,
        Category::Anomaly => settings.anomaly,
    }
}

pub fn decide(scores: &Scores, settings: &Settings) -> Verdict {
    let flagged: Vec<Category> = Category::ALL
        .iter()
        .copied()
        .filter(|c| {
            let score = scores.get(*c);
            score > 0.0 && score >= rule(settings, *c).threshold
        })
        .collect();
    let action = flagged
        .iter()
        .map(|c| rule(settings, *c).action)
        .max()
        .unwrap_or(Action::Monitor);
    let blocking = flagged
        .iter()
        .copied()
        .find(|c| rule(settings, *c).action == Action::Block);
    Verdict {
        flagged,
        action,
        blocking,
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

pub fn report(
    scores: &Scores,
    verdict: &Verdict,
    applied: Action,
    source: &str,
    judge_ms: Option<u128>,
) -> Value {
    let mut by_category = Map::new();
    for category in Category::ALL {
        by_category.insert(
            category.as_str().to_string(),
            json!(round2(scores.get(category))),
        );
    }
    json!({
        "flagged": verdict.flagged.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
        "action": applied.as_str(),
        "source": source,
        "judgeMs": judge_ms,
        "scores": by_category,
        "reasons": scores.reasons,
    })
}

/// Rewrites a buffered JSON body for `annotate` / `block`, or to attach the report to every
/// response when `always` (`reportAll`). `None` means deliver it unchanged.
pub fn rewrite(body: &[u8], verdict: &Verdict, report: Value, always: bool) -> Option<Vec<u8>> {
    if verdict.action == Action::Monitor && !always {
        return None;
    }
    let mut value: Value = serde_json::from_slice(body).ok()?;
    if crate::extract::a2a_payload(&value).is_some() {
        return rewrite_a2a(value, verdict, report);
    }
    let object = value.as_object_mut()?;

    if verdict.action == Action::Block {
        let withheld_choices = match object.get_mut("choices").and_then(Value::as_array_mut) {
            Some(choices) if !choices.is_empty() => {
                choices.iter_mut().for_each(withhold_choice);
                true
            }
            _ => false,
        };
        if !withheld_choices {
            let code = verdict.blocking.map_or("output_quality", Category::as_str);
            let envelope = json!({
                "error": {
                    "message": WITHHELD_MESSAGE,
                    "type": "output_quality_violation",
                    "code": code,
                    "param": null,
                },
                REPORT_FIELD: report,
            });
            return Some(envelope.to_string().into_bytes());
        }
    }

    object.insert(REPORT_FIELD.to_string(), report);
    serde_json::to_vec(&value).ok()
}

/// A2A: the report goes into the Task / Message `metadata` (A2A-native, so strict JSON-RPC
/// clients are unaffected); `block` replaces every text part with the withheld notice.
fn rewrite_a2a(mut value: Value, verdict: &Verdict, report: Value) -> Option<Vec<u8>> {
    let target = a2a_target_mut(&mut value)?;
    if verdict.action == Action::Block {
        withhold_a2a(target);
    }
    let object = target.as_object_mut()?;
    let metadata = object.entry("metadata").or_insert_with(|| json!({}));
    if !metadata.is_object() {
        *metadata = json!({});
    }
    if let Some(metadata) = metadata.as_object_mut() {
        metadata.insert(REPORT_FIELD.to_string(), report);
    }
    serde_json::to_vec(&value).ok()
}

fn a2a_target_mut(value: &mut Value) -> Option<&mut Value> {
    let root = if value.get("jsonrpc").is_some() {
        value.get_mut("result")?
    } else {
        value
    };
    let key = ["task", "message", "statusUpdate", "artifactUpdate"]
        .iter()
        .copied()
        .find(|key| root.get(*key).is_some_and(Value::is_object));
    match key {
        Some(key) => root.get_mut(key),
        None => Some(root),
    }
}

fn withhold_a2a(target: &mut Value) {
    if let Some(artifacts) = target.get_mut("artifacts").and_then(Value::as_array_mut) {
        artifacts
            .iter_mut()
            .for_each(|a| withhold_parts(a.get_mut("parts")));
    }
    if let Some(artifact) = target.get_mut("artifact") {
        withhold_parts(artifact.get_mut("parts"));
    }
    if let Some(message) = target.get_mut("status").and_then(|s| s.get_mut("message")) {
        withhold_parts(message.get_mut("parts"));
    }
    withhold_parts(target.get_mut("parts"));
}

fn withhold_parts(parts: Option<&mut Value>) {
    let Some(parts) = parts.and_then(Value::as_array_mut) else {
        return;
    };
    for part in parts.iter_mut().filter_map(Value::as_object_mut) {
        if part.contains_key("text") {
            part.insert("text".to_string(), json!(WITHHELD_MESSAGE));
        }
    }
}

fn withhold_choice(choice: &mut Value) {
    let Some(choice) = choice.as_object_mut() else {
        return;
    };
    if let Some(message) = choice.get_mut("message").and_then(Value::as_object_mut) {
        message.insert("content".to_string(), json!(WITHHELD_MESSAGE));
        message.remove("tool_calls");
        message.remove("function_call");
        message.remove("audio");
    }
    if choice.contains_key("text") {
        choice.insert("text".to_string(), json!(WITHHELD_MESSAGE));
    }
    choice.remove("logprobs");
    choice.insert("finish_reason".to_string(), json!("content_filter"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::TryFrom;

    fn settings(json: &str) -> Settings {
        Settings::try_from(json.as_bytes()).unwrap()
    }

    fn scores(category: Category, weight: f64) -> Scores {
        let mut s = Scores::default();
        s.add(category, weight, "test");
        s
    }

    #[test]
    fn test_decide_thresholds_and_precedence() {
        let cfg = settings(r#"{"toxicityAction":"block","biasAction":"annotate"}"#);
        assert!(!decide(&scores(Category::Toxicity, 0.49), &cfg).is_flagged());

        let mut s = scores(Category::Bias, 0.6);
        let v = decide(&s, &cfg);
        assert_eq!(v.flagged, vec![Category::Bias]);
        assert_eq!(v.action, Action::Annotate);
        assert_eq!(v.blocking, None);

        s.add(Category::Toxicity, 0.5, "x");
        let v = decide(&s, &cfg);
        assert_eq!(v.action, Action::Block);
        assert_eq!(v.blocking, Some(Category::Toxicity));
    }

    #[test]
    fn test_zero_threshold_does_not_flag_zero_score() {
        let cfg = settings(r#"{"anomalyThreshold":0}"#);
        assert!(!decide(&Scores::default(), &cfg).is_flagged());
    }

    #[test]
    fn test_monitor_never_rewrites() {
        let cfg = settings("{}");
        let s = scores(Category::Toxicity, 0.9);
        let v = decide(&s, &cfg);
        assert!(v.is_flagged());
        assert_eq!(
            rewrite(
                br#"{"choices":[]}"#,
                &v,
                report(&s, &v, v.action, "heuristic", None),
                false
            ),
            None
        );
    }

    #[test]
    fn test_report_all_annotates_unflagged_response() {
        let cfg = settings("{}");
        let s = Scores::default();
        let v = decide(&s, &cfg);
        let report = report(&s, &v, v.action, "llm", Some(840));
        let body = rewrite(
            br#"{"choices":[{"message":{"content":"ok"}}]}"#,
            &v,
            report,
            true,
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value[REPORT_FIELD]["flagged"], json!([]));
        assert_eq!(value[REPORT_FIELD]["action"], "monitor");
        assert_eq!(value[REPORT_FIELD]["judgeMs"], 840);
        assert_eq!(value["choices"][0]["message"]["content"], "ok");
    }

    #[test]
    fn test_annotate_adds_report() {
        let cfg = settings(r#"{"biasAction":"annotate"}"#);
        let s = scores(Category::Bias, 0.58);
        let v = decide(&s, &cfg);
        let body = rewrite(
            br#"{"id":"c","choices":[{"message":{"content":"x"}}]}"#,
            &v,
            report(&s, &v, v.action, "heuristic", None),
            false,
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["choices"][0]["message"]["content"], "x");
        assert_eq!(value[REPORT_FIELD]["flagged"], json!(["bias"]));
        assert_eq!(value[REPORT_FIELD]["action"], "annotate");
        assert_eq!(value[REPORT_FIELD]["scores"]["bias"], 0.58);
    }

    #[test]
    fn test_block_chat_completion_withholds_choices() {
        let cfg = settings(r#"{"toxicityAction":"block"}"#);
        let s = scores(Category::Toxicity, 0.9);
        let v = decide(&s, &cfg);
        let body = rewrite(
            br#"{"choices":[{"message":{"content":"bad","tool_calls":[{}]},"finish_reason":"stop","logprobs":{}}]}"#,
            &v,
            report(&s, &v, v.action, "heuristic", None), false
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        let choice = &value["choices"][0];
        assert_eq!(choice["message"]["content"], WITHHELD_MESSAGE);
        assert!(choice["message"].get("tool_calls").is_none());
        assert!(choice.get("logprobs").is_none());
        assert_eq!(choice["finish_reason"], "content_filter");
        assert_eq!(value[REPORT_FIELD]["action"], "block");
    }

    #[test]
    fn test_block_other_shape_uses_error_envelope() {
        let cfg = settings(r#"{"anomalyAction":"block"}"#);
        let s = scores(Category::Anomaly, 0.9);
        let v = decide(&s, &cfg);
        let body = rewrite(
            br#"{"type":"message","content":[]}"#,
            &v,
            report(&s, &v, v.action, "heuristic", None),
            false,
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["type"], "output_quality_violation");
        assert_eq!(value["error"]["code"], "anomaly");
        assert!(value.get("content").is_none());
    }

    const A2A_TASK: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"id":"t1","contextId":"c1","status":{"state":"TASK_STATE_COMPLETED"},"artifacts":[{"artifactId":"a1","parts":[{"text":"bad words"},{"data":{"x":1}}]}]}}"#;

    #[test]
    fn test_a2a_annotate_puts_report_in_task_metadata() {
        let cfg = settings(r#"{"biasAction":"annotate"}"#);
        let s = scores(Category::Bias, 0.8);
        let v = decide(&s, &cfg);
        let body = rewrite(
            A2A_TASK.as_bytes(),
            &v,
            report(&s, &v, v.action, "llm", None),
            false,
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            value.get(REPORT_FIELD).is_none(),
            "no extra top-level JSON-RPC member"
        );
        assert_eq!(
            value["result"]["metadata"][REPORT_FIELD]["flagged"],
            json!(["bias"])
        );
        assert_eq!(
            value["result"]["artifacts"][0]["parts"][0]["text"],
            "bad words"
        );
    }

    #[test]
    fn test_a2a_block_withholds_text_parts_only() {
        let cfg = settings(r#"{"toxicityAction":"block"}"#);
        let s = scores(Category::Toxicity, 0.9);
        let v = decide(&s, &cfg);
        let wrapped = r#"{"jsonrpc":"2.0","id":1,"result":{"task":{"id":"t1","status":{"state":"TASK_STATE_COMPLETED","message":{"parts":[{"text":"x"}]}},"artifacts":[{"parts":[{"text":"bad"}]}]}}}"#;
        let body = rewrite(
            wrapped.as_bytes(),
            &v,
            report(&s, &v, v.action, "llm", None),
            false,
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        let task = &value["result"]["task"];
        assert_eq!(task["artifacts"][0]["parts"][0]["text"], WITHHELD_MESSAGE);
        assert_eq!(
            task["status"]["message"]["parts"][0]["text"],
            WITHHELD_MESSAGE
        );
        assert_eq!(task["metadata"][REPORT_FIELD]["action"], "block");

        let body = rewrite(
            A2A_TASK.as_bytes(),
            &v,
            report(&s, &v, v.action, "llm", None),
            false,
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value["result"]["artifacts"][0]["parts"][1]["data"]["x"], 1,
            "non-text parts untouched"
        );
    }

    #[test]
    fn test_rewrite_ignores_non_object_bodies() {
        let cfg = settings(r#"{"anomalyAction":"annotate"}"#);
        let s = scores(Category::Anomaly, 0.9);
        let v = decide(&s, &cfg);
        assert_eq!(
            rewrite(
                b"[1,2]",
                &v,
                report(&s, &v, v.action, "heuristic", None),
                false
            ),
            None
        );
    }
}
