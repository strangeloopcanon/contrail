use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde_json::Value;
use std::fs;

#[derive(Clone)]
pub struct LlmClient {
    http: Client,
    api_key: String,
    model: String,
}

impl LlmClient {
    pub fn from_env() -> Result<Option<Self>> {
        let api_key = match std::env::var("OPENAI_API_KEY") {
            Ok(k) if !k.trim().is_empty() => Some(k),
            _ => read_key_file(),
        };
        let Some(api_key) = api_key else {
            return Ok(None);
        };
        let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-5.1".to_string());
        Ok(Some(Self {
            http: Client::new(),
            api_key,
            model,
        }))
    }

    pub async fn chat(
        &self,
        prompt: &str,
        model_override: Option<String>,
        temperature: Option<f32>,
    ) -> Result<Value> {
        let model = model_override.unwrap_or_else(|| self.model.clone());
        let body = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": "You are a concise analyst generating structured hypotheses and follow-up questions from AI coding session traces. Respond with JSON only."},
                {"role": "user", "content": prompt}
            ],
            "temperature": temperature.unwrap_or(0.0),
        });

        let res = self
            .http
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("send chat request")?;

        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            bail!("LLM call failed: {} - {}", status, text);
        }

        let json: Value = res.json().await.context("decode chat response")?;
        let content = json
            .pointer("/choices/0/message/content")
            .cloned()
            .unwrap_or(Value::String(String::from("")));
        let parsed_json = if let Some(text) = content.as_str() {
            serde_json::from_str::<Value>(text).unwrap_or_else(|_| Value::String(text.to_string()))
        } else {
            content
        };

        Ok(serde_json::json!({
            "raw": json,
            "parsed": parsed_json
        }))
    }
}

fn read_key_file() -> Option<String> {
    let candidates = [
        "~/.config/openai/api_key",
        "~/.config/openai/key",
        "~/.openai/api_key",
    ];
    for path in candidates {
        let expanded = shellexpand::tilde(path).into_owned();
        if let Ok(content) = fs::read_to_string(&expanded) {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}
