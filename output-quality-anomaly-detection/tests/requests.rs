// Copyright 2026 Salesforce, Inc. All rights reserved.

mod common;

use httpmock::MockServer;
use pdk_test::port::Port;
use pdk_test::services::flex::{ApiConfig, Flex, FlexConfig, PolicyConfig};
use pdk_test::services::httpmock::{HttpMock, HttpMockConfig};
use pdk_test::{pdk_test, TestComposite};
use serde_json::{json, Value};

use common::*;

// Flex port for the internal test network
const FLEX_PORT: Port = 8081;

const TOXIC: &str = "You are an idiot and a moron. Honestly, kill yourself.";
const CLEAN: &str = "Paris is the capital of France.";

fn chat(content: &str) -> String {
    json!({
        "id": "chatcmpl-it",
        "object": "chat.completion",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}]
    })
    .to_string()
}

/// Real Flex Gateway + httpmock backend: toxic output is withheld, clean output passes, and the
/// feedback endpoint is answered by the gateway without reaching the backend.
#[pdk_test]
async fn blocks_toxic_and_serves_feedback() -> anyhow::Result<()> {
    let httpmock_config = HttpMockConfig::builder()
        .port(80)
        .version("latest")
        .hostname("backend")
        .build();

    let policy_config = PolicyConfig::builder()
        .name(POLICY_NAME)
        .configuration(json!({"toxicityAction": "block"}))
        .build();

    let api_config = ApiConfig::builder()
        .name("myApi")
        .upstream(&httpmock_config)
        .path("/")
        .port(FLEX_PORT)
        .policies([policy_config])
        .build();

    let flex_config = FlexConfig::builder()
        .version("1.13.0")
        .hostname("local-flex")
        .with_api(api_config)
        .config_mounts([(POLICY_DIR, "policy"), (COMMON_CONFIG_DIR, "common")])
        .build();

    let composite = TestComposite::builder()
        .with_service(flex_config)
        .with_service(httpmock_config)
        .build()
        .await?;

    let flex: Flex = composite.service()?;
    let flex_url = flex.external_url(FLEX_PORT).unwrap();
    let httpmock: HttpMock = composite.service()?;
    let mock_server = MockServer::connect_async(httpmock.socket()).await;

    mock_server
        .mock_async(|when, then| {
            when.path("/toxic");
            then.status(200)
                .header("content-type", "application/json")
                .body(chat(TOXIC));
        })
        .await;
    mock_server
        .mock_async(|when, then| {
            when.path("/clean");
            then.status(200)
                .header("content-type", "application/json")
                .body(chat(CLEAN));
        })
        .await;
    let feedback_mock = mock_server
        .mock_async(|when, then| {
            when.path("/quality-feedback");
            then.status(500);
        })
        .await;

    let client = reqwest::Client::new();

    let blocked: Value = client
        .post(format!("{flex_url}/toxic"))
        .body("{}")
        .send()
        .await?
        .text()
        .await?
        .parse()?;
    assert_eq!(blocked["choices"][0]["finish_reason"], "content_filter");
    assert_eq!(blocked["x_output_quality"]["action"], "block");
    assert_eq!(blocked["x_output_quality"]["flagged"], json!(["toxicity"]));

    let clean: Value = client
        .post(format!("{flex_url}/clean"))
        .body("{}")
        .send()
        .await?
        .text()
        .await?
        .parse()?;
    assert_eq!(clean["choices"][0]["message"]["content"], CLEAN);
    assert!(clean.get("x_output_quality").is_none());

    let feedback = client
        .post(format!("{flex_url}/quality-feedback"))
        .header("content-type", "application/json")
        .body(r#"{"rating":"negative","responseId":"chatcmpl-it"}"#)
        .send()
        .await?;
    assert_eq!(feedback.status(), 202);
    let body: Value = feedback.text().await?.parse()?;
    assert_eq!(body["status"], "recorded");
    assert_eq!(
        feedback_mock.hits_async().await,
        0,
        "feedback must not reach the backend"
    );

    Ok(())
}
