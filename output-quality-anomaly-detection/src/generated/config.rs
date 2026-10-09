use serde::Deserialize;
#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(alias = "anomalyAction")]
    pub anomaly_action: Option<String>,
    #[serde(alias = "anomalyThreshold")]
    pub anomaly_threshold: Option<f64>,
    #[serde(alias = "baselineWarmupSamples")]
    pub baseline_warmup_samples: Option<i64>,
    #[serde(alias = "biasAction")]
    pub bias_action: Option<String>,
    #[serde(alias = "biasGroupTerms")]
    pub bias_group_terms: Option<Vec<String>>,
    #[serde(alias = "biasThreshold")]
    pub bias_threshold: Option<f64>,
    #[serde(alias = "feedbackMinSamples")]
    pub feedback_min_samples: Option<i64>,
    #[serde(alias = "feedbackNegativeRateThreshold")]
    pub feedback_negative_rate_threshold: Option<f64>,
    #[serde(alias = "feedbackPath")]
    pub feedback_path: Option<String>,
    #[serde(alias = "feedbackWindowSeconds")]
    pub feedback_window_seconds: Option<i64>,
    #[serde(alias = "hallucinationAction")]
    pub hallucination_action: Option<String>,
    #[serde(alias = "hallucinationThreshold")]
    pub hallucination_threshold: Option<f64>,
    #[serde(alias = "judgeApiKey")]
    pub judge_api_key: Option<String>,
    #[serde(alias = "judgeContextChars")]
    pub judge_context_chars: Option<i64>,
    #[serde(alias = "judgeInstructions")]
    pub judge_instructions: Option<String>,
    #[serde(alias = "judgeJsonMode")]
    pub judge_json_mode: Option<bool>,
    #[serde(alias = "judgeMode")]
    pub judge_mode: Option<String>,
    #[serde(alias = "judgeModel")]
    pub judge_model: Option<String>,
    #[serde(alias = "judgePath")]
    pub judge_path: Option<String>,
    #[serde(
        alias = "judgeService",
        default,
        deserialize_with = "pdk::serde::deserialize_service_opt"
    )]
    pub judge_service: Option<pdk::hl::Service>,
    #[serde(alias = "judgeSuspicionScore")]
    pub judge_suspicion_score: Option<f64>,
    #[serde(alias = "judgeTimeoutMs")]
    pub judge_timeout_ms: Option<i64>,
    #[serde(alias = "lengthZScoreThreshold")]
    pub length_z_score_threshold: Option<f64>,
    #[serde(alias = "lowConfidenceLogprob")]
    pub low_confidence_logprob: Option<f64>,
    #[serde(alias = "maxInspectedBytes")]
    pub max_inspected_bytes: Option<i64>,
    #[serde(alias = "reportAll")]
    pub report_all: Option<bool>,
    #[serde(alias = "stripAcceptEncoding")]
    pub strip_accept_encoding: Option<bool>,
    #[serde(alias = "toxicityAction")]
    pub toxicity_action: Option<String>,
    #[serde(alias = "toxicityTerms")]
    pub toxicity_terms: Option<Vec<String>>,
    #[serde(alias = "toxicityThreshold")]
    pub toxicity_threshold: Option<f64>,
}
#[pdk::hl::entrypoint_flex]
fn init(abi: &dyn pdk::flex_abi::api::FlexAbi) -> Result<(), anyhow::Error> {
    let config: Config = serde_json::from_slice(abi.get_configuration())
        .map_err(|err| {
            anyhow::anyhow!(
                "Failed to parse configuration '{}'. Cause: {}",
                String::from_utf8_lossy(abi.get_configuration()), err
            )
        })?;
    if config.judge_service.is_some() {
        let service = config.judge_service.unwrap();
        abi.service_create(service)?;
    }
    abi.setup()?;
    Ok(())
}
