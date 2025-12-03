use crate::models::{Dataset, ProbeMatch};
use crate::salience::tokenize;
use chrono::NaiveDate;

pub fn probe(
    dataset: &Dataset,
    query: &str,
    day: Option<NaiveDate>,
    limit: usize,
) -> Vec<ProbeMatch> {
    let q_tokens = tokenize(query);
    let q_set: std::collections::HashSet<_> = q_tokens.iter().cloned().collect();
    if q_set.is_empty() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    for session in &dataset.sessions {
        if let Some(day_filter) = day.or(dataset.day_filter) {
            if session.summary.started_at.date_naive() != day_filter {
                continue;
            }
        }

        for turn in &session.turns {
            let overlap: usize = turn.tokens.intersection(&q_set).count();
            if overlap == 0 {
                continue;
            }
            let coverage = overlap as f32 / q_set.len().max(1) as f32;
            let score = coverage * 2.0 + turn.salience * 0.3 + session.summary.score * 0.05;
            matches.push(ProbeMatch {
                session_id: session.summary.session_id.clone(),
                source_tool: session.summary.source_tool.clone(),
                project_context: session.summary.project_context.clone(),
                timestamp: turn.turn.timestamp,
                role: turn.turn.role.clone(),
                content_snippet: turn.turn.content_snippet.clone(),
                score,
                cues: turn.cues.clone(),
            });
        }
    }

    matches.sort_by(|a, b| b.score.total_cmp(&a.score));
    matches.truncate(limit);
    matches
}

pub fn build_probe_prompt(query: &str, matches: &[ProbeMatch]) -> Option<String> {
    if matches.is_empty() {
        return None;
    }
    let mut prompt = String::new();
    prompt.push_str("You are an analyst generating hypotheses and follow-up questions from prior AI coding sessions.\n");
    prompt.push_str("Use the snippets to infer goals, blockers, habits, and risks. Avoid restating; synthesize patterns.\n");
    prompt.push_str("Query:\n");
    prompt.push_str(query);
    prompt.push('\n');
    prompt.push_str("Snippets:\n");
    for m in matches.iter().take(6) {
        prompt.push_str(&format!(
            "- [{} @ {}] {} :: {}\n",
            m.session_id,
            m.timestamp,
            m.role,
            m.content_snippet.replace('\n', " ")
        ));
    }
    prompt.push_str(
        "\nRespond with JSON: {\"hypotheses\":[...],\"risks\":[...],\"questions\":[...],\"next_steps\":[...]}.",
    );
    Some(prompt)
}
