use anyhow::{anyhow, ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MasterLog {
    pub event_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub source_tool: String,
    pub project_context: String,
    pub session_id: String,
    pub interaction: Interaction,
    pub security_flags: SecurityFlags,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Interaction {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<Artifact>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Artifact {
    pub r#type: String,
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SecurityFlags {
    pub has_pii: bool,
    pub redacted_secrets: Vec<String>,
}

impl MasterLog {
    pub fn validate_schema(&self) -> Result<()> {
        validate_log_value(&serde_json::to_value(self)?)
    }
}

pub fn validate_log_value(value: &serde_json::Value) -> Result<()> {
    let obj = value
        .as_object()
        .context("log entry must be a JSON object")?;

    let event_id = obj
        .get("event_id")
        .and_then(|v| v.as_str())
        .context("event_id missing or not string")?;
    Uuid::parse_str(event_id).context("event_id must be a UUID")?;

    let timestamp = obj
        .get("timestamp")
        .and_then(|v| v.as_str())
        .context("timestamp missing or not string")?;
    DateTime::parse_from_rfc3339(timestamp).context("timestamp must be RFC3339")?;

    ensure_string(obj, "source_tool")?;
    ensure_string(obj, "project_context")?;
    ensure_string(obj, "session_id")?;

    let interaction = obj
        .get("interaction")
        .and_then(|v| v.as_object())
        .context("interaction must be an object")?;
    ensure_string(interaction, "role")?;
    ensure_string(interaction, "content")?;

    if let Some(artifacts) = interaction.get("artifacts") {
        let artifacts_array = artifacts
            .as_array()
            .context("artifacts must be an array when present")?;
        for artifact in artifacts_array {
            let artifact_obj = artifact.as_object().context("artifact must be an object")?;
            ensure_string(artifact_obj, "type")?;
            ensure_string(artifact_obj, "content")?;
        }
    }

    let security_flags = obj
        .get("security_flags")
        .and_then(|v| v.as_object())
        .context("security_flags must be an object")?;
    ensure!(
        security_flags
            .get("has_pii")
            .and_then(|v| v.as_bool())
            .is_some(),
        "security_flags.has_pii must be a bool"
    );
    let redacted_secrets = security_flags
        .get("redacted_secrets")
        .and_then(|v| v.as_array())
        .context("security_flags.redacted_secrets must be an array")?;
    for entry in redacted_secrets {
        ensure!(
            entry.as_str().is_some(),
            "security_flags.redacted_secrets entries must be strings"
        );
    }

    let metadata = obj.get("metadata").context("metadata missing")?;
    ensure!(
        metadata.is_object(),
        "metadata must be a JSON object (can be empty)"
    );

    Ok(())
}

fn ensure_string<'a>(
    map: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<&'a str> {
    map.get(key)
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("{key} missing or not a non-empty string"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_schema() -> Result<()> {
        let log = MasterLog {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            source_tool: "cursor".to_string(),
            project_context: "/tmp/project".to_string(),
            session_id: "session-123".to_string(),
            interaction: Interaction {
                role: "assistant".to_string(),
                content: "hello".to_string(),
                artifacts: None,
            },
            security_flags: SecurityFlags {
                has_pii: false,
                redacted_secrets: vec![],
            },
            metadata: serde_json::json!({"example": true}),
        };

        log.validate_schema()?;
        Ok(())
    }

    #[test]
    fn rejects_invalid_uuid() {
        let invalid = serde_json::json!({
            "event_id": "not-a-uuid",
            "timestamp": Utc::now().to_rfc3339(),
            "source_tool": "cursor",
            "project_context": "/tmp/project",
            "session_id": "session-123",
            "interaction": { "role": "assistant", "content": "hello" },
            "security_flags": { "has_pii": false, "redacted_secrets": [] },
            "metadata": {}
        });

        assert!(validate_log_value(&invalid).is_err());
    }

    #[test]
    fn rejects_missing_content() {
        let invalid = serde_json::json!({
            "event_id": Uuid::new_v4(),
            "timestamp": Utc::now().to_rfc3339(),
            "source_tool": "cursor",
            "project_context": "/tmp/project",
            "session_id": "session-123",
            "interaction": { "role": "assistant" },
            "security_flags": { "has_pii": false, "redacted_secrets": [] },
            "metadata": {}
        });

        assert!(validate_log_value(&invalid).is_err());
    }
}
