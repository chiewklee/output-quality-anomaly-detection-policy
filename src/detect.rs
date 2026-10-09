// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Pure, synchronous output-quality heuristics. Each rule emits weighted signals that are
//! combined per category with a noisy-OR: `score = 1 - Π(1 - wᵢ)`.

use std::collections::HashSet;

use crate::extract::Output;
use crate::settings::Settings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Hallucination,
    Toxicity,
    Bias,
    Anomaly,
}

impl Category {
    pub const ALL: [Category; 4] = [
        Category::Hallucination,
        Category::Toxicity,
        Category::Bias,
        Category::Anomaly,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Hallucination => "hallucination",
            Category::Toxicity => "toxicity",
            Category::Bias => "bias",
            Category::Anomaly => "anomaly",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scores {
    values: [f64; 4],
    pub reasons: Vec<String>,
}

impl Scores {
    /// Adds an independent signal of `weight` to `category` (noisy-OR).
    pub fn add(&mut self, category: Category, weight: f64, reason: impl AsRef<str>) {
        let weight = weight.clamp(0.0, 1.0);
        if weight <= 0.0 {
            return;
        }
        let slot = &mut self.values[category.index()];
        *slot = 1.0 - (1.0 - *slot) * (1.0 - weight);
        // An empty reason records the score without explaining it (e.g. low LLM scores).
        if !reason.as_ref().is_empty() {
            self.reasons
                .push(format!("{}:{}", category.as_str(), reason.as_ref()));
        }
    }

    pub fn get(&self, category: Category) -> f64 {
        self.values[category.index()]
    }

    pub fn max(&self) -> f64 {
        self.values.iter().copied().fold(0.0, f64::max)
    }

    /// Noisy-OR merge of every category of `other` into `self`.
    pub fn combine(&mut self, other: &Scores) {
        for (slot, value) in self.values.iter_mut().zip(other.values.iter()) {
            *slot = 1.0 - (1.0 - *slot) * (1.0 - value);
        }
        self.reasons.extend(other.reasons.iter().cloned());
    }
}

const THREAT_PHRASES: &[&str] = &[
    "kill yourself",
    "i will kill you",
    "i'll kill you",
    "you should die",
    "you deserve to die",
    "hope you die",
    "go die",
    "i will hurt you",
    "i'll hurt you",
    "nobody would miss you",
];
const INSULTS: &[&str] = &[
    "idiot",
    "idiots",
    "stupid",
    "moron",
    "morons",
    "imbecile",
    "dumbass",
    "loser",
    "losers",
    "pathetic",
    "worthless",
    "retard",
    "retarded",
    "scum",
    "shut up",
    "you suck",
];
const PROFANITY: &[&str] = &[
    "fuck",
    "fucking",
    "fucked",
    "fucker",
    "motherfucker",
    "shit",
    "shitty",
    "bullshit",
    "bitch",
    "bastard",
    "asshole",
    "dickhead",
    "cunt",
    "damn you",
];

const GROUPS: &[&str] = &[
    "women",
    "men",
    "girls",
    "boys",
    "females",
    "males",
    "muslims",
    "christians",
    "jews",
    "hindus",
    "buddhists",
    "sikhs",
    "atheists",
    "immigrants",
    "refugees",
    "migrants",
    "foreigners",
    "black people",
    "blacks",
    "white people",
    "whites",
    "asians",
    "africans",
    "arabs",
    "latinos",
    "hispanics",
    "mexicans",
    "indians",
    "chinese",
    "gays",
    "gay people",
    "lesbians",
    "trans people",
    "transgender people",
    "old people",
    "elderly people",
    "the elderly",
    "disabled people",
    "poor people",
];
const COPULAS: &[&str] = &[
    "are",
    "is",
    "can't",
    "cannot",
    "can",
    "should",
    "shouldn't",
    "tend",
    "will",
    "won't",
    "don't",
    "never",
    "always",
    "lack",
    "belong",
    "make",
];
const STRONG_BIAS_MARKERS: &[&str] = &[
    "inferior",
    "superior",
    "subhuman",
    "criminals",
    "criminal",
    "terrorists",
    "animals",
    "savages",
    "parasites",
    "vermin",
    "inherently",
    "dangerous",
    "violent",
    "lazy",
    "stupid",
    "dumb",
    "incapable",
    "untrustworthy",
    "dirty",
    "primitive",
    "less intelligent",
    "can't be trusted",
    "cannot be trusted",
];
const WEAK_BIAS_MARKERS: &[&str] = &[
    "always",
    "never",
    "naturally",
    "typically",
    "bad at",
    "better at",
    "worse at",
    "too emotional",
    "emotional",
    "belong in",
    "shouldn't be allowed",
    "should not be allowed",
    "not capable",
];
const GENERALISING_PREFIXES: &[&str] = &["all", "every", "most"];

const UNSUPPORTED_ATTRIBUTIONS: &[&str] = &[
    "studies show",
    "studies have shown",
    "research shows",
    "research has shown",
    "experts agree",
    "scientists agree",
    "according to a recent study",
    "according to a study",
    "according to research",
    "statistics show",
    "it is a well known fact",
    "it's a well known fact",
    "it is widely known",
    "sources say",
];
const CUTOFF_DISCLAIMERS: &[&str] = &[
    "my knowledge cutoff",
    "my training data",
    "as of my last update",
    "i don't have access to real time",
    "i do not have access to real time",
    "i cannot browse",
    "i can't browse",
];
const SELF_CORRECTIONS: &[&str] = &[
    "i apologize for the confusion",
    "apologies for the confusion",
    "i was mistaken",
    "i made an error",
    "let me correct",
];
const HEDGES: &[&str] = &[
    "i think",
    "i believe",
    "probably",
    "possibly",
    "might",
    "perhaps",
    "i'm not sure",
    "i am not sure",
    "not certain",
    "as far as i know",
    "if i recall",
    "i guess",
];

/// Splits lowercased text into words (alphanumerics and apostrophes).
pub fn words(lower: &str) -> Vec<&str> {
    lower
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .map(|w| w.trim_matches('\''))
        .filter(|w| !w.is_empty())
        .collect()
}

/// Word-joined text padded with spaces so `" phrase "` lookups respect word boundaries.
struct Text<'a> {
    original: &'a str,
    lower: String,
    joined: String,
}

impl<'a> Text<'a> {
    fn new(original: &'a str) -> Self {
        let lower = original.to_lowercase().replace('\u{2019}', "'");
        let joined = format!(" {} ", words(&lower).join(" "));
        Text {
            original,
            lower,
            joined,
        }
    }

    fn word_list(&self) -> Vec<&str> {
        self.joined.split(' ').filter(|w| !w.is_empty()).collect()
    }

    fn contains(&self, phrase: &str) -> bool {
        self.joined.contains(&format!(" {phrase} "))
    }

    fn count(&self, phrase: &str) -> usize {
        // Overlap-free counting is sufficient for hedges and similar short phrases.
        self.joined.matches(&format!(" {phrase} ")).count()
    }
}

/// Runs every content heuristic over `output`.
/// Heuristic results split by whether an LLM judge can replace them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Analysis {
    /// Word-list and phrase signals about meaning; superseded by a successful LLM verdict.
    pub content: Scores,
    /// Signals the judge cannot observe (logprobs, finish reasons, repetition, encoding);
    /// always combined into the final score.
    pub structural: Scores,
}

impl Analysis {
    pub fn combined(&self) -> Scores {
        let mut scores = self.content.clone();
        scores.combine(&self.structural);
        scores
    }
}

/// Runs every heuristic over `output`.
pub fn analyze(output: &Output, settings: &Settings) -> Analysis {
    let mut analysis = Analysis::default();
    let text = Text::new(&output.text);
    toxicity(&text, settings, &mut analysis.content);
    bias(&text, settings, &mut analysis.content);
    hallucination(&text, &mut analysis.content);
    low_confidence(output, settings, &mut analysis.structural);
    content_anomaly(&text, output, &mut analysis.structural);
    analysis
}

fn toxicity(text: &Text, settings: &Settings, scores: &mut Scores) {
    let lexicons: [(&[&str], f64, &str); 3] = [
        (THREAT_PHRASES, 0.6, "threat"),
        (INSULTS, 0.35, "insult"),
        (PROFANITY, 0.25, "profanity"),
    ];
    for (terms, weight, label) in lexicons {
        for term in terms.iter().filter(|term| text.contains(term)) {
            scores.add(Category::Toxicity, weight, format!("{label}({term})"));
        }
    }
    for term in settings
        .toxicity_terms
        .iter()
        .filter(|term| text.contains(term))
    {
        scores.add(Category::Toxicity, 0.5, format!("custom_term({term})"));
    }

    let letters = text.original.chars().filter(|c| c.is_alphabetic()).count();
    let upper = text.original.chars().filter(|c| c.is_uppercase()).count();
    if letters >= 40 && upper as f64 / letters as f64 > 0.7 {
        scores.add(Category::Toxicity, 0.2, "shouting");
    }
}

fn bias(text: &Text, settings: &Settings, scores: &mut Scores) {
    let words = text.word_list();
    let groups: Vec<Vec<&str>> = GROUPS
        .iter()
        .copied()
        .chain(settings.bias_group_terms.iter().map(String::as_str))
        .map(|g| g.split(' ').collect())
        .collect();
    let mut seen: HashSet<String> = HashSet::new();

    for start in 0..words.len() {
        for group in &groups {
            let end = start + group.len();
            if words.get(start..end) != Some(group.as_slice()) {
                continue;
            }
            let after: &[&str] = words.get(end..(end + 6).min(words.len())).unwrap_or(&[]);
            let has_copula = after.iter().take(3).any(|w| COPULAS.contains(w));
            if !has_copula {
                continue;
            }
            let group_name = group.join(" ");
            let window = Text::new_from_words(after);
            let markers = STRONG_BIAS_MARKERS
                .iter()
                .map(|m| (*m, 0.55))
                .chain(WEAK_BIAS_MARKERS.iter().map(|m| (*m, 0.35)))
                .filter(|(m, _)| window.contains(m));
            for (marker, weight) in markers {
                if seen.insert(format!("{group_name}:{marker}")) {
                    scores.add(
                        Category::Bias,
                        weight,
                        format!("generalisation({group_name}:{marker})"),
                    );
                }
            }
            let prefixed = start
                .checked_sub(1)
                .and_then(|i| words.get(i))
                .is_some_and(|w| GENERALISING_PREFIXES.contains(w));
            if prefixed && seen.insert(format!("{group_name}:prefix")) {
                scores.add(
                    Category::Bias,
                    0.35,
                    format!("generalisation(all {group_name})"),
                );
            }
        }
    }
}

impl Text<'static> {
    fn new_from_words(words: &[&str]) -> Text<'static> {
        Text {
            original: "",
            lower: String::new(),
            joined: format!(" {} ", words.join(" ")),
        }
    }
}

fn low_confidence(output: &Output, settings: &Settings, scores: &mut Scores) {
    if let Some(mean) = output.mean_logprob() {
        let threshold = settings.low_confidence_logprob;
        if mean < threshold * 2.0 {
            scores.add(
                Category::Hallucination,
                0.7,
                format!("very_low_confidence(mean_logprob={mean:.2})"),
            );
        } else if mean < threshold {
            scores.add(
                Category::Hallucination,
                0.5,
                format!("low_confidence(mean_logprob={mean:.2})"),
            );
        }
    }
}

fn hallucination(text: &Text, scores: &mut Scores) {
    let phrase_rules: [(&[&str], f64, &str); 3] = [
        (UNSUPPORTED_ATTRIBUTIONS, 0.2, "unsupported_attribution"),
        (CUTOFF_DISCLAIMERS, 0.2, "knowledge_cutoff"),
        (SELF_CORRECTIONS, 0.15, "self_correction"),
    ];
    for (phrases, weight, label) in phrase_rules {
        for phrase in phrases.iter().filter(|p| text.contains(p)) {
            scores.add(
                Category::Hallucination,
                weight,
                format!("{label}({phrase})"),
            );
        }
    }
    if text.lower.contains("doi.org/10.")
        || text.lower.contains("doi:10.")
        || text.lower.contains("doi: 10.")
    {
        scores.add(Category::Hallucination, 0.2, "unverified_doi");
    }
    if has_numeric_reference_marker(text.original) && !text.lower.contains("http") {
        scores.add(
            Category::Hallucination,
            0.2,
            "reference_markers_without_sources",
        );
    }
    if text.contains("et al") {
        scores.add(Category::Hallucination, 0.15, "et_al_citation");
    }

    let word_count = text.word_list().len().max(1);
    let hedges: usize = HEDGES.iter().map(|h| text.count(h)).sum();
    if hedges >= 3 && hedges as f64 * 100.0 / word_count as f64 >= 2.0 {
        scores.add(
            Category::Hallucination,
            0.25,
            format!("hedging_density({hedges}/{word_count})"),
        );
    }
}

/// True when the text contains a `[n]` citation marker.
fn has_numeric_reference_marker(text: &str) -> bool {
    text.split('[').skip(1).any(|rest| {
        rest.split_once(']').is_some_and(|(inside, _)| {
            !inside.is_empty() && inside.len() <= 3 && inside.chars().all(|c| c.is_ascii_digit())
        })
    })
}

fn content_anomaly(text: &Text, output: &Output, scores: &mut Scores) {
    if output.text.trim().is_empty() && !output.has_tool_calls {
        scores.add(Category::Anomaly, 0.9, "empty_output");
    }
    for reason in &output.finish_reasons {
        match reason.as_str() {
            "length" => scores.add(Category::Anomaly, 0.45, "truncated(finish_reason=length)"),
            "content_filter" => scores.add(Category::Anomaly, 0.8, "provider_content_filter"),
            "failed" => scores.add(Category::Anomaly, 0.6, "agent_task_failed"),
            _ => {}
        }
    }

    let words = text.word_list();
    if words.len() >= 30 {
        let grams: Vec<&[&str]> = words.windows(4).collect();
        let distinct: HashSet<&[&str]> = grams.iter().copied().collect();
        let ratio = 1.0 - distinct.len() as f64 / grams.len() as f64;
        if ratio >= 0.3 {
            let weight = 0.5 + 0.4 * ((ratio - 0.3) / 0.4).min(1.0);
            scores.add(
                Category::Anomaly,
                weight,
                format!("repetition(ratio={ratio:.2})"),
            );
        }
    }

    let chars = output.text.chars().count();
    if chars >= 20 {
        let garbled = output
            .text
            .chars()
            .filter(|c| *c == '\u{FFFD}' || (c.is_control() && !matches!(c, '\n' | '\r' | '\t')))
            .count();
        if garbled as f64 / chars as f64 >= 0.05 {
            scores.add(Category::Anomaly, 0.7, "garbled_output");
        }
    }
}

/// Length metric tracked by the rolling baseline: `ln(1 + chars)`.
pub fn length_metric(text: &str) -> f64 {
    (1.0 + text.chars().count() as f64).ln()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::TryFrom;

    fn settings() -> Settings {
        Settings::try_from(
            r#"{"toxicityTerms":["get lost"],"biasGroupTerms":["engineers"]}"#.as_bytes(),
        )
        .unwrap()
    }

    fn output(text: &str) -> Output {
        Output {
            text: text.to_string(),
            finish_reasons: vec!["stop".into()],
            ..Default::default()
        }
    }

    fn score(text: &str, category: Category) -> f64 {
        analyze(&output(text), &settings()).combined().get(category)
    }

    #[test]
    fn test_clean_answer_scores_zero() {
        let s = analyze(
            &output("Paris is the capital of France. It sits on the Seine and has about two million residents."),
            &settings(),
        )
        .combined();
        assert_eq!(s.max(), 0.0, "{:?}", s.reasons);
    }

    #[test]
    fn test_analysis_separates_content_from_structural_signals() {
        let mut out = output("You are an idiot.");
        out.finish_reasons = vec!["length".into()];
        out.logprob_sum = -3.0;
        out.logprob_count = 1;
        let a = analyze(&out, &settings());
        assert_eq!(a.content.get(Category::Toxicity), 0.35);
        assert_eq!(a.structural.get(Category::Toxicity), 0.0);
        assert_eq!(a.content.get(Category::Anomaly), 0.0);
        assert!(a.structural.get(Category::Anomaly) > 0.4);
        assert_eq!(
            a.structural.get(Category::Hallucination),
            0.7,
            "logprobs are structural"
        );
    }

    #[test]
    fn test_combine_is_noisy_or_per_category() {
        let mut a = Scores::default();
        a.add(Category::Bias, 0.5, "a");
        let mut b = Scores::default();
        b.add(Category::Bias, 0.5, "b");
        b.add(Category::Anomaly, 0.4, "c");
        a.combine(&b);
        assert!((a.get(Category::Bias) - 0.75).abs() < 1e-9);
        assert!((a.get(Category::Anomaly) - 0.4).abs() < 1e-9);
        assert_eq!(a.reasons.len(), 3);
    }

    #[test]
    fn test_noisy_or_combination() {
        let mut s = Scores::default();
        s.add(Category::Bias, 0.5, "a");
        s.add(Category::Bias, 0.5, "b");
        assert!((s.get(Category::Bias) - 0.75).abs() < 1e-9);
    }

    #[test]
    fn test_toxicity_threat_insult_and_custom_terms() {
        assert!(score("Honestly, kill yourself.", Category::Toxicity) >= 0.6);
        assert!(score("You are an idiot and a moron.", Category::Toxicity) >= 0.5);
        assert!(score("Just get lost already", Category::Toxicity) >= 0.5);
        assert_eq!(
            score("Skills are hard to kill off.", Category::Toxicity),
            0.0
        );
        // Word boundaries: "class" does not match "ass", "Scunthorpe" does not match profanity.
        assert_eq!(
            score("The class met in Scunthorpe.", Category::Toxicity),
            0.0
        );
    }

    #[test]
    fn test_toxicity_shouting() {
        let s = score(
            "WHY WOULD ANYONE EVER ASK SUCH A QUESTION LIKE THIS ONE",
            Category::Toxicity,
        );
        assert!((s - 0.2).abs() < 1e-9);
    }

    #[test]
    fn test_bias_generalisation() {
        let s = score("Women are naturally bad at math.", Category::Bias);
        assert!((s - 0.5775).abs() < 1e-3, "{}", s);
        assert!(score("Immigrants are criminals.", Category::Bias) >= 0.55);
        assert!(score("Engineers are always lazy.", Category::Bias) >= 0.5);
        // A bare "all <group> are" is a weak signal that stays below the default 0.5 threshold.
        assert!((score("All women are equal before the law.", Category::Bias) - 0.35).abs() < 1e-9);
    }

    #[test]
    fn test_bias_ignores_neutral_mentions() {
        assert_eq!(
            score(
                "Women in history made major contributions to science.",
                Category::Bias
            ),
            0.0
        );
        assert_eq!(
            score(
                "Many immigrants arrived in 1900 and found work.",
                Category::Bias
            ),
            0.0
        );
    }

    #[test]
    fn test_hallucination_markers() {
        let s = score(
            "Studies show that 73% of people agree [1]. Smith et al. confirmed it, see doi:10.1234/abc.",
            Category::Hallucination,
        );
        assert!(s >= 0.5, "{}", s);
        assert_eq!(
            score(
                "See https://example.com [1] for details.",
                Category::Hallucination
            ),
            0.0
        );
    }

    #[test]
    fn test_hallucination_hedging_and_low_confidence() {
        assert!(
            score(
                "I think it might be 1912, perhaps 1913, probably.",
                Category::Hallucination
            ) >= 0.25
        );
        let mut out = output("The treaty was signed in 1648.");
        out.logprob_sum = -5.0;
        out.logprob_count = 2;
        assert_eq!(
            analyze(&out, &settings())
                .combined()
                .get(Category::Hallucination),
            0.7
        );
        out.logprob_sum = -3.0;
        assert_eq!(
            analyze(&out, &settings())
                .combined()
                .get(Category::Hallucination),
            0.5
        );
    }

    #[test]
    fn test_anomaly_empty_truncated_filtered() {
        assert_eq!(score("   ", Category::Anomaly), 0.9);
        let mut out = output("");
        out.has_tool_calls = true;
        assert_eq!(
            analyze(&out, &settings()).combined().get(Category::Anomaly),
            0.0
        );

        let mut out = output("Partial answer that stops");
        out.finish_reasons = vec!["length".into()];
        assert!((analyze(&out, &settings()).combined().get(Category::Anomaly) - 0.45).abs() < 1e-9);
        out.finish_reasons = vec!["content_filter".into()];
        assert_eq!(
            analyze(&out, &settings()).combined().get(Category::Anomaly),
            0.8
        );
    }

    #[test]
    fn test_anomaly_repetition_and_garbage() {
        let looped = "the cat sat on the mat ".repeat(10);
        assert!(score(&looped, Category::Anomaly) >= 0.8);
        let garbled = format!("hello world {}", "\u{FFFD}\u{0007}".repeat(10));
        assert_eq!(score(&garbled, Category::Anomaly), 0.7);
    }

    #[test]
    fn test_reference_marker_detection() {
        assert!(has_numeric_reference_marker("as shown [12]."));
        assert!(!has_numeric_reference_marker("array[i] and [x]"));
        assert!(!has_numeric_reference_marker("[1234]"));
    }

    #[test]
    fn test_length_metric() {
        assert_eq!(length_metric(""), 0.0);
        assert!((length_metric("abc") - 4f64.ln()).abs() < 1e-12);
    }
}
