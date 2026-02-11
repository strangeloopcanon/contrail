use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// An actionable rule derived from session exhaust.
///
/// Learnings flow through a lifecycle: candidate → active → deprecated.
/// Each learning links back to the evidence that produced it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Learning {
    pub id: Uuid,
    /// One imperative sentence describing the rule.
    pub rule: String,
    pub scope: LearningScope,
    /// References to source data that support this learning.
    pub evidence: Vec<EvidenceRef>,
    /// Confidence score in the range 0.0–1.0.
    pub confidence: f32,
    pub status: LearningStatus,
    /// Which agent produced or observed this, e.g. "cursor", "codex-cli".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_tool: Option<String>,
    /// Repository or project path this applies to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_context: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// How many times this pattern has been observed.
    pub count: u32,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

/// Where the learning applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningScope {
    Repo,
    Global,
    Tool(String),
}

/// Lifecycle status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningStatus {
    Candidate,
    Active,
    Deprecated,
}

/// A reference back to the source data supporting a learning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub kind: EvidenceKind,
    /// The actual path, id, or SHA.
    pub reference: String,
    /// Optional human-readable snippet or description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

/// What kind of evidence is referenced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    SessionFile,
    Commit,
    EventId,
    MasterLogLine,
}

// ---------------------------------------------------------------------------
// JSONL helpers
// ---------------------------------------------------------------------------

/// Read learnings from a JSONL file. Skips malformed lines.
pub fn read_learnings(path: &Path) -> Result<Vec<Learning>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file =
        fs::File::open(path).with_context(|| format!("open learnings at {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Learning>(&line) {
            Ok(l) => out.push(l),
            Err(e) => {
                tracing::warn!(error = %e, "skipping malformed learning line");
            }
        }
    }
    Ok(out)
}

/// Append a single learning to a JSONL file.
pub fn append_learning(path: &Path, learning: &Learning) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open learnings for append at {}", path.display()))?;
    serde_json::to_writer(&mut file, learning)?;
    file.write_all(b"\n")?;
    Ok(())
}

/// Atomically rewrite the entire learnings file (for compaction / dedup).
pub fn write_learnings(path: &Path, learnings: &[Learning]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut file = fs::File::create(&tmp)
            .with_context(|| format!("create temp file {}", tmp.display()))?;
        for l in learnings {
            serde_json::to_writer(&mut file, l)?;
            file.write_all(b"\n")?;
        }
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Deduplicate learnings by merging entries with identical normalised `rule` text.
///
/// For each group of duplicates:
/// - Keeps the earliest `first_seen` and latest `last_seen`.
/// - Sums `count`.
/// - Takes the highest `confidence`.
/// - Merges evidence lists.
/// - Preserves the `id` of the entry that was first seen earliest.
/// - Prefers `Active` > `Candidate` > `Deprecated` for status.
pub fn dedup_learnings(learnings: &mut Vec<Learning>) {
    if learnings.len() <= 1 {
        return;
    }

    // Group by normalised rule text.
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, l) in learnings.iter().enumerate() {
        let key = normalise_rule(&l.rule);
        groups.entry(key).or_default().push(i);
    }

    let mut merged: Vec<Learning> = Vec::with_capacity(groups.len());
    // Collect groups sorted by the earliest index to maintain insertion order.
    let mut ordered: Vec<(String, Vec<usize>)> = groups.into_iter().collect();
    ordered.sort_by_key(|(_, indices)| indices[0]);

    for (_, indices) in ordered {
        let mut base = learnings[indices[0]].clone();
        for &idx in &indices[1..] {
            let other = &learnings[idx];
            if other.first_seen < base.first_seen {
                base.first_seen = other.first_seen;
                base.id = other.id;
            }
            if other.last_seen > base.last_seen {
                base.last_seen = other.last_seen;
            }
            base.count = base.count.saturating_add(other.count);
            if other.confidence > base.confidence {
                base.confidence = other.confidence;
            }
            base.evidence.extend(other.evidence.iter().cloned());
            base.status = higher_status(&base.status, &other.status);
        }
        merged.push(base);
    }

    *learnings = merged;
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Normalise rule text for dedup: lowercase, collapse whitespace.
fn normalise_rule(rule: &str) -> String {
    rule.split_whitespace()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Active > Candidate > Deprecated.
fn higher_status(a: &LearningStatus, b: &LearningStatus) -> LearningStatus {
    fn rank(s: &LearningStatus) -> u8 {
        match s {
            LearningStatus::Deprecated => 0,
            LearningStatus::Candidate => 1,
            LearningStatus::Active => 2,
        }
    }
    if rank(b) > rank(a) {
        b.clone()
    } else {
        a.clone()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_learning(rule: &str) -> Learning {
        Learning {
            id: Uuid::new_v4(),
            rule: rule.to_string(),
            scope: LearningScope::Repo,
            evidence: vec![EvidenceRef {
                kind: EvidenceKind::SessionFile,
                reference: "2026-02-10_cursor_abc.md".to_string(),
                context: Some("user said: never use unwrap".to_string()),
            }],
            confidence: 0.8,
            status: LearningStatus::Candidate,
            source_tool: Some("cursor".to_string()),
            project_context: Some("/tmp/project".to_string()),
            tags: vec!["style".to_string()],
            count: 1,
            first_seen: Utc::now(),
            last_seen: Utc::now(),
        }
    }

    #[test]
    fn round_trip_json() {
        let l = sample_learning("Never use unwrap in production code");
        let json = serde_json::to_string(&l).unwrap();
        let back: Learning = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, l.id);
        assert_eq!(back.rule, l.rule);
        assert_eq!(back.scope, LearningScope::Repo);
        assert_eq!(back.status, LearningStatus::Candidate);
        assert_eq!(back.count, 1);
        assert_eq!(back.evidence.len(), 1);
        assert_eq!(back.evidence[0].kind, EvidenceKind::SessionFile);
    }

    #[test]
    fn scope_variants_serialize() {
        let repo: LearningScope = serde_json::from_str("\"repo\"").unwrap();
        assert_eq!(repo, LearningScope::Repo);

        let global: LearningScope = serde_json::from_str("\"global\"").unwrap();
        assert_eq!(global, LearningScope::Global);

        let tool: LearningScope = serde_json::from_str("{\"tool\":\"cursor\"}").unwrap();
        assert_eq!(tool, LearningScope::Tool("cursor".to_string()));

        // Round-trip Tool variant.
        let json = serde_json::to_string(&LearningScope::Tool("codex-cli".to_string())).unwrap();
        let back: LearningScope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, LearningScope::Tool("codex-cli".to_string()));
    }

    #[test]
    fn status_variants_serialize() {
        for (input, expected) in [
            ("\"candidate\"", LearningStatus::Candidate),
            ("\"active\"", LearningStatus::Active),
            ("\"deprecated\"", LearningStatus::Deprecated),
        ] {
            let parsed: LearningStatus = serde_json::from_str(input).unwrap();
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn optional_fields_omitted() {
        let mut l = sample_learning("test");
        l.source_tool = None;
        l.project_context = None;
        l.tags = vec![];
        let json = serde_json::to_string(&l).unwrap();
        assert!(!json.contains("source_tool"));
        assert!(!json.contains("project_context"));
        assert!(!json.contains("tags"));
    }

    #[test]
    fn jsonl_read_write_round_trip() {
        let dir = std::env::temp_dir().join(format!("contrail_learnings_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("learnings.jsonl");

        let l1 = sample_learning("Prefer Result over unwrap");
        let l2 = sample_learning("Run clippy before committing");

        append_learning(&path, &l1).unwrap();
        append_learning(&path, &l2).unwrap();

        let loaded = read_learnings(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].rule, l1.rule);
        assert_eq!(loaded[1].rule, l2.rule);

        // Clean up.
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_learnings_atomic() {
        let dir = std::env::temp_dir().join(format!("contrail_learnings_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("learnings.jsonl");

        let l1 = sample_learning("rule one");
        let l2 = sample_learning("rule two");
        let l3 = sample_learning("rule three");

        // Write initial set.
        write_learnings(&path, &[l1.clone(), l2.clone()]).unwrap();
        let loaded = read_learnings(&path).unwrap();
        assert_eq!(loaded.len(), 2);

        // Overwrite with different set.
        write_learnings(&path, std::slice::from_ref(&l3)).unwrap();
        let loaded = read_learnings(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].rule, "rule three");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_learnings_skips_empty_lines_and_bad_json() {
        let dir = std::env::temp_dir().join(format!("contrail_learnings_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("learnings.jsonl");

        let l = sample_learning("good rule");
        let good_json = serde_json::to_string(&l).unwrap();

        let content = format!("{good_json}\n\n{{\"bad\":true}}\n{good_json}\n");
        fs::write(&path, content).unwrap();

        let loaded = read_learnings(&path).unwrap();
        assert_eq!(loaded.len(), 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_learnings_missing_file() {
        let path =
            std::env::temp_dir().join(format!("nonexistent_learnings_{}.jsonl", Uuid::new_v4()));
        let loaded = read_learnings(&path).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn dedup_merges_identical_rules() {
        let now = Utc::now();
        let earlier = now - chrono::Duration::hours(2);

        let mut l1 = sample_learning("Never use unwrap");
        l1.first_seen = earlier;
        l1.last_seen = earlier;
        l1.count = 2;
        l1.confidence = 0.6;
        l1.status = LearningStatus::Candidate;

        let mut l2 = sample_learning("never use unwrap"); // same rule, different case
        l2.first_seen = now;
        l2.last_seen = now;
        l2.count = 3;
        l2.confidence = 0.9;
        l2.status = LearningStatus::Active;
        l2.evidence = vec![EvidenceRef {
            kind: EvidenceKind::Commit,
            reference: "abc123".to_string(),
            context: None,
        }];

        let mut learnings = vec![l1.clone(), l2];
        dedup_learnings(&mut learnings);

        assert_eq!(learnings.len(), 1);
        let merged = &learnings[0];
        assert_eq!(merged.id, l1.id); // l1 had earlier first_seen
        assert_eq!(merged.first_seen, earlier);
        assert_eq!(merged.last_seen, now);
        assert_eq!(merged.count, 5); // 2 + 3
        assert!((merged.confidence - 0.9).abs() < f32::EPSILON); // max
        assert_eq!(merged.status, LearningStatus::Active); // Active > Candidate
        assert_eq!(merged.evidence.len(), 2); // merged from both
    }

    #[test]
    fn dedup_preserves_distinct_rules() {
        let mut learnings = vec![
            sample_learning("rule alpha"),
            sample_learning("rule beta"),
            sample_learning("rule gamma"),
        ];
        dedup_learnings(&mut learnings);
        assert_eq!(learnings.len(), 3);
    }

    #[test]
    fn dedup_empty_and_single() {
        let mut empty: Vec<Learning> = vec![];
        dedup_learnings(&mut empty);
        assert!(empty.is_empty());

        let mut single = vec![sample_learning("only one")];
        dedup_learnings(&mut single);
        assert_eq!(single.len(), 1);
    }

    #[test]
    fn higher_status_ordering() {
        assert_eq!(
            higher_status(&LearningStatus::Candidate, &LearningStatus::Active),
            LearningStatus::Active
        );
        assert_eq!(
            higher_status(&LearningStatus::Active, &LearningStatus::Deprecated),
            LearningStatus::Active
        );
        assert_eq!(
            higher_status(&LearningStatus::Deprecated, &LearningStatus::Candidate),
            LearningStatus::Candidate
        );
    }
}
