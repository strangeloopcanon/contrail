use crate::models::{ScoredTurn, SessionSummary};
use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use regex::Regex;

pub fn score_turn(content: &str, role: &str, metadata: &serde_json::Value) -> (f32, Vec<String>) {
    let mut score = 1.0;
    let mut cues = Vec::new();

    let lower = content.to_lowercase();
    if role == "user" {
        score += 0.3;
    }
    if lower.contains('?') {
        score += 0.4;
        cues.push("question".to_string());
    }
    if contains_any(&lower, &["error", "fail", "panic", "exception", "stack trace"]) {
        score += 0.3;
        cues.push("error".to_string());
    }
    if lower.contains("TODO") || lower.contains("todo") {
        score += 0.2;
        cues.push("todo".to_string());
    }
    if content.len() > 800 {
        score += 0.2;
        cues.push("long".to_string());
    }

    if let Some(obj) = metadata.as_object() {
        if obj.get("interrupted").and_then(|v| v.as_bool()).unwrap_or(false) {
            score += 0.5;
            cues.push("interrupted".to_string());
        }
        if obj.get("file_effects").and_then(|v| v.as_array()).is_some() {
            score += 0.6;
            cues.push("file_effects".to_string());
        }
        if obj
            .get("copied_to_clipboard")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            score += 0.3;
            cues.push("clipboard".to_string());
        }
    }

    (score, cues)
}

pub fn score_session(turns: &[ScoredTurn], summary: &SessionSummary, now: DateTime<Utc>) -> f32 {
    let mut score: f32 = turns.iter().map(|t| t.salience).sum();

    if summary.interrupted {
        score += 1.0;
    }
    if summary.file_effects > 0 {
        score += 0.5;
    }

    let age_days = (now - summary.ended_at).num_seconds().abs() as f32 / 86_400.0;
    let recency_boost = 1.0 + (0.5 / (1.0 + age_days));
    score *= recency_boost;
    score
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

pub fn tokenize(content: &str) -> Vec<String> {
    // basic alphanumeric tokenization; keeps it dependency-light
    // Use regex to avoid tiny tokens
    static REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"[A-Za-z0-9]{3,}").unwrap());

    REGEX
        .find_iter(content)
        .map(|m| m.as_str().to_lowercase())
        .collect()
}
