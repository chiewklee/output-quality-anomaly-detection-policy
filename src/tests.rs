// Copyright 2026 Salesforce, Inc. All rights reserved.

//! End-to-end pdk-unit tests of the request/response orchestration.

mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use pdk_unit::{
        TraceBackend, UnitHttpMessage, UnitHttpRequest, UnitHttpResponse, UnitTest, UnitTestBuilder,
    };
    use serde_json::{json, Value};

    use crate::verdict::{REPORT_FIELD, WITHHELD_MESSAGE};

    const CLEAN: &str = "Paris is the capital of France and sits on the Seine.";
    const TOXIC: &str = "You are an idiot and a moron. Honestly, kill yourself.";

    fn chat(content: &str) -> String {
        json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "model": "gpt-test",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}]
        })
        .to_string()
    }

    fn json_backend(body: String) -> UnitHttpResponse {
        UnitHttpResponse::new(200)
            .with_header("content-type", "application/json")
            .with_header("content-length", body.len().to_string())
            .with_body(body)
    }

    fn tester(config: Value, backend: UnitHttpResponse) -> UnitTest {
        UnitTestBuilder::default()
            .with_config(config.to_string())
            .with_backend(backend)
            .with_entrypoint(crate::configure)
    }

    fn post_chat() -> UnitHttpRequest {
        UnitHttpRequest::post()
            .with_path("/v1/chat/completions")
            .with_header("content-type", "application/json")
            .with_body(r#"{"model":"gpt-test","messages":[{"role":"user","content":"hi"}]}"#)
    }

    fn body_json(response: &UnitHttpResponse) -> Value {
        serde_json::from_slice(response.body()).unwrap()
    }

    mod response_filter {
        use super::*;

        #[test]
        fn test_clean_response_passes_unchanged() {
            let mut t = tester(
                json!({"toxicityAction": "block"}),
                json_backend(chat(CLEAN)),
            );
            let response = t.request(post_chat());
            assert_eq!(response.status_code(), 200);
            assert_eq!(
                body_json(&response),
                serde_json::from_str::<Value>(&chat(CLEAN)).unwrap()
            );
            assert!(response.violation().is_none());
        }

        #[test]
        fn test_monitor_flags_without_changing_body() {
            let mut t = tester(json!({}), json_backend(chat(TOXIC)));
            let response = t.request(post_chat());
            assert_eq!(response.status_code(), 200);
            assert_eq!(
                body_json(&response)["choices"][0]["message"]["content"],
                TOXIC
            );
            assert!(body_json(&response).get(REPORT_FIELD).is_none());
            assert!(
                response.violation().is_some(),
                "flagged response must record a violation"
            );
        }

        #[test]
        fn test_block_withholds_toxic_completion() {
            let mut t = tester(
                json!({"toxicityAction": "block"}),
                json_backend(chat(TOXIC)),
            );
            let response = t.request(post_chat());
            assert_eq!(
                response.status_code(),
                200,
                "status is committed before the body is read"
            );
            let body = body_json(&response);
            assert_eq!(body["choices"][0]["message"]["content"], WITHHELD_MESSAGE);
            assert_eq!(body["choices"][0]["finish_reason"], "content_filter");
            assert_eq!(body[REPORT_FIELD]["action"], "block");
            assert_eq!(body[REPORT_FIELD]["flagged"], json!(["toxicity"]));
            assert!(response
                .header("content-length")
                .is_none_or(|len| len == response.body().len().to_string()));
            assert!(response.violation().is_some());
        }

        #[test]
        fn test_annotate_bias() {
            let mut t = tester(
                json!({"biasAction": "annotate"}),
                json_backend(chat("Women are naturally bad at math.")),
            );
            let body = body_json(&t.request(post_chat()));
            assert_eq!(
                body["choices"][0]["message"]["content"],
                "Women are naturally bad at math."
            );
            assert_eq!(body[REPORT_FIELD]["flagged"], json!(["bias"]));
            assert_eq!(body[REPORT_FIELD]["scores"]["bias"], 0.58);
        }

        #[test]
        fn test_non_2xx_is_not_inspected() {
            let backend = UnitHttpResponse::new(500)
                .with_header("content-type", "application/json")
                .with_body(chat(TOXIC));
            let mut t = tester(json!({"toxicityAction": "block"}), backend);
            let response = t.request(post_chat());
            assert_eq!(response.status_code(), 500);
            assert_eq!(
                body_json(&response)["choices"][0]["message"]["content"],
                TOXIC
            );
        }

        #[test]
        fn test_compressed_response_is_not_inspected() {
            let backend = json_backend(chat(TOXIC)).with_header("content-encoding", "gzip");
            let mut t = tester(json!({"toxicityAction": "block"}), backend);
            let response = t.request(post_chat());
            assert_eq!(
                body_json(&response)["choices"][0]["message"]["content"],
                TOXIC
            );
            assert!(response.violation().is_none());
        }

        #[test]
        fn test_oversized_body_is_not_inspected() {
            let big = format!("{TOXIC} {}", "padding ".repeat(400));
            let mut t = tester(
                json!({"toxicityAction": "block", "maxInspectedBytes": 1024}),
                json_backend(chat(&big)),
            );
            let response = t.request(post_chat());
            assert_eq!(
                body_json(&response)["choices"][0]["message"]["content"],
                big.as_str()
            );
        }

        #[test]
        fn test_streaming_is_monitored_and_forwarded_unchanged() {
            let events = [
                json!({"id": "c1", "choices": [{"delta": {"content": "You are an idiot "}}]}),
                json!({"id": "c1", "choices": [{"delta": {"content": "and a moron."}}]}),
                json!({"id": "c1", "choices": [{"delta": {}, "finish_reason": "stop"}]}),
            ];
            let mut stream: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
            stream.push_str("data: [DONE]\n\n");
            let backend = UnitHttpResponse::new(200)
                .with_header("content-type", "text/event-stream")
                .with_body(stream.clone());

            let mut t = tester(json!({"toxicityAction": "block"}), backend);
            t.set_chunk_size(7); // force events to straddle chunks
            let response = t.request(post_chat());
            assert_eq!(response.status_code(), 200);
            assert_eq!(std::str::from_utf8(response.body()).unwrap(), stream);
            assert!(response.violation().is_some());
        }

        #[test]
        fn test_length_outlier_is_flagged_after_warmup() {
            let calls = Rc::new(Cell::new(0usize));
            let seen = Rc::clone(&calls);
            let backend = move |_req: UnitHttpRequest| {
                let n = seen.get();
                seen.set(n + 1);
                // Five ordinary answers of slightly varying length, then a far longer one.
                let text = if n < 5 {
                    "word ".repeat(20 + n)
                } else {
                    "Lorem ipsum dolor. ".repeat(400)
                };
                json_backend(chat(&text))
            };
            let mut t = UnitTestBuilder::default()
                .with_config(
                    json!({"baselineWarmupSamples": 5, "anomalyAction": "annotate"}).to_string(),
                )
                .with_backend(backend)
                .with_entrypoint(crate::configure);

            for _ in 0..5 {
                assert!(body_json(&t.request(post_chat()))
                    .get(REPORT_FIELD)
                    .is_none());
            }
            let body = body_json(&t.request(post_chat()));
            assert_eq!(body[REPORT_FIELD]["flagged"], json!(["anomaly"]));
            let reasons = body[REPORT_FIELD]["reasons"].to_string();
            assert!(reasons.contains("length_outlier"), "{}", reasons);
        }
    }

    mod judge {
        use super::*;
        use std::cell::RefCell;

        const JUDGE: &str = "judge.example.com";

        #[derive(Default)]
        struct Seen {
            path: Option<String>,
            authorization: Option<String>,
            guard: Option<String>,
            body: Value,
        }

        fn judge_config(extra: Value) -> Value {
            let mut config = json!({
                "judgeService": format!("http://{JUDGE}"),
                "judgeModel": "judge-mini",
                "judgeApiKey": "test-key",
                "toxicityAction": "block",
                "biasAction": "annotate",
                "anomalyAction": "annotate"
            });
            if let (Some(base), Some(extra)) = (config.as_object_mut(), extra.as_object()) {
                base.extend(extra.clone());
            }
            config
        }

        fn llm_reply(verdict: Value) -> UnitHttpResponse {
            let body = json!({"choices": [{"message": {"role": "assistant", "content": verdict.to_string()}}]});
            UnitHttpResponse::new(200)
                .with_header("content-type", "application/json")
                .with_body(body.to_string())
        }

        /// Policy in front of a mock LLM backend returning `answer`, judged by a mock judge.
        fn judged(
            config: Value,
            answer: UnitHttpResponse,
            judge: UnitHttpResponse,
        ) -> (UnitTest, Rc<RefCell<Seen>>) {
            let seen = Rc::new(RefCell::new(Seen::default()));
            let record = Rc::clone(&seen);
            let tester = UnitTestBuilder::default()
                .with_config(config.to_string())
                .with_backend(answer)
                .with_http_upstream_from_authority(JUDGE, move |req: UnitHttpRequest| {
                    let mut seen = record.borrow_mut();
                    seen.path = req.header(":path").map(str::to_string);
                    seen.authorization = req.header("authorization").map(str::to_string);
                    seen.guard = req.header("x-output-quality-judge").map(str::to_string);
                    seen.body = serde_json::from_slice(req.body()).unwrap_or(Value::Null);
                    judge.clone()
                })
                .with_entrypoint(crate::configure);
            (tester, seen)
        }

        fn ask(question: &str) -> UnitHttpRequest {
            UnitHttpRequest::post()
                .with_path("/v1/chat/completions")
                .with_header("content-type", "application/json")
                .with_body(json!({"model": "gpt-test", "messages": [{"role": "user", "content": question}]}).to_string())
        }

        #[test]
        fn test_llm_verdict_drives_flags_and_sees_the_conversation() {
            let config = judge_config(json!({}));
            let (mut t, seen) = judged(
                config.clone(),
                json_backend(chat(
                    "Nurses are usually women because they are more caring.",
                )),
                llm_reply(
                    json!({"hallucination": 0.1, "toxicity": 0, "bias": 0.86, "anomaly": 0,
                                 "reasons": {"bias": "Gender stereotype about nurses."}}),
                ),
            );
            let body = body_json(&t.request(ask("Why are most nurses women?")));
            let report = &body[REPORT_FIELD];
            assert_eq!(report["flagged"], json!(["bias"]));
            assert_eq!(report["source"], "llm");
            assert_eq!(report["scores"]["bias"], 0.86);
            assert_eq!(
                report["reasons"],
                json!(["bias:llm(Gender stereotype about nurses.)"])
            );

            let seen = seen.borrow();
            assert_eq!(seen.path.as_deref(), Some("/v1/chat/completions"));
            assert_eq!(seen.authorization.as_deref(), Some("Bearer test-key"));
            assert_eq!(
                seen.guard.as_deref(),
                Some(crate::judge_guard_token(config.to_string().as_bytes()).as_str())
            );
            assert_eq!(seen.body["model"], "judge-mini");
            let user = seen.body["messages"][1]["content"].as_str().unwrap();
            assert!(
                user.contains("USER: Why are most nurses women?"),
                "{}",
                user
            );
            assert!(user.contains("Nurses are usually women because they are more caring."));
        }

        #[test]
        fn test_llm_clears_word_list_false_positive() {
            // The word lists score "kill yourself" 0.6 and would block; the LLM sees a refusal.
            let (mut t, _) = judged(
                judge_config(json!({})),
                json_backend(chat(
                    "I won't repeat that. Telling someone to \"kill yourself\" is harmful.",
                )),
                llm_reply(json!({"hallucination": 0, "toxicity": 0.05, "bias": 0, "anomaly": 0})),
            );
            let response = t.request(ask("Insult me"));
            let body = body_json(&response);
            assert!(body["choices"][0]["message"]["content"]
                .as_str()
                .unwrap()
                .starts_with("I won't"));
            assert!(body.get(REPORT_FIELD).is_none());
            assert!(response.violation().is_none());
        }

        #[test]
        fn test_judge_failure_falls_back_to_heuristics() {
            let (mut t, _) = judged(
                judge_config(json!({})),
                json_backend(chat(TOXIC)),
                UnitHttpResponse::new(503),
            );
            let body = body_json(&t.request(ask("hi")));
            assert_eq!(body["choices"][0]["message"]["content"], WITHHELD_MESSAGE);
            assert_eq!(body[REPORT_FIELD]["source"], "heuristic_fallback");
        }

        #[test]
        fn test_unreadable_judge_reply_falls_back() {
            let reply = UnitHttpResponse::new(200).with_body(
                json!({"choices": [{"message": {"content": "Sorry, I can't help."}}]}).to_string(),
            );
            let (mut t, _) = judged(judge_config(json!({})), json_backend(chat(TOXIC)), reply);
            let body = body_json(&t.request(ask("hi")));
            assert_eq!(body[REPORT_FIELD]["source"], "heuristic_fallback");
        }

        #[test]
        fn test_structural_signals_apply_alongside_llm() {
            let answer = json!({"id": "c", "choices": [{"message": {"content": "Here is a partial"}, "finish_reason": "content_filter"}]});
            let (mut t, _) = judged(
                judge_config(json!({})),
                json_backend(answer.to_string()),
                llm_reply(json!({"hallucination": 0, "toxicity": 0, "bias": 0, "anomaly": 0})),
            );
            let body = body_json(&t.request(ask("hi")));
            assert_eq!(body[REPORT_FIELD]["flagged"], json!(["anomaly"]));
            assert_eq!(body[REPORT_FIELD]["source"], "llm");
            assert!(body[REPORT_FIELD]["reasons"]
                .to_string()
                .contains("provider_content_filter"));
        }

        #[test]
        fn test_zero_context_sends_response_only() {
            let (mut t, seen) = judged(
                judge_config(json!({"judgeContextChars": 0})),
                json_backend(chat(CLEAN)),
                llm_reply(json!({"hallucination": 0, "toxicity": 0, "bias": 0, "anomaly": 0})),
            );
            t.request(ask("secret question"));
            let user = seen.borrow().body["messages"][1]["content"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(user.contains("(none provided)"));
            assert!(!user.contains("secret question"));
        }

        #[test]
        fn test_on_suspicion_skips_clean_output() {
            let (mut t, seen) = judged(
                judge_config(json!({"judgeMode": "onSuspicion"})),
                json_backend(chat(CLEAN)),
                llm_reply(json!({"toxicity": 1})),
            );
            let body = body_json(&t.request(ask("hi")));
            assert!(seen.borrow().path.is_none(), "judge must not be called");
            assert!(body.get(REPORT_FIELD).is_none());
        }

        #[test]
        fn test_own_judge_calls_are_not_judged() {
            let config = judge_config(json!({}));
            let token = crate::judge_guard_token(config.to_string().as_bytes());
            let backend = Rc::new(TraceBackend::new(json_backend(chat(TOXIC))));
            let mut t = UnitTestBuilder::default()
                .with_config(config.to_string())
                .with_backend(Rc::clone(&backend))
                .with_http_upstream_from_authority(JUDGE, |_req: UnitHttpRequest| {
                    UnitHttpResponse::new(503)
                })
                .with_entrypoint(crate::configure);

            let body =
                body_json(&t.request(ask("hi").with_header("x-output-quality-judge", token)));
            assert_eq!(
                body["choices"][0]["message"]["content"], TOXIC,
                "judge traffic passes untouched"
            );
            assert_eq!(
                backend.next().unwrap().header("x-output-quality-judge"),
                None,
                "guard header is stripped"
            );

            let body =
                body_json(&t.request(ask("hi").with_header("x-output-quality-judge", "guess")));
            assert_eq!(
                body["choices"][0]["message"]["content"], WITHHELD_MESSAGE,
                "a wrong token is evaluated"
            );
        }
    }

    mod feedback {
        use super::*;

        fn feedback(rating: &str) -> UnitHttpRequest {
            UnitHttpRequest::post()
                .with_path("/quality-feedback")
                .with_header("content-type", "application/json")
                .with_body(json!({"rating": rating, "responseId": "chatcmpl-1", "category": "hallucination"}).to_string())
        }

        #[test]
        fn test_feedback_is_answered_by_gateway() {
            let backend = Rc::new(TraceBackend::new(json_backend(chat(CLEAN))));
            let mut t = UnitTestBuilder::default()
                .with_config("{}")
                .with_backend(Rc::clone(&backend))
                .with_entrypoint(crate::configure);

            let response = t.request(feedback("negative"));
            assert_eq!(response.status_code(), 202);
            let body = body_json(&response);
            assert_eq!(body["status"], "recorded");
            assert_eq!(body["windowSamples"], 1);
            assert_eq!(body["windowNegativeRate"], 1.0);
            assert!(
                backend.next().is_none(),
                "feedback must not reach the upstream"
            );
        }

        #[test]
        fn test_feedback_validation() {
            let mut t = tester(json!({}), json_backend(chat(CLEAN)));
            let response = t.request(UnitHttpRequest::get().with_path("/quality-feedback?x=1"));
            assert_eq!(response.status_code(), 405);
            assert_eq!(response.header("allow"), Some("POST"));

            let response = t.request(
                UnitHttpRequest::post()
                    .with_path("/quality-feedback")
                    .with_body(r#"{"rating":"meh"}"#),
            );
            assert_eq!(response.status_code(), 400);
            assert!(body_json(&response)["error"]
                .as_str()
                .unwrap()
                .contains("rating"));
        }

        #[test]
        fn test_negative_feedback_rate_raises_anomaly() {
            let mut t = tester(
                json!({"feedbackMinSamples": 4, "feedbackNegativeRateThreshold": 0.5, "anomalyAction": "annotate"}),
                json_backend(chat(CLEAN)),
            );
            for rating in ["negative", "negative", "positive"] {
                assert_eq!(t.request(feedback(rating)).status_code(), 202);
            }
            assert!(
                body_json(&t.request(post_chat()))
                    .get(REPORT_FIELD)
                    .is_none(),
                "below min samples"
            );

            assert_eq!(t.request(feedback("negative")).status_code(), 202);
            let body = body_json(&t.request(post_chat()));
            assert_eq!(body[REPORT_FIELD]["flagged"], json!(["anomaly"]));
            assert!(body[REPORT_FIELD]["reasons"]
                .to_string()
                .contains("feedback_negative_rate"));
        }

        #[test]
        fn test_feedback_disabled_forwards_path() {
            let backend = Rc::new(TraceBackend::new(json_backend(chat(CLEAN))));
            let mut t = UnitTestBuilder::default()
                .with_config(json!({"feedbackPath": ""}).to_string())
                .with_backend(Rc::clone(&backend))
                .with_entrypoint(crate::configure);
            assert_eq!(t.request(feedback("negative")).status_code(), 200);
            assert!(backend.next().is_some());
        }
    }

    mod request_filter {
        use super::*;

        #[test]
        fn test_accept_encoding_is_stripped_by_default() {
            let backend = Rc::new(TraceBackend::new(json_backend(chat(CLEAN))));
            let mut t = UnitTestBuilder::default()
                .with_config("{}")
                .with_backend(Rc::clone(&backend))
                .with_entrypoint(crate::configure);
            t.request(post_chat().with_header("accept-encoding", "gzip"));
            assert_eq!(backend.next().unwrap().header("accept-encoding"), None);
        }

        #[test]
        fn test_accept_encoding_kept_when_disabled() {
            let backend = Rc::new(TraceBackend::new(json_backend(chat(CLEAN))));
            let mut t = UnitTestBuilder::default()
                .with_config(json!({"stripAcceptEncoding": false}).to_string())
                .with_backend(Rc::clone(&backend))
                .with_entrypoint(crate::configure);
            t.request(post_chat().with_header("accept-encoding", "gzip"));
            assert_eq!(
                backend.next().unwrap().header("accept-encoding"),
                Some("gzip")
            );
        }
    }

    mod a2a {
        use super::*;
        use std::cell::RefCell;

        fn task(text: &str) -> String {
            json!({"jsonrpc": "2.0", "id": 1, "result": {
                "id": "task-1", "contextId": "ctx-1",
                "status": {"state": "TASK_STATE_COMPLETED"},
                "artifacts": [{"artifactId": "a1", "name": "answer", "parts": [{"text": text}]}]
            }})
            .to_string()
        }

        fn send_message(question: &str) -> UnitHttpRequest {
            UnitHttpRequest::post()
                .with_path("/quality-demo-agent/jsonrpc")
                .with_header("content-type", "application/json")
                .with_header("a2a-version", "1.0")
                .with_body(json!({"jsonrpc": "2.0", "id": 1, "method": "SendMessage", "params": {"message": {
                    "messageId": "m1", "role": "ROLE_USER", "parts": [{"text": question}]
                }}}).to_string())
        }

        #[test]
        fn test_toxic_agent_reply_is_withheld_with_report_in_metadata() {
            let mut t = tester(
                json!({"toxicityAction": "block"}),
                json_backend(task(TOXIC)),
            );
            let response = t.request(send_message("My code does not compile"));
            assert_eq!(response.status_code(), 200);
            let body = body_json(&response);
            assert_eq!(body["jsonrpc"], "2.0");
            assert_eq!(
                body["result"]["artifacts"][0]["parts"][0]["text"],
                WITHHELD_MESSAGE
            );
            let report = &body["result"]["metadata"][REPORT_FIELD];
            assert_eq!(report["action"], "block");
            assert_eq!(report["flagged"], json!(["toxicity"]));
            assert!(response.violation().is_some());
        }

        #[test]
        fn test_get_task_is_not_rescored() {
            let mut t = tester(
                json!({"toxicityAction": "block"}),
                json_backend(task(TOXIC)),
            );
            let response = t.request(
                UnitHttpRequest::post()
                    .with_path("/quality-demo-agent/jsonrpc")
                    .with_header("content-type", "application/json")
                    .with_body(
                        r#"{"jsonrpc":"2.0","id":2,"method":"GetTask","params":{"id":"task-1"}}"#,
                    ),
            );
            assert_eq!(
                body_json(&response)["result"]["artifacts"][0]["parts"][0]["text"],
                TOXIC
            );
            assert!(response.violation().is_none());
        }

        #[test]
        fn test_judge_receives_a2a_user_message_as_context() {
            let seen = Rc::new(RefCell::new(String::new()));
            let record = Rc::clone(&seen);
            let mut t = UnitTestBuilder::default()
                .with_config(json!({"judgeService": "http://judge.example.com", "judgeModel": "j", "hallucinationAction": "annotate"}).to_string())
                .with_backend(json_backend(task("Edison invented the telephone in 1921.")))
                .with_http_upstream_from_authority("judge.example.com", move |req: UnitHttpRequest| {
                    let body: Value = serde_json::from_slice(req.body()).unwrap_or(Value::Null);
                    *record.borrow_mut() = body["messages"][1]["content"].as_str().unwrap_or_default().to_string();
                    let verdict = json!({"hallucination": 0.95, "toxicity": 0, "bias": 0, "anomaly": 0,
                                         "reasons": {"hallucination": "Bell, not Edison."}});
                    UnitHttpResponse::new(200).with_body(json!({"choices": [{"message": {"content": verdict.to_string()}}]}).to_string())
                })
                .with_entrypoint(crate::configure);
            let body = body_json(&t.request(send_message("Who invented the telephone?")));
            assert!(
                seen.borrow().contains("USER: Who invented the telephone?"),
                "{}",
                seen.borrow()
            );
            assert!(seen
                .borrow()
                .contains("Edison invented the telephone in 1921."));
            let report = &body["result"]["metadata"][REPORT_FIELD];
            assert_eq!(report["flagged"], json!(["hallucination"]));
            assert_eq!(report["source"], "llm");
        }
    }

    mod local_mode {
        use super::*;

        #[test]
        fn test_local_mode_scores_and_blocks_without_control_plane() {
            let mut t = UnitTestBuilder::default()
                .local_mode()
                .with_config(json!({"toxicityAction": "block"}).to_string())
                .with_backend(json_backend(chat(TOXIC)))
                .with_entrypoint(crate::configure);
            let body = body_json(&t.request(post_chat()));
            assert_eq!(body["choices"][0]["message"]["content"], WITHHELD_MESSAGE);
        }
    }
}
