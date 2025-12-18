use crate::models::{Dataset, ScoredTurn, SessionBundle, SessionSummary, TurnSummary};
use crate::salience::{score_session, score_turn, tokenize};
use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use scrapers::types::MasterLog;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::path::PathBuf;

pub fn load_dataset(log_path: &Path, day_filter: Option<NaiveDate>) -> Result<Dataset> {
    let file = File::open(log_path).context("open master_log.jsonl")?;
    let reader = BufReader::new(file);
    let mut logs = Vec::new();

    for line in reader.lines() {
        let line = line?;
        match serde_json::from_str::<MasterLog>(&line) {
            Ok(log) => {
                if let Some(day) = day_filter {
                    let ts_day = log.timestamp.date_naive();
                    if ts_day != day {
                        continue;
                    }
                }
                logs.push(log);
            }
            Err(err) => {
                eprintln!("Skipping malformed log line: {err}");
            }
        }
    }

    // Group by source + session_id
    let mut grouped: HashMap<(String, String), Vec<MasterLog>> = HashMap::new();
    for log in logs {
        let key = (log.source_tool.clone(), log.session_id.clone());
        grouped.entry(key).or_default().push(log);
    }

    let now = Utc::now();
    let mut sessions = Vec::new();

    for ((source_tool, session_id), mut events) in grouped {
        events.sort_by_key(|l| l.timestamp);

        let mut models = HashSet::new();
        let mut branches = HashSet::new();
        let mut file_effects_total = 0usize;
        let mut clipboard_hits = 0usize;
        let mut interrupted = false;
        let mut turns = Vec::new();
        let mut project_context_counts: HashMap<String, usize> = HashMap::new();

        for log in &events {
            *project_context_counts
                .entry(log.project_context.clone())
                .or_insert(0) += 1;
            let mut meta = log.metadata.clone();
            // Pull cues from metadata
            if let Some(obj) = meta.as_object_mut() {
                if obj
                    .get("interrupted")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    interrupted = true;
                }
                if let Some(arr) = obj.get("file_effects").and_then(|v| v.as_array()) {
                    file_effects_total += arr.len();
                }
                if obj
                    .get("copied_to_clipboard")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    clipboard_hits += 1;
                }
                if let Some(branch) = obj.get("git_branch").and_then(|v| v.as_str()) {
                    branches.insert(branch.trim().to_string());
                }
                if let Some(model) = obj.get("model").and_then(|v| v.as_str()) {
                    models.insert(model.to_string());
                }
            }

            let content_snippet = snippet(&log.interaction.content);
            let (turn_score, mut cues) =
                score_turn(&log.interaction.content, &log.interaction.role, &meta);
            let tokens = tokenize(&log.interaction.content)
                .into_iter()
                .collect::<HashSet<_>>();

            // Surface any metadata-derived cues
            if file_effects_total > 0 && !cues.contains(&"file_effects".to_string()) {
                cues.push("file_effects".to_string());
            }
            if interrupted && !cues.contains(&"interrupted".to_string()) {
                cues.push("interrupted".to_string());
            }

            turns.push(ScoredTurn {
                turn: TurnSummary {
                    event_id: log.event_id.to_string(),
                    timestamp: log.timestamp,
                    source_tool: source_tool.clone(),
                    session_id: session_id.clone(),
                    project_context: log.project_context.clone(),
                    role: log.interaction.role.clone(),
                    content_snippet,
                    metadata: meta,
                },
                tokens,
                salience: turn_score,
                cues,
            });
        }

        let started_at = events.first().map(|l| l.timestamp).unwrap_or_else(Utc::now);
        let ended_at = events.last().map(|l| l.timestamp).unwrap_or_else(Utc::now);

        let mut project_context = pick_best_project_context(&source_tool, &project_context_counts)
            .unwrap_or_else(|| {
                events
                    .first()
                    .map(|e| e.project_context.clone())
                    .unwrap_or_else(|| "Unknown".to_string())
            });

        if is_generic_project_context(&source_tool, &project_context)
            && let Some(inferred) = infer_project_context(&source_tool, &events)
        {
            project_context = inferred;
        }

        let mut summary = SessionSummary {
            source_tool: source_tool.clone(),
            session_id: session_id.clone(),
            project_context: project_context.clone(),
            started_at,
            ended_at,
            turn_count: turns.len(),
            interrupted,
            file_effects: file_effects_total,
            clipboard_hits,
            models: to_sorted_vec(models),
            git_branches: to_sorted_vec(branches),
            score: 0.0,
        };

        summary.score = score_session(&turns, &summary, now);
        sessions.push(SessionBundle { summary, turns });
    }

    // Order newest first by default
    sessions.sort_by(|a, b| b.summary.ended_at.cmp(&a.summary.ended_at));

    Ok(Dataset {
        sessions,
        day_filter,
    })
}

fn snippet(content: &str) -> String {
    let max_chars = 600usize;
    let mut out = String::new();
    for c in content.chars().take(max_chars) {
        out.push(c);
    }
    out
}

fn to_sorted_vec(set: HashSet<String>) -> Vec<String> {
    let mut v: Vec<String> = set.into_iter().collect();
    v.sort();
    v
}

fn pick_context(counts: &HashMap<String, usize>) -> Option<String> {
    counts
        .iter()
        .max_by_key(|(_, count)| *count)
        .map(|(ctx, _)| ctx.clone())
}

fn pick_best_project_context(source_tool: &str, counts: &HashMap<String, usize>) -> Option<String> {
    if source_tool == "codex-cli"
        && let Some((ctx, _)) = counts
            .iter()
            .filter(|(ctx, _)| !is_generic_project_context(source_tool, ctx))
            .max_by_key(|(_, count)| *count)
    {
        return Some(ctx.clone());
    }
    pick_context(counts)
}

fn is_generic_project_context(source_tool: &str, project_context: &str) -> bool {
    if source_tool != "codex-cli" {
        return false;
    }
    matches!(
        project_context,
        "Imported History" | "Codex Session" | "Unknown"
    )
}

fn infer_project_context(source_tool: &str, events: &[MasterLog]) -> Option<String> {
    if source_tool != "codex-cli" {
        return None;
    }
    infer_codex_project_context(events)
}

fn infer_codex_project_context(events: &[MasterLog]) -> Option<String> {
    static RE_GIT_C: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\bgit\s+-C\s+(?P<path>'[^']+'|"[^"]+"|\S+)"#).unwrap());
    static RE_CD: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\bcd\s+(?P<path>'[^']+'|"[^"]+"|[^&;\n]+)"#).unwrap());
    static RE_PATH_HINT: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(/(?:Users|Volumes|private|opt|tmp)/[^\n\r]+)").unwrap());

    for log in events.iter().take(250) {
        if let Some(obj) = log.metadata.as_object()
            && let Some(cwd) = obj.get("cwd").and_then(Value::as_str)
            && let Some(root) = project_root_from_path(cwd)
        {
            return Some(root);
        }

        if let Ok(value) = serde_json::from_str::<Value>(&log.interaction.content)
            && let Some(obj) = value.as_object()
        {
            if let Some(cwd) = obj.get("cwd").and_then(Value::as_str)
                && let Some(root) = project_root_from_path(cwd)
            {
                return Some(root);
            }

            let ty = obj.get("type").and_then(Value::as_str).unwrap_or("");
            if ty == "function_call"
                && let Some(args) = obj.get("arguments").and_then(Value::as_str)
                && let Some(path) = extract_shell_path(args, &RE_GIT_C, &RE_CD)
                && let Some(root) = project_root_from_path(&path)
            {
                return Some(root);
            }

            if ty == "function_call_output"
                && let Some(output) = obj.get("output").and_then(Value::as_str)
                && let Some(path) = extract_path_from_output(output, &RE_PATH_HINT)
                && let Some(root) = project_root_from_path(&path)
            {
                return Some(root);
            }
        }

        if let Some(path) = extract_path_hint(&log.interaction.content, &RE_PATH_HINT)
            && let Some(root) = project_root_from_path(&path)
        {
            return Some(root);
        }
    }
    None
}

fn extract_shell_path(args_json: &str, re_git_c: &Regex, re_cd: &Regex) -> Option<String> {
    let value = serde_json::from_str::<Value>(args_json).ok()?;
    let cmd = value.get("command")?.as_array()?;
    let joined = cmd
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");

    if let Some(cap) = re_git_c.captures(&joined) {
        let raw = cap.name("path")?.as_str();
        return Some(strip_wrapping_quotes(raw).to_string());
    }
    if let Some(cap) = re_cd.captures(&joined) {
        let raw = cap.name("path")?.as_str();
        return Some(strip_wrapping_quotes(raw).trim().to_string());
    }
    None
}

fn extract_path_from_output(output: &str, re_hint: &Regex) -> Option<String> {
    let output_text = serde_json::from_str::<Value>(output)
        .ok()
        .and_then(|v| {
            v.get("output")
                .and_then(Value::as_str)
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| output.to_string());

    for line in output_text.lines().take(2000) {
        if let Some(path) = extract_path_hint(line, re_hint) {
            return Some(path);
        }
    }
    extract_path_hint(&output_text, re_hint)
}

fn extract_path_hint(text: &str, re_hint: &Regex) -> Option<String> {
    let cap = re_hint.captures(text)?;
    let raw = cap.get(1)?.as_str();
    Some(trim_path_tail(raw).to_string())
}

fn strip_wrapping_quotes(s: &str) -> &str {
    let trimmed = s.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        let first = bytes[0];
        let last = bytes[trimmed.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &trimmed[1..trimmed.len() - 1];
        }
    }
    trimmed
}

fn trim_path_tail(s: &str) -> &str {
    s.trim()
        .trim_end_matches(['"', '\'', ')', ']', '}', ',', ';'])
}

fn project_root_from_path(raw: &str) -> Option<String> {
    let raw = strip_wrapping_quotes(raw);
    if raw.trim().is_empty() {
        return None;
    }
    let expanded = shellexpand::tilde(raw);
    let mut path = PathBuf::from(expanded.as_ref());
    if path.as_os_str().is_empty() {
        return None;
    }

    if path.extension().is_some() || path.is_file() {
        path.pop();
    }
    if path.as_os_str().is_empty() {
        return None;
    }

    let mut cur = path.clone();
    for _ in 0..15 {
        if cur.join(".git").is_dir() {
            return Some(cur.to_string_lossy().to_string());
        }
        if !cur.pop() {
            break;
        }
    }

    Some(path.to_string_lossy().to_string())
}
