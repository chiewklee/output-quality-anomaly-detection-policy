// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Parsing of feedback signals POSTed to `feedbackPath`.

use serde_json::Value;

const CATEGORIES: &[&str] = &["hallucination", "toxicity", "bias", "anomaly", "other"];
const MAX_ID_CHARS: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub struct Feedback {
    pub negative: bool,
    pub response_id: Option<String>,
    pub category: Option<String>,
}

pub fn parse(body: &[u8]) -> Result<Feedback, String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| "body must be a JSON object".to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| "body must be a JSON object".to_string())?;

    let negative = match object.get("rating").and_then(Value::as_str) {
        Some("negative") => true,
        Some("positive") => false,
        _ => return Err("rating must be \"positive\" or \"negative\"".to_string()),
    };
    let response_id = match object.get("responseId") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) => Some(id.chars().take(MAX_ID_CHARS).collect()),
        Some(_) => return Err("responseId must be a string".to_string()),
    };
    let category = match object.get("category") {
        None | Some(Value::Null) => None,
        Some(Value::String(c)) if CATEGORIES.contains(&c.as_str()) => Some(c.clone()),
        Some(_) => return Err(format!("category must be one of {}", CATEGORIES.join(", "))),
    };
    Ok(Feedback {
        negative,
        response_id,
        category,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_feedback() {
        let f = parse(
            br#"{"rating":"negative","responseId":"chatcmpl-1","category":"bias","comment":"x"}"#,
        )
        .unwrap();
        assert_eq!(
            f,
            Feedback {
                negative: true,
                response_id: Some("chatcmpl-1".into()),
                category: Some("bias".into())
            }
        );
        assert!(!parse(br#"{"rating":"positive"}"#).unwrap().negative);
    }

    #[test]
    fn test_invalid_feedback() {
        assert!(parse(b"not json").is_err());
        assert!(parse(b"[1]").is_err());
        assert!(parse(br#"{"rating":"meh"}"#).is_err());
        assert!(parse(br#"{"rating":"negative","category":"spam"}"#).is_err());
        assert!(parse(br#"{"rating":"negative","responseId":5}"#).is_err());
    }

    #[test]
    fn test_response_id_is_bounded() {
        let body = format!(
            r#"{{"rating":"negative","responseId":"{}"}}"#,
            "a".repeat(1000)
        );
        assert_eq!(
            parse(body.as_bytes()).unwrap().response_id.unwrap().len(),
            MAX_ID_CHARS
        );
    }
}
