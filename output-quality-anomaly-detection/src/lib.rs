// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Output Quality & Anomaly Detection policy.
//!
//! Scores LLM responses for hallucination, toxicity, bias and abnormal output with an LLM judge
//! (OpenAI-compatible chat completions), falling back to in-gateway word-list heuristics when the
//! judge is unavailable, and always combining structural signals: token confidence, finish
//! reasons, repetition, a rolling length baseline and user feedback. Each category is then
//! monitored, annotated or blocked. See `docs/spec.md`.

mod detect;
mod extract;
mod feedback;
mod generated;
mod judge;
mod settings;
mod sse;
mod stats;
mod verdict;

#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests;

use std::collections::hash_map::DefaultHasher;
use std::convert::TryFrom;
use std::hash::{Hash, Hasher};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use pdk::data_storage::{
    DataStorage, DataStorageBuilder, DataStorageError, LocalDataStorage, StoreMode,
};
use pdk::hl::*;
use pdk::logger;
use pdk::policy_violation::PolicyViolations;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use crate::detect::{Category, Scores};
use crate::extract::{Output, StreamAccumulator};
use crate::settings::{Action, FeedbackSettings, JudgeSettings, Settings};
use crate::sse::SseBuffer;
use crate::stats::{Baseline, FeedbackWindow};
use crate::verdict::Verdict;

const POLICY_NAME: &str = "output-quality-anomaly-detection";
const STORAGE_ID: &str = "output-quality";
const BASELINE_KEY: &str = "length-baseline";
const FEEDBACK_KEY: &str = "feedback-window";
const CAS_ATTEMPTS: usize = 3;
const FEEDBACK_ANOMALY_WEIGHT: f64 = 0.75;

const SOURCE_LLM: &str = "llm";
const SOURCE_HEURISTIC: &str = "heuristic";
const SOURCE_FALLBACK: &str = "heuristic_fallback";

const ACCEPT_ENCODING_HEADER: &str = "accept-encoding";
const ALLOW_HEADER: &str = "allow";
const AUTHORIZATION_HEADER: &str = "authorization";
const CONTENT_ENCODING_HEADER: &str = "content-encoding";
const CONTENT_LENGTH_HEADER: &str = "content-length";
const CONTENT_TYPE_HEADER: &str = "content-type";
/// Marks the policy's own judge calls so an instance in front of the judge API skips them.
const JUDGE_GUARD_HEADER: &str = "x-output-quality-judge";
const APPLICATION_JSON: &str = "application/json";

struct Policy {
    settings: Settings,
    storage: LocalDataStorage,
    client: HttpClient,
    violations: PolicyViolations,
    /// Secret value of `JUDGE_GUARD_HEADER`, derived from the configuration so every replica
    /// with the same config agrees on it while clients cannot guess it.
    guard_token: String,
}

/// Data carried from the request filter to the response filter.
#[derive(Clone, Debug, Default)]
struct RequestContext {
    /// Transcript of the request conversation, for grounding the judge.
    conversation: Option<String>,
    /// Do not evaluate the response: this policy's own judge call, or an A2A method that
    /// carries no agent-authored content (`GetTask`, agent card, push config, …).
    skip: bool,
}

struct Evaluation {
    scores: Scores,
    verdict: Verdict,
    source: &'static str,
    judge_ms: Option<u128>,
}

async fn request_filter(request_state: RequestState, policy: &Policy) -> Flow<RequestContext> {
    let headers_state = request_state.into_headers_state().await;

    if let Some(feedback) = &policy.settings.feedback {
        let path = headers_state.path();
        if path.split('?').next().unwrap_or_default() == feedback.path {
            return handle_feedback(headers_state, policy, feedback).await;
        }
    }

    let handler = headers_state.handler();
    let guard = handler.header(JUDGE_GUARD_HEADER);
    handler.remove_header(JUDGE_GUARD_HEADER);
    if guard.as_deref() == Some(policy.guard_token.as_str()) {
        logger::debug!("[{POLICY_NAME}] judge call passes through unevaluated");
        return Flow::Continue(RequestContext {
            skip: true,
            ..Default::default()
        });
    }

    if policy.settings.strip_accept_encoding {
        handler.remove_header(ACCEPT_ENCODING_HEADER);
    }

    let context_chars = policy
        .settings
        .judge
        .as_ref()
        .map_or(0, |j| j.context_chars);
    let is_json = handler
        .header(CONTENT_TYPE_HEADER)
        .is_some_and(|ct| ct.to_ascii_lowercase().contains("json"));
    if !is_json || !headers_state.method().eq_ignore_ascii_case("POST") {
        return Flow::Continue(RequestContext::default());
    }

    // The body tells A2A methods apart and carries the conversation for the judge.
    let path = headers_state.path();
    let body_state = headers_state.into_body_state().await;
    if !body_state.contains_body() {
        return Flow::Continue(RequestContext::default());
    }
    let body = body_state.handler().body();
    if body.len() > policy.settings.max_inspected_bytes {
        logger::debug!(
            "[{POLICY_NAME}] request of {} bytes too large to inspect",
            body.len()
        );
        return Flow::Continue(RequestContext::default());
    }
    Flow::Continue(request_context(&body, &path, context_chars))
}

/// Builds the request context: A2A requests by method, everything else as an LLM API call.
fn request_context(body: &[u8], path: &str, context_chars: usize) -> RequestContext {
    let value: Option<Value> = serde_json::from_slice(body).ok();
    match value.as_ref().and_then(|v| extract::a2a_request(v, path)) {
        Some(a2a) => RequestContext {
            skip: !a2a.inspect,
            conversation: (context_chars > 0 && !a2a.text.is_empty())
                .then(|| extract::tail_chars(&format!("USER: {}", a2a.text), context_chars)),
        },
        None if context_chars > 0 => RequestContext {
            conversation: extract::prompt_from_request(body, context_chars),
            skip: false,
        },
        None => RequestContext::default(),
    }
}

async fn handle_feedback(
    headers_state: RequestHeadersState,
    policy: &Policy,
    settings: &FeedbackSettings,
) -> Flow<RequestContext> {
    if !headers_state.method().eq_ignore_ascii_case("POST") {
        return Flow::Break(
            json_response(405, &json!({ "error": "method not allowed" }))
                .with_headers(vec![(ALLOW_HEADER.to_string(), "POST".to_string())]),
        );
    }

    let body_state = headers_state.into_body_state().await;
    let signal = match feedback::parse(&body_state.handler().body()) {
        Ok(signal) => signal,
        Err(reason) => return Flow::Break(json_response(400, &json!({ "error": reason }))),
    };

    let bucket = stats::bucket_for(now_secs(), settings.window_seconds);
    let recorded = update_with_retry(
        &policy.storage,
        FEEDBACK_KEY,
        |window: &mut FeedbackWindow| {
            window.roll(bucket);
            window.record(signal.negative);
        },
    )
    .await;

    let response_id = signal.response_id.as_deref().unwrap_or("-");
    let category = signal.category.as_deref().unwrap_or("-");
    match recorded {
        Ok((_, window)) => {
            let (samples, rate) = window.rate();
            if signal.negative {
                logger::warn!(
                    "[{POLICY_NAME}] negative feedback response_id={response_id} category={category} window_samples={samples} window_negative_rate={rate:.2}"
                );
            } else {
                logger::info!(
                    "[{POLICY_NAME}] positive feedback response_id={response_id} window_samples={samples} window_negative_rate={rate:.2}"
                );
            }
            Flow::Break(json_response(
                202,
                &json!({ "status": "recorded", "windowSamples": samples, "windowNegativeRate": round2(rate) }),
            ))
        }
        Err(err) => {
            logger::warn!("[{POLICY_NAME}] feedback not recorded response_id={response_id}: {err}");
            Flow::Break(json_response(
                202,
                &json!({ "status": "accepted_unrecorded" }),
            ))
        }
    }
}

async fn response_filter(
    response_state: ResponseState,
    request_data: RequestData<RequestContext>,
    policy: &Policy,
) {
    let RequestData::Continue(context) = request_data else {
        return;
    };
    if context.skip {
        return;
    }
    let headers_state = response_state.into_headers_state().await;
    if !(200..300).contains(&headers_state.status_code()) {
        return;
    }

    let handler = headers_state.handler();
    let encoding = handler.header(CONTENT_ENCODING_HEADER).unwrap_or_default();
    if !encoding.is_empty() && !encoding.eq_ignore_ascii_case("identity") {
        logger::debug!("[{POLICY_NAME}] uninspected: compressed response ({encoding})");
        return;
    }
    let content_type = handler
        .header(CONTENT_TYPE_HEADER)
        .unwrap_or_default()
        .to_ascii_lowercase();

    if content_type.contains("text/event-stream") {
        inspect_stream(headers_state, &context, policy).await;
    } else if content_type.contains("json") {
        inspect_json(headers_state, &context, policy).await;
    }
}

async fn inspect_json(
    headers_state: ResponseHeadersState,
    context: &RequestContext,
    policy: &Policy,
) {
    let settings = &policy.settings;
    if settings.may_rewrite() {
        // Headers are committed before the body is read; drop the length now so a rewrite is valid.
        headers_state.handler().remove_header(CONTENT_LENGTH_HEADER);
    }

    let body_state = headers_state.into_body_state().await;
    if !body_state.contains_body() {
        return;
    }
    let body = body_state.handler().body();
    if body.len() > settings.max_inspected_bytes {
        logger::info!(
            "[{POLICY_NAME}] uninspected: body of {} bytes exceeds maxInspectedBytes={}",
            body.len(),
            settings.max_inspected_bytes
        );
        return;
    }
    let Some(output) = extract::from_json(&body, settings.max_inspected_bytes) else {
        logger::debug!("[{POLICY_NAME}] uninspected: unrecognised JSON response shape");
        return;
    };

    let evaluation = evaluate(&output, context.conversation.as_deref(), policy).await;
    let verdict = &evaluation.verdict;
    let report = verdict::report(
        &evaluation.scores,
        verdict,
        verdict.action,
        evaluation.source,
        evaluation.judge_ms,
    );
    if let Some(new_body) = verdict::rewrite(&body, verdict, report, settings.report_all) {
        if let Err(err) = body_state.handler().set_body(&new_body) {
            logger::error!(
                "[{POLICY_NAME}] could not rewrite response body ({err:?}); delivering original"
            );
        }
    }
    finish(&output, &evaluation, verdict.action.as_str(), policy);
}

async fn inspect_stream(
    headers_state: ResponseHeadersState,
    context: &RequestContext,
    policy: &Policy,
) {
    let limit = policy.settings.max_inspected_bytes;
    let body_state = headers_state.into_body_stream_state().await;
    let mut stream = body_state.stream();
    let mut sse = SseBuffer::default();
    let mut accumulator = StreamAccumulator::new(limit);
    let mut consumed = 0usize;
    let mut capped = false;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.into_bytes();
        if consumed >= limit {
            capped = true;
            continue; // Keep draining so the stream flows; stop accumulating.
        }
        consumed += bytes.len();
        for payload in sse.push(&bytes) {
            accumulator.push_event(&payload);
        }
    }
    if let Some(payload) = sse.finish() {
        accumulator.push_event(&payload);
    }

    let Some(mut output) = accumulator.finish() else {
        logger::debug!("[{POLICY_NAME}] uninspected: unrecognised event stream");
        return;
    };
    output.truncated |= capped;
    let evaluation = evaluate(&output, context.conversation.as_deref(), policy).await;
    let applied = if evaluation.verdict.action == Action::Monitor {
        "monitor"
    } else {
        "monitor(streaming)"
    };
    finish(&output, &evaluation, applied, policy);
}

/// Scores `output`: the LLM verdict when available, else the word-list heuristics, combined in
/// both cases with structural signals the judge cannot observe.
async fn evaluate(output: &Output, conversation: Option<&str>, policy: &Policy) -> Evaluation {
    let settings = &policy.settings;
    let analysis = detect::analyze(output, settings);

    let mut structural = analysis.structural.clone();
    if !output.truncated && !output.text.trim().is_empty() {
        length_signal(output, policy, &mut structural).await;
    }
    if let Some(feedback) = &settings.feedback {
        feedback_signal(feedback, policy, &mut structural).await;
    }

    let mut judge_ms = None;
    let (mut scores, source) = match &settings.judge {
        Some(judge) if judge::should_call(judge, analysis.combined().max(), &output.text) => {
            let started = Instant::now();
            let verdict = call_judge(
                &policy.client,
                judge,
                &policy.guard_token,
                conversation,
                &output.text,
            )
            .await;
            judge_ms = Some(started.elapsed().as_millis());
            match verdict {
                Some(llm) => (llm, SOURCE_LLM),
                None => {
                    logger::warn!("[{POLICY_NAME}] judge_unavailable: falling back to heuristics");
                    (analysis.content, SOURCE_FALLBACK)
                }
            }
        }
        _ => (analysis.content, SOURCE_HEURISTIC),
    };
    scores.combine(&structural);

    let verdict = verdict::decide(&scores, settings);
    Evaluation {
        scores,
        verdict,
        source,
        judge_ms,
    }
}

async fn length_signal(output: &Output, policy: &Policy, scores: &mut Scores) {
    let settings = &policy.settings;
    let metric = detect::length_metric(&output.text);
    match update_with_retry(&policy.storage, BASELINE_KEY, |b: &mut Baseline| {
        b.update(metric)
    })
    .await
    {
        Ok((previous, _)) => {
            if let Some(z) = previous.z_score(metric, settings.baseline_warmup_samples) {
                let threshold = settings.length_z_score_threshold;
                if z.abs() >= threshold {
                    scores.add(Category::Anomaly, 0.8, format!("length_outlier(z={z:.2})"));
                } else if z.abs() >= threshold * 0.66 {
                    scores.add(
                        Category::Anomaly,
                        0.4,
                        format!("length_deviation(z={z:.2})"),
                    );
                }
            }
        }
        Err(err) => logger::warn!("[{POLICY_NAME}] length baseline unavailable: {err}"),
    }
}

async fn feedback_signal(feedback: &FeedbackSettings, policy: &Policy, scores: &mut Scores) {
    match policy.storage.get::<FeedbackWindow>(FEEDBACK_KEY).await {
        Ok(Some((mut window, _))) => {
            window.roll(stats::bucket_for(now_secs(), feedback.window_seconds));
            let (samples, rate) = window.rate();
            if samples >= feedback.min_samples && rate >= feedback.negative_rate_threshold {
                scores.add(
                    Category::Anomaly,
                    FEEDBACK_ANOMALY_WEIGHT,
                    format!("feedback_negative_rate(rate={rate:.2},samples={samples})"),
                );
            }
        }
        Ok(None) => {}
        Err(err) => logger::warn!("[{POLICY_NAME}] feedback window unavailable: {err}"),
    }
}

async fn call_judge(
    client: &HttpClient,
    judge: &JudgeSettings,
    guard_token: &str,
    conversation: Option<&str>,
    text: &str,
) -> Option<Scores> {
    let body = judge::build_request(judge, conversation, text);
    let bearer = judge.api_key.as_ref().map(|key| format!("Bearer {key}"));
    let mut headers = vec![
        (CONTENT_TYPE_HEADER, APPLICATION_JSON),
        (JUDGE_GUARD_HEADER, guard_token),
    ];
    if let Some(bearer) = &bearer {
        headers.push((AUTHORIZATION_HEADER, bearer.as_str()));
    }

    let response = client
        .request(&judge.service)
        .path(&judge.path)
        .headers(headers)
        .body(&body)
        .timeout(judge.timeout)
        .post()
        .await;
    match response {
        Ok(response) if (200..300).contains(&response.status_code()) => {
            let parsed = judge::parse_response(response.body());
            if parsed.is_none() {
                logger::warn!("[{POLICY_NAME}] judge reply had no readable verdict");
            }
            parsed
        }
        Ok(response) => {
            logger::warn!(
                "[{POLICY_NAME}] judge returned status {}",
                response.status_code()
            );
            None
        }
        Err(err) => {
            logger::warn!("[{POLICY_NAME}] judge call failed: {err:?}");
            None
        }
    }
}

/// Reports the outcome: one log line per evaluation and a policy violation when flagged.
/// Response text and the judge API key are never logged.
fn finish(output: &Output, evaluation: &Evaluation, applied: &str, policy: &Policy) {
    let scores = &evaluation.scores;
    let id = output.id.as_deref().unwrap_or("-");
    let model = output.model.as_deref().unwrap_or("-");
    let judge_ms = evaluation
        .judge_ms
        .map_or_else(|| "-".to_string(), |ms| ms.to_string());
    let summary = format!(
        "h={:.2} t={:.2} b={:.2} a={:.2}",
        scores.get(Category::Hallucination),
        scores.get(Category::Toxicity),
        scores.get(Category::Bias),
        scores.get(Category::Anomaly)
    );
    let source = evaluation.source;
    if evaluation.verdict.is_flagged() {
        let flagged: Vec<&str> = evaluation
            .verdict
            .flagged
            .iter()
            .map(|c| c.as_str())
            .collect();
        logger::warn!(
            "[{POLICY_NAME}] flagged response_id={id} model={model} categories={} action={applied} source={source} judge_ms={judge_ms} scores=[{summary}] reasons={:?} truncated={}",
            flagged.join(","),
            scores.reasons,
            output.truncated
        );
        policy.violations.generate_policy_violation();
    } else {
        logger::info!(
            "[{POLICY_NAME}] evaluated response_id={id} model={model} source={source} judge_ms={judge_ms} scores=[{summary}] truncated={}",
            output.truncated
        );
    }
}

/// Read-modify-write with CAS retries. Returns the value before and after `update`.
async fn update_with_retry<T, F>(
    storage: &LocalDataStorage,
    key: &str,
    update: F,
) -> Result<(T, T), DataStorageError>
where
    T: Serialize + DeserializeOwned + Default + Clone,
    F: Fn(&mut T),
{
    for _ in 0..CAS_ATTEMPTS {
        let (previous, mode) = match storage.get::<T>(key).await? {
            Some((value, cas)) => (value, StoreMode::Cas(cas)),
            None => (T::default(), StoreMode::Absent),
        };
        let mut next = previous.clone();
        update(&mut next);
        match storage.store(key, &mode, &next).await {
            Ok(()) => return Ok((previous, next)),
            Err(DataStorageError::CasMismatch) => continue,
            Err(err) => return Err(err),
        }
    }
    Err(DataStorageError::CasMismatch)
}

fn json_response(status: u32, body: &Value) -> Response {
    let body = body.to_string();
    Response::new(status)
        .with_headers(vec![
            (
                CONTENT_TYPE_HEADER.to_string(),
                APPLICATION_JSON.to_string(),
            ),
            (CONTENT_LENGTH_HEADER.to_string(), body.len().to_string()),
        ])
        .with_body(body.into_bytes())
}

/// Deterministic per configuration (same on every replica), unguessable without the config.
fn judge_guard_token(config: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    POLICY_NAME.hash(&mut hasher);
    config.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

#[entrypoint]
async fn configure(
    launcher: Launcher,
    Configuration(bytes): Configuration,
    client: HttpClient,
    store_builder: DataStorageBuilder,
    violations: PolicyViolations,
) -> Result<()> {
    // Never echo the raw configuration: it may contain judgeApiKey.
    let settings = Settings::try_from(bytes.as_slice()).map_err(|err| {
        logger::error!("[{POLICY_NAME}] invalid configuration: {err}");
        err
    })?;
    if settings.judge_misconfigured {
        logger::warn!(
            "[{POLICY_NAME}] judgeMode is enabled but judgeService is not set; using heuristics only"
        );
    }

    let policy = Policy {
        guard_token: judge_guard_token(&bytes),
        settings,
        storage: store_builder.local(STORAGE_ID),
        client,
        violations,
    };
    let filter = on_request(|rs| request_filter(rs, &policy))
        .on_response(|rs, rd| response_filter(rs, rd, &policy));
    launcher.launch(filter).await?;
    Ok(())
}
