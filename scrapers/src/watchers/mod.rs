mod antigravity;
mod claude;
mod codex;
mod cursor;
mod deepseek_harness;

use crate::config::ContrailConfig;
use crate::log_writer::LogWriter;
use crate::notifier::Notifier;
use crate::sentry::Sentry;
use crate::types::{Interaction, MasterLog};

use anyhow::Result;
use chrono::{DateTime, Utc};
use uuid::Uuid;

pub struct Harvester {
    pub(crate) sentry: Sentry,
    pub(crate) notifier: Notifier,
    pub(crate) log_writer: LogWriter,
    pub(crate) config: ContrailConfig,
}

impl Harvester {
    pub fn new(log_writer: LogWriter, config: ContrailConfig) -> Self {
        Self {
            sentry: Sentry::new(),
            notifier: Notifier::new(),
            log_writer,
            config,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn log_interaction_with_metadata(
        &self,
        source: &str,
        session: &str,
        project: &str,
        content: &str,
        role: &str,
        extra_metadata: serde_json::Map<String, serde_json::Value>,
        timestamp: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let (clean_content, flags) = self.sentry.scan_and_redact(content);

        let mut metadata = serde_json::Map::new();
        metadata.insert(
            "user".to_string(),
            serde_json::Value::String(whoami::username()),
        );
        metadata.insert(
            "hostname".to_string(),
            serde_json::Value::String(whoami::devicename()),
        );

        // Check clipboard for leaks (did user copy this?)
        if role == "assistant" {
            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                if let Ok(clip_text) = clipboard.get_text() {
                    let copied = content_was_copied(&clean_content, &clip_text);
                    if copied {
                        metadata.insert(
                            "copied_to_clipboard".to_string(),
                            serde_json::Value::Bool(true),
                        );
                    }
                }
            }
        }

        // Merge extra metadata
        for (k, v) in extra_metadata {
            metadata.insert(k, v);
        }

        let log = MasterLog {
            event_id: Uuid::new_v4(),
            timestamp: timestamp.unwrap_or_else(Utc::now),
            source_tool: source.to_string(),
            project_context: project.to_string(),
            session_id: session.to_string(),
            interaction: Interaction {
                role: role.to_string(),
                content: clean_content,
                artifacts: None,
            },
            security_flags: flags,
            metadata: serde_json::Value::Object(metadata),
        };

        log.validate_schema()?;
        self.log_writer.write(log).await?;
        Ok(())
    }

    pub async fn flush_logs(&self) -> Result<()> {
        self.log_writer.flush().await
    }
}

fn content_was_copied(content: &str, clipboard: &str) -> bool {
    const PREFIX_CHARS: usize = 20;
    if content == clipboard {
        return true;
    }
    let mut chars = content.chars();
    let prefix = chars.by_ref().take(PREFIX_CHARS).collect::<String>();
    chars.next().is_some() && clipboard.contains(&prefix)
}

#[cfg(test)]
mod tests {
    use super::content_was_copied;

    #[test]
    fn clipboard_prefix_check_is_utf8_safe() {
        let content = "The graph exporter’s default path remains local.";
        assert!(content_was_copied(
            content,
            "prefix: The graph exporter’s default path"
        ));
        assert!(!content_was_copied(content, "different clipboard text"));
    }
}
