use crate::types::Session;
use scrapers::sentry::Sentry;

/// Render a session as a readable markdown transcript.
pub fn render_session(session: &Session) -> String {
    let sentry = Sentry::new();
    let mut out = String::new();

    // Header
    let ts = session
        .started_at
        .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown time".to_string());
    out.push_str(&format!("# Session: {}\n", ts));

    let mut meta_parts = vec![format!("Tool: {}", session.tool)];
    if let Some(branch) = &session.branch {
        meta_parts.push(format!("Branch: {}", branch));
    }
    if let (Some(start), Some(end)) = (session.started_at, session.ended_at) {
        let dur = end.signed_duration_since(start);
        let mins = dur.num_minutes();
        if mins > 0 {
            meta_parts.push(format!("Duration: ~{} min", mins));
        }
    }
    out.push_str(&meta_parts.join(" | "));
    out.push_str("\n\n");

    // Turns
    for turn in &session.turns {
        out.push_str(&format!("## {}\n", turn.role));
        out.push_str(&turn.content);
        out.push_str("\n\n");
    }

    // Footer
    if !session.files_changed.is_empty() {
        out.push_str("---\n");
        out.push_str(&format!(
            "Files changed: {}\n",
            session.files_changed.join(", ")
        ));
    }

    // Redact secrets
    let (redacted, _flags) = sentry.scan_and_redact(&out);
    redacted
}
