use crate::memory::MemoryRecord;
use crate::memory_blocks::MemoryBlock;
use crate::models::SalientSession;
use chrono::{DateTime, NaiveDate, Utc};
use scrapers::sentry::Sentry;
use scrapers::types::SecurityFlags;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize, Clone)]
pub struct MemorySnippet {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub query: String,
    pub day: Option<String>,
    pub llm_response_parsed: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct ContextPackResponse {
    pub generated_at: DateTime<Utc>,
    pub day: Option<NaiveDate>,
    pub prompt: String,
    pub security_flags: SecurityFlags,
    pub memory_blocks: Vec<MemoryBlock>,
    pub top_sessions: Vec<SalientSession>,
    pub recent_memories: Vec<MemorySnippet>,
}

pub fn build_prompt(
    day: Option<NaiveDate>,
    blocks: &[MemoryBlock],
    sessions: &[SalientSession],
    memories: &[MemorySnippet],
    max_chars: usize,
) -> (String, SecurityFlags) {
    let mut out = String::new();
    out.push_str("CONTRAIL CONTEXT PACK (local-first)\n");
    out.push_str("This bundle is derived from local Contrail logs; secrets/PII are redacted.\n");
    out.push_str(&format!(
        "Generated: {}\n",
        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    ));
    if let Some(d) = day {
        out.push_str(&format!("Day filter: {d}\n"));
    } else {
        out.push_str("Day filter: (none)\n");
    }
    out.push('\n');

    if !blocks.is_empty() {
        out.push_str("MEMORY BLOCKS (editable)\n");
        for b in blocks {
            let ctx = b
                .project_context
                .as_deref()
                .map(|c| format!(" @ {c}"))
                .unwrap_or_default();
            let mut line = format!("- [{}]{ctx}: {}\n", b.label, squash_ws(&b.value));
            line = truncate_chars(&line, 900);
            out.push_str(&line);
            if !b.security_flags.redacted_secrets.is_empty() {
                out.push_str(&format!(
                    "  (redacted: {})\n",
                    b.security_flags.redacted_secrets.join(", ")
                ));
            }
        }
        out.push('\n');
    }

    if !sessions.is_empty() {
        out.push_str("TOP SESSIONS (evidence)\n");
        for (idx, s) in sessions.iter().enumerate() {
            let mut flags = Vec::new();
            if s.session.interrupted {
                flags.push("interrupted");
            }
            if s.session.file_effects > 0 {
                flags.push("file_effects");
            }
            if s.session.clipboard_hits > 0 {
                flags.push("clipboard");
            }
            let flags = if flags.is_empty() {
                String::new()
            } else {
                format!(" flags={}", flags.join(","))
            };

            out.push_str(&format!(
                "{}. {} {} ({}) score={:.2}{}\n",
                idx + 1,
                s.session.source_tool,
                s.session.project_context,
                s.session.session_id,
                s.session.score,
                flags
            ));
            for t in &s.top_turns {
                out.push_str(&format!(
                    "   - [{}] {}: {}\n",
                    t.timestamp
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    t.role,
                    truncate_chars(&squash_ws(&t.content_snippet), 260)
                ));
            }
        }
        out.push('\n');
    }

    if !memories.is_empty() {
        out.push_str("RECENT MEMORIES (derived)\n");
        for m in memories {
            out.push_str(&format!(
                "- [{}] probe=\"{}\"\n",
                m.created_at
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                truncate_chars(&squash_ws(&m.query), 120)
            ));

            if let Some(v) = m.llm_response_parsed.as_ref() {
                let rendered = render_json_brief(v);
                if !rendered.trim().is_empty() {
                    out.push_str(&indent_lines(&truncate_chars(&rendered, 1200), 2));
                    out.push('\n');
                }
            }
        }
        out.push('\n');
    }

    if out.chars().count() > max_chars {
        out = truncate_chars(&out, max_chars);
        out.push_str("\n\n[TRUNCATED]\n");
    }

    let sentry = Sentry::new();
    let (redacted, flags) = sentry.scan_and_redact(&out);
    (redacted, flags)
}

pub fn to_memory_snippets(
    records: Vec<MemoryRecord>,
    limit: usize,
    day: Option<NaiveDate>,
) -> Vec<MemorySnippet> {
    let mut out = Vec::new();

    let iter = records.into_iter().rev();
    for r in iter {
        if let (Some(day_filter), Some(record_day)) = (day, r.day.as_deref())
            && record_day != day_filter.to_string()
        {
            continue;
        }

        let parsed = r
            .llm_response
            .as_ref()
            .and_then(|v| v.get("parsed").cloned())
            .filter(|v| !v.is_null());

        out.push(MemorySnippet {
            id: r.id,
            created_at: r.created_at,
            query: r.query,
            day: r.day,
            llm_response_parsed: parsed,
        });

        if out.len() >= limit {
            break;
        }
    }

    out.reverse();
    out
}

fn render_json_brief(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.to_string(),
        serde_json::Value::Object(map) => {
            let mut out = String::new();
            for key in ["hypotheses", "risks", "questions", "next_steps"] {
                if let Some(v) = map.get(key) {
                    let rendered = serde_json::to_string_pretty(v).unwrap_or_default();
                    if rendered.trim().is_empty() || rendered == "null" {
                        continue;
                    }
                    out.push_str(&format!("{key}:\n{rendered}\n"));
                }
            }
            if out.trim().is_empty() {
                serde_json::to_string_pretty(value).unwrap_or_default()
            } else {
                out
            }
        }
        _ => serde_json::to_string_pretty(value).unwrap_or_default(),
    }
}

fn indent_lines(s: &str, spaces: usize) -> String {
    let pad = " ".repeat(spaces);
    s.lines()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else {
                format!("{pad}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_chars(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    for c in s.chars().take(max) {
        out.push(c);
    }
    out
}

fn squash_ws(s: &str) -> String {
    let mut out = String::new();
    let mut prev_space = false;
    for c in s.chars() {
        let is_space = c.is_whitespace();
        if is_space {
            if !prev_space {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
        prev_space = is_space;
    }
    out.trim().to_string()
}
