use crate::types::SecurityFlags;
use regex::Regex;

pub struct Sentry {
    patterns: Vec<(&'static str, Regex)>,
}

impl Sentry {
    pub fn new() -> Self {
        // Basic but broader patterns; labels are surfaced in redacted_secrets.
        let patterns = vec![
            ("openai_key", Regex::new(r"sk-[a-zA-Z0-9]{20,}").unwrap()),
            (
                "openai_proj_key",
                Regex::new(r"sk-proj-[a-zA-Z0-9]{20,}").unwrap(),
            ),
            (
                "anthropic_key",
                Regex::new(r"sk-ant-[a-zA-Z0-9_-]{20,}").unwrap(),
            ),
            (
                "github_token",
                Regex::new(r"gh[pousr]_[a-zA-Z0-9]{20,}").unwrap(),
            ),
            (
                "slack_token",
                Regex::new(r"xox[baprs]-[a-zA-Z0-9-]{10,}").unwrap(),
            ),
            ("aws_access_key", Regex::new(r"AKIA[0-9A-Z]{16}").unwrap()),
            (
                "jwt",
                Regex::new(r"eyJ[a-zA-Z0-9_-]{10,}\\.[a-zA-Z0-9_-]{10,}\\.[a-zA-Z0-9_-]{10,}")
                    .unwrap(),
            ),
            (
                "email",
                Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\\.[A-Za-z]{2,}").unwrap(),
            ),
        ];
        Self { patterns }
    }

    pub fn scan_and_redact(&self, content: &str) -> (String, SecurityFlags) {
        let mut redacted_content = content.to_string();
        let mut detected_secrets = Vec::new();
        let mut has_pii = false;

        for (label, pattern) in &self.patterns {
            if pattern.is_match(&redacted_content) {
                has_pii = true;
                redacted_content = pattern
                    .replace_all(&redacted_content, "[REDACTED]")
                    .to_string();
                detected_secrets.push(label.to_string());
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

impl Default for Sentry {
    fn default() -> Self {
        Self::new()
    }
}
