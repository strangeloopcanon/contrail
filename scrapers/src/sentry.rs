use crate::types::SecurityFlags;
use regex::Regex;

pub struct Sentry {
    secret_patterns: Vec<Regex>,
}

impl Sentry {
    pub fn new() -> Self {
        // Basic patterns for demo
        let patterns = vec![
            Regex::new(r"sk-[a-zA-Z0-9]{20,}").unwrap(), // OpenAI-ish
            Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),    // AWS
        ];
        Self {
            secret_patterns: patterns,
        }
    }

    pub fn scan_and_redact(&self, content: &str) -> (String, SecurityFlags) {
        let mut redacted_content = content.to_string();
        let mut detected_secrets = Vec::new();
        let mut has_pii = false;

        for pattern in &self.secret_patterns {
            if pattern.is_match(content) {
                has_pii = true; // Treating secrets as sensitive
                                // Redact
                redacted_content = pattern
                    .replace_all(&redacted_content, "[REDACTED_SECRET]")
                    .to_string();
                detected_secrets.push("SECRET_DETECTED".to_string());
            }
        }

        (
            redacted_content,
            SecurityFlags {
                has_pii,
                redacted_secrets: detected_secrets,
            },
        )
    }
}
