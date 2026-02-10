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
                Regex::new(r"eyJ[a-zA-Z0-9_-]{10,}\.[a-zA-Z0-9_-]{10,}\.[a-zA-Z0-9_-]{10,}")
                    .unwrap(),
            ),
            (
                "email",
                Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sentry() -> Sentry {
        Sentry::new()
    }

    #[test]
    fn clean_content_passes_through() {
        let (content, flags) = sentry().scan_and_redact("Hello, this is normal text.");
        assert_eq!(content, "Hello, this is normal text.");
        assert!(!flags.has_pii);
        assert!(flags.redacted_secrets.is_empty());
    }

    #[test]
    fn redacts_openai_key() {
        let input = "My key is sk-abcdefghijklmnopqrstuvwxyz";
        let (content, flags) = sentry().scan_and_redact(input);
        assert!(content.contains("[REDACTED]"));
        assert!(!content.contains("sk-abc"));
        assert!(flags.has_pii);
        assert!(flags.redacted_secrets.contains(&"openai_key".to_string()));
    }

    #[test]
    fn redacts_openai_project_key() {
        let input = "key: sk-proj-abcdefghijklmnopqrstuvwxyz";
        let (content, flags) = sentry().scan_and_redact(input);
        assert!(content.contains("[REDACTED]"));
        assert!(flags.has_pii);
    }

    #[test]
    fn redacts_anthropic_key() {
        let input = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz";
        let (content, flags) = sentry().scan_and_redact(input);
        assert!(content.contains("[REDACTED]"));
        assert!(flags
            .redacted_secrets
            .contains(&"anthropic_key".to_string()));
    }

    #[test]
    fn redacts_github_token() {
        let input = "token=ghp_abcdefghijklmnopqrstuvwxyz";
        let (content, flags) = sentry().scan_and_redact(input);
        assert!(content.contains("[REDACTED]"));
        assert!(flags.redacted_secrets.contains(&"github_token".to_string()));
    }

    #[test]
    fn redacts_slack_token() {
        let input = "xoxb-abcdefghij";
        let (content, flags) = sentry().scan_and_redact(input);
        assert!(content.contains("[REDACTED]"));
        assert!(flags.redacted_secrets.contains(&"slack_token".to_string()));
    }

    #[test]
    fn redacts_aws_access_key() {
        let input = "aws key: AKIAIOSFODNN7EXAMPLE";
        let (content, flags) = sentry().scan_and_redact(input);
        assert!(content.contains("[REDACTED]"));
        assert!(flags
            .redacted_secrets
            .contains(&"aws_access_key".to_string()));
    }

    #[test]
    fn redacts_jwt() {
        // Construct a JWT-shaped token at runtime so secret scanners don't flag the repo.
        let segment_1 = format!("eyJ{}", "a".repeat(24));
        let segment_2 = "b".repeat(32);
        let segment_3 = "c".repeat(48);
        let jwt = format!("{segment_1}.{segment_2}.{segment_3}");
        let input = format!("Bearer {jwt}");
        let (content, flags) = sentry().scan_and_redact(&input);
        assert!(content.contains("[REDACTED]"));
        assert!(flags.redacted_secrets.contains(&"jwt".to_string()));
    }

    #[test]
    fn redacts_email() {
        let input = "contact john.doe@example.com for help";
        let (content, flags) = sentry().scan_and_redact(input);
        assert!(content.contains("[REDACTED]"));
        assert!(!content.contains("john.doe@example.com"));
        assert!(flags.redacted_secrets.contains(&"email".to_string()));
    }

    #[test]
    fn redacts_multiple_secrets_in_one_pass() {
        let input = "key=sk-abcdefghijklmnopqrstuvwxyz email=user@example.com";
        let (content, flags) = sentry().scan_and_redact(input);
        assert!(!content.contains("sk-abc"));
        assert!(!content.contains("user@example.com"));
        assert!(flags.has_pii);
        assert!(flags.redacted_secrets.len() >= 2);
    }
}
