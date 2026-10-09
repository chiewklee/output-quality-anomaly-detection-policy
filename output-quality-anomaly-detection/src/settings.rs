// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Resolves the generated `Config` into validated runtime `Settings` with defaults applied.

use std::convert::TryFrom;
use std::time::Duration;

use anyhow::{anyhow, Result};
use pdk::hl::Service;

use crate::generated::config::Config;

pub const DEFAULT_JUDGE_MODEL: &str = "gpt-5.4-mini";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Action {
    Monitor,
    Annotate,
    Block,
}

impl Action {
    fn parse(value: Option<&str>, property: &str) -> Result<Self> {
        match value.unwrap_or("monitor") {
            "monitor" => Ok(Action::Monitor),
            "annotate" => Ok(Action::Annotate),
            "block" => Ok(Action::Block),
            other => Err(anyhow!("{property}: unsupported action '{other}'")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Action::Monitor => "monitor",
            Action::Annotate => "annotate",
            Action::Block => "block",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JudgeMode {
    OnSuspicion,
    Always,
}

#[derive(Clone, Copy, Debug)]
pub struct CategoryRule {
    pub threshold: f64,
    pub action: Action,
}

#[derive(Clone, Debug)]
pub struct FeedbackSettings {
    pub path: String,
    pub window_seconds: u64,
    pub min_samples: u64,
    pub negative_rate_threshold: f64,
}

#[derive(Clone, Debug)]
pub struct JudgeSettings {
    pub mode: JudgeMode,
    pub service: Service,
    pub path: String,
    pub model: String,
    pub api_key: Option<String>,
    pub instructions: Option<String>,
    pub context_chars: usize,
    pub json_mode: bool,
    pub suspicion_score: f64,
    pub timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub hallucination: CategoryRule,
    pub toxicity: CategoryRule,
    pub bias: CategoryRule,
    pub anomaly: CategoryRule,
    pub toxicity_terms: Vec<String>,
    pub bias_group_terms: Vec<String>,
    pub low_confidence_logprob: f64,
    pub max_inspected_bytes: usize,
    pub baseline_warmup_samples: u64,
    pub length_z_score_threshold: f64,
    pub feedback: Option<FeedbackSettings>,
    pub judge: Option<JudgeSettings>,
    /// Set when `judgeMode` asked for a judge but no `judgeService` was configured.
    pub judge_misconfigured: bool,
    pub strip_accept_encoding: bool,
    pub report_all: bool,
}

impl Settings {
    /// True when a buffered response body may be rewritten (an action or `reportAll`).
    pub fn may_rewrite(&self) -> bool {
        self.report_all
            || [self.hallucination, self.toxicity, self.bias, self.anomaly]
                .iter()
                .any(|rule| rule.action != Action::Monitor)
    }
}

impl TryFrom<&[u8]> for Settings {
    type Error = anyhow::Error;

    fn try_from(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Err(anyhow!("Configuration was empty"));
        }
        let config: Config = serde_json::from_slice(bytes)
            .map_err(|err| anyhow!("Configuration could not be parsed: {err}"))?;
        Settings::try_from(config)
    }
}

impl TryFrom<Config> for Settings {
    type Error = anyhow::Error;

    fn try_from(config: Config) -> Result<Self> {
        let rule = |threshold: Option<f64>, default: f64, action: Option<&str>, name: &str| {
            Ok::<_, anyhow::Error>(CategoryRule {
                threshold: unit_interval(
                    threshold.unwrap_or(default),
                    &format!("{name}Threshold"),
                )?,
                action: Action::parse(action, &format!("{name}Action"))?,
            })
        };

        let feedback_path = config
            .feedback_path
            .clone()
            .unwrap_or_else(|| "/quality-feedback".to_string());
        let feedback = if feedback_path.trim().is_empty() {
            None
        } else {
            Some(FeedbackSettings {
                path: feedback_path.trim().to_string(),
                window_seconds: at_least(
                    config.feedback_window_seconds.unwrap_or(300),
                    10,
                    "feedbackWindowSeconds",
                )?,
                min_samples: at_least(
                    config.feedback_min_samples.unwrap_or(10),
                    1,
                    "feedbackMinSamples",
                )?,
                negative_rate_threshold: unit_interval(
                    config.feedback_negative_rate_threshold.unwrap_or(0.3),
                    "feedbackNegativeRateThreshold",
                )?,
            })
        };

        // `always` is the default, but only an explicitly chosen mode warns about a missing service.
        let explicit_mode = config.judge_mode.is_some();
        let judge_mode = match config.judge_mode.as_deref().unwrap_or("always") {
            "off" => None,
            "onSuspicion" => Some(JudgeMode::OnSuspicion),
            "always" => Some(JudgeMode::Always),
            other => return Err(anyhow!("judgeMode: unsupported value '{other}'")),
        };
        let judge_misconfigured =
            explicit_mode && judge_mode.is_some() && config.judge_service.is_none();
        let judge = match (judge_mode, config.judge_service.clone()) {
            (Some(mode), Some(service)) => Some(JudgeSettings {
                mode,
                service,
                path: config
                    .judge_path
                    .clone()
                    .unwrap_or_else(|| "/v1/chat/completions".to_string()),
                model: config
                    .judge_model
                    .clone()
                    .map(|model| model.trim().to_string())
                    .filter(|model| !model.is_empty())
                    .unwrap_or_else(|| DEFAULT_JUDGE_MODEL.to_string()),
                api_key: config.judge_api_key.clone().filter(|key| !key.is_empty()),
                instructions: config
                    .judge_instructions
                    .clone()
                    .map(|text| text.trim().to_string())
                    .filter(|text| !text.is_empty()),
                context_chars: at_least(
                    config.judge_context_chars.unwrap_or(8000),
                    0,
                    "judgeContextChars",
                )?
                .min(100_000) as usize,
                json_mode: config.judge_json_mode.unwrap_or(true),
                suspicion_score: unit_interval(
                    config.judge_suspicion_score.unwrap_or(0.3),
                    "judgeSuspicionScore",
                )?,
                timeout: Duration::from_millis(
                    at_least(
                        config.judge_timeout_ms.unwrap_or(5000),
                        100,
                        "judgeTimeoutMs",
                    )?
                    .min(30_000),
                ),
            }),
            _ => None,
        };

        let low_confidence_logprob = config.low_confidence_logprob.unwrap_or(-1.0);
        if low_confidence_logprob > 0.0 {
            return Err(anyhow!("lowConfidenceLogprob must be <= 0"));
        }
        let length_z_score_threshold = config.length_z_score_threshold.unwrap_or(3.0);
        if length_z_score_threshold < 1.0 {
            return Err(anyhow!("lengthZScoreThreshold must be >= 1"));
        }

        Ok(Settings {
            hallucination: rule(
                config.hallucination_threshold,
                0.6,
                config.hallucination_action.as_deref(),
                "hallucination",
            )?,
            toxicity: rule(
                config.toxicity_threshold,
                0.5,
                config.toxicity_action.as_deref(),
                "toxicity",
            )?,
            bias: rule(
                config.bias_threshold,
                0.5,
                config.bias_action.as_deref(),
                "bias",
            )?,
            anomaly: rule(
                config.anomaly_threshold,
                0.7,
                config.anomaly_action.as_deref(),
                "anomaly",
            )?,
            toxicity_terms: normalize_terms(config.toxicity_terms.unwrap_or_default()),
            bias_group_terms: normalize_terms(config.bias_group_terms.unwrap_or_default()),
            low_confidence_logprob,
            max_inspected_bytes: at_least(
                config.max_inspected_bytes.unwrap_or(262_144),
                1024,
                "maxInspectedBytes",
            )?
            .min(10 * 1024 * 1024) as usize,
            baseline_warmup_samples: at_least(
                config.baseline_warmup_samples.unwrap_or(30),
                1,
                "baselineWarmupSamples",
            )?,
            length_z_score_threshold,
            feedback,
            judge,
            judge_misconfigured,
            strip_accept_encoding: config.strip_accept_encoding.unwrap_or(true),
            report_all: config.report_all.unwrap_or(false),
        })
    }
}

fn unit_interval(value: f64, property: &str) -> Result<f64> {
    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(anyhow!("{property} must be between 0 and 1, got {value}"))
    }
}

fn at_least(value: i64, minimum: i64, property: &str) -> Result<u64> {
    if value >= minimum {
        Ok(value as u64)
    } else {
        Err(anyhow!("{property} must be >= {minimum}, got {value}"))
    }
}

/// Lowercases and collapses whitespace so terms match the word-joined text in `detect`.
fn normalize_terms(terms: Vec<String>) -> Vec<String> {
    terms
        .into_iter()
        .map(|term| crate::detect::words(&term.to_lowercase()).join(" "))
        .filter(|term| !term.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(json: &str) -> Result<Settings> {
        Settings::try_from(json.as_bytes())
    }

    #[test]
    fn test_defaults_apply_to_empty_object() {
        let s = settings("{}").unwrap();
        assert_eq!(s.toxicity.threshold, 0.5);
        assert_eq!(s.anomaly.action, Action::Monitor);
        assert_eq!(s.max_inspected_bytes, 262_144);
        assert_eq!(s.feedback.as_ref().unwrap().path, "/quality-feedback");
        assert!(s.judge.is_none());
        assert!(!s.judge_misconfigured);
        assert!(!s.may_rewrite());
        assert!(s.strip_accept_encoding);
        assert!(!s.report_all);
        assert!(settings(r#"{"reportAll": true}"#).unwrap().may_rewrite());
    }

    #[test]
    fn test_empty_config_is_rejected() {
        assert!(settings("").is_err());
    }

    #[test]
    fn test_invalid_values_are_rejected() {
        assert!(settings(r#"{"toxicityThreshold": 1.5}"#).is_err());
        assert!(settings(r#"{"biasAction": "drop"}"#).is_err());
        assert!(settings(r#"{"judgeMode": "sometimes"}"#).is_err());
        assert!(settings(r#"{"maxInspectedBytes": 10}"#).is_err());
        assert!(settings(r#"{"lowConfidenceLogprob": 0.5}"#).is_err());
    }

    #[test]
    fn test_empty_feedback_path_disables_feedback() {
        assert!(settings(r#"{"feedbackPath": ""}"#)
            .unwrap()
            .feedback
            .is_none());
    }

    #[test]
    fn test_judge_without_service_is_flagged_misconfigured() {
        let s = settings(r#"{"judgeMode": "always"}"#).unwrap();
        assert!(s.judge.is_none());
        assert!(s.judge_misconfigured);
        assert!(
            !settings("{}").unwrap().judge_misconfigured,
            "default mode alone does not warn"
        );
    }

    #[test]
    fn test_judge_model_defaults() {
        let s = settings(r#"{"judgeService": "https://api.openai.com"}"#).unwrap();
        assert_eq!(s.judge.unwrap().model, DEFAULT_JUDGE_MODEL);
        let s =
            settings(r#"{"judgeService": "https://api.openai.com", "judgeModel": " "}"#).unwrap();
        assert_eq!(s.judge.unwrap().model, DEFAULT_JUDGE_MODEL);
        let s =
            settings(r#"{"judgeService": "https://api.openai.com", "judgeModel": "gpt-4.1-mini"}"#)
                .unwrap();
        assert_eq!(s.judge.unwrap().model, "gpt-4.1-mini");
        let s =
            settings(r#"{"judgeService": "https://api.openai.com", "judgeMode": "off"}"#).unwrap();
        assert!(s.judge.is_none());
    }

    #[test]
    fn test_judge_with_service_is_enabled() {
        let s = settings(
            r#"{"judgeMode": "onSuspicion", "judgeService": "http://judge.example.com", "judgeModel": "m", "judgeApiKey": "k", "judgeInstructions": "  No medical advice. "}"#,
        )
        .unwrap();
        let judge = s.judge.unwrap();
        assert_eq!(judge.mode, JudgeMode::OnSuspicion);
        assert_eq!(judge.path, "/v1/chat/completions");
        assert_eq!(judge.model, "m");
        assert_eq!(judge.api_key.as_deref(), Some("k"));
        assert_eq!(judge.instructions.as_deref(), Some("No medical advice."));
        assert_eq!(judge.context_chars, 8000);
        assert!(judge.json_mode);
        assert_eq!(judge.timeout, Duration::from_millis(5000));
    }

    #[test]
    fn test_terms_are_normalized_and_rewrite_detected() {
        let s =
            settings(r#"{"toxicityTerms": ["  Get   LOST "], "toxicityAction": "block"}"#).unwrap();
        assert_eq!(s.toxicity_terms, vec!["get lost".to_string()]);
        assert!(s.may_rewrite());
    }
}
