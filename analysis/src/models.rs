use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize)]
pub struct TurnSummary {
    pub event_id: String,
    pub timestamp: DateTime<Utc>,
    pub source_tool: String,
    pub session_id: String,
    pub project_context: String,
    pub role: String,
    pub content_snippet: String,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct ScoredTurn {
    pub turn: TurnSummary,
    pub tokens: HashSet<String>,
    pub salience: f32,
    pub cues: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionSummary {
    pub source_tool: String,
    pub session_id: String,
    pub project_context: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub turn_count: usize,
    pub interrupted: bool,
    pub file_effects: usize,
    pub clipboard_hits: usize,
    pub models: Vec<String>,
    pub git_branches: Vec<String>,
    pub score: f32,
}

#[derive(Clone, Debug)]
pub struct SessionBundle {
    pub summary: SessionSummary,
    pub turns: Vec<ScoredTurn>,
}

#[derive(Clone, Debug)]
pub struct Dataset {
    pub sessions: Vec<SessionBundle>,
    pub day_filter: Option<NaiveDate>,
}

#[derive(Debug, Serialize)]
pub struct SessionsResponse {
    pub sessions: Vec<SessionSummary>,
    pub day: Option<NaiveDate>,
}

#[derive(Debug, Serialize)]
pub struct SalientSession {
    pub session: SessionSummary,
    pub top_turns: Vec<TurnSummary>,
}

#[derive(Debug, Serialize)]
pub struct SalientResponse {
    pub sessions: Vec<SalientSession>,
    pub day: Option<NaiveDate>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProbeMatch {
    pub session_id: String,
    pub source_tool: String,
    pub project_context: String,
    pub timestamp: DateTime<Utc>,
    pub role: String,
    pub content_snippet: String,
    pub score: f32,
    pub cues: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ProbeResponse {
    pub query: String,
    pub matches: Vec<ProbeMatch>,
    pub prompt_suggestion: Option<String>,
    pub day: Option<NaiveDate>,
}

#[derive(Debug, Serialize)]
pub struct MemoriesResponse {
    pub memories: Vec<crate::memory::MemoryRecord>,
}
