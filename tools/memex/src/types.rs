use chrono::{DateTime, Utc};

/// A single turn in a conversation (one user or assistant message).
#[derive(Debug, Clone)]
pub struct Turn {
    pub role: String,
    pub content: String,
    #[allow(dead_code)]
    pub timestamp: Option<DateTime<Utc>>,
}

/// A complete session: a sequence of turns from one agent in one project.
#[derive(Debug, Clone)]
pub struct Session {
    pub tool: String,
    #[allow(dead_code)]
    pub session_id: String,
    #[allow(dead_code)]
    pub project_path: String,
    pub branch: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub turns: Vec<Turn>,
    pub files_changed: Vec<String>,
}

impl Session {
    pub fn filename(&self) -> String {
        // Prefer a stable timestamp for determinism; fall back to "unknown" if missing.
        let turns_earliest = self
            .turns
            .iter()
            .filter_map(|t| t.timestamp.as_ref())
            .min()
            .cloned();

        fn min_opt(
            a: Option<chrono::DateTime<Utc>>,
            b: Option<chrono::DateTime<Utc>>,
        ) -> Option<chrono::DateTime<Utc>> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        }

        let ts_opt = min_opt(
            min_opt(
                self.started_at.as_ref().cloned(),
                self.ended_at.as_ref().cloned(),
            ),
            turns_earliest,
        );

        let ts = ts_opt
            .map(|t| t.format("%Y-%m-%dT%H-%M-%S").to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let tool = sanitize_filename_component(&self.tool);
        let sid_raw = sanitize_filename_component(&self.session_id);
        let sid = if sid_raw.is_empty() {
            "unknown".to_string()
        } else {
            sid_raw
        };
        // Keep paths short and predictable; we only need enough to avoid collisions.
        let sid = truncate(&sid, 32);
        format!("{}_{}_{}.md", ts, tool, sid)
    }
}

fn sanitize_filename_component(input: &str) -> String {
    // Conservatively allow only common filename-safe characters.
    // Replace everything else (including path separators) with '-'.
    input
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '-',
        })
        .collect()
}

fn truncate(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Which agents are active for a given repo.
#[derive(Debug, Default)]
pub struct DetectedAgents {
    pub cursor: bool,
    pub codex: bool,
    pub claude: bool,
    pub gemini: bool,
}

impl DetectedAgents {
    pub fn any(&self) -> bool {
        self.cursor || self.codex || self.claude || self.gemini
    }
}
