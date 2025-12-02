use crate::models::{
    Dataset, ScoredTurn, SessionBundle, SessionSummary, TurnSummary,
};
use crate::salience::{score_session, score_turn, tokenize};
use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use scrapers::types::MasterLog;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

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

        let started_at = events
            .first()
            .map(|l| l.timestamp)
            .unwrap_or_else(Utc::now);
        let ended_at = events
            .last()
            .map(|l| l.timestamp)
            .unwrap_or_else(Utc::now);

        let project_context = pick_context(&project_context_counts).unwrap_or_else(|| {
            events
                .get(0)
                .map(|e| e.project_context.clone())
                .unwrap_or_else(|| "Unknown".to_string())
        });

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
