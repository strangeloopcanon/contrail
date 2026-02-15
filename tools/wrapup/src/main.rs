use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use contrail_types::MasterLog;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

mod report;

#[derive(Debug, Default)]
struct SessionAgg {
    source_tool: String,
    session_id: String,
    project_counts: HashMap<String, usize>,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    turns: usize,
    interrupted: bool,
    clipboard_hits: usize,
    file_effects: usize,
    models: HashSet<String>,
    git_branches: HashSet<String>,
    // Cumulative tokens (Codex style - take max)
    token_cumulative_total_max: u64,
    token_cumulative_prompt_max: u64,
    token_cumulative_completion_max: u64,
    token_cumulative_cached_input_max: u64,
    token_cumulative_reasoning_output_max: u64,
    saw_token_cumulative: bool,
    // Per-turn tokens (Claude Code style - sum across session)
    token_sum_prompt: u64,
    token_sum_completion: u64,
    token_sum_cached_input: u64,
    token_sum_cache_creation: u64,
    saw_token_per_turn: bool,
}

#[derive(Debug, Serialize)]
pub struct TopEntry {
    pub key: String,
    pub count: u64,
}

#[derive(Debug, Serialize, Clone)]
pub struct LongestSession {
    pub source_tool: String,
    pub session_id: String,
    pub project_context: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub duration_seconds: i64,
    pub turns: u64,
}

#[derive(Debug, Serialize)]
pub struct TokensSummary {
    pub sessions_with_token_counts: u64,
    pub total_tokens: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_output_tokens: u64,
}

#[derive(Debug, Serialize)]
pub struct CursorUsageSummary {
    pub team_id: u32,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_write_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cost_cents: Option<f64>,
    pub by_model: Vec<CursorModelUsage>,
}

#[derive(Debug, Serialize)]
pub struct CursorModelUsage {
    pub model_intent: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    pub total_cents: Option<f64>,
    pub request_cost: Option<f64>,
    pub tier: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct Wrapup {
    pub year: i32,
    pub range_start: Option<DateTime<Utc>>,
    pub range_end: Option<DateTime<Utc>>,
    pub turns_total: u64,
    pub sessions_total: u64,
    pub turns_by_tool: Vec<TopEntry>,
    pub sessions_by_tool: Vec<TopEntry>,
    pub roles: Vec<TopEntry>,
    pub active_days: u64,
    pub longest_streak_days: u64,
    pub busiest_day: Option<String>,
    pub busiest_day_turns: Option<u64>,
    pub peak_hour_local: Option<u32>,
    pub peak_hour_turns: Option<u64>,
    pub top_projects_by_turns: Vec<TopEntry>,
    pub top_projects_by_sessions: Vec<TopEntry>,
    pub top_models: Vec<TopEntry>,
    pub tokens: TokensSummary,
    pub cursor_usage: Option<CursorUsageSummary>,
    pub redacted_turns: u64,
    pub redacted_labels: Vec<TopEntry>,
    pub clipboard_hits: u64,
    pub file_effects: u64,
    pub function_calls: u64,
    pub function_call_outputs: u64,
    pub apply_patch_calls: u64,
    pub antigravity_images: u64,
    pub unique_projects: u64,
    pub longest_session_by_duration: Option<LongestSession>,
    pub longest_session_by_turns: Option<LongestSession>,
    pub user_turns: u64,
    pub user_avg_words: Option<f64>,
    pub user_question_rate: Option<f64>,
    pub user_code_hint_rate: Option<f64>,
    pub hourly_activity: Vec<u64>,
    pub daily_activity: Vec<(String, u64)>,
    pub total_interrupts: u64,
    pub languages: Vec<TopEntry>,
}

fn main() -> Result<()> {
    let mut year: Option<i32> = None;
    let mut start: Option<DateTime<Utc>> = None;
    let mut end: Option<DateTime<Utc>> = None;
    let mut last_days: Option<i64> = None;
    let mut include_cursor_usage = false;
    let mut log_path: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut top_n: usize = 10;

    let mut args = std::env::args().skip(1).peekable();
    let mut html_path: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            "--year" => {
                let val = args.next().context("--year requires YYYY")?;
                year = Some(val.parse::<i32>().context("invalid --year")?);
            }
            "--start" => {
                let val = args
                    .next()
                    .context("--start requires DATE (YYYY-MM-DD) or RFC3339")?;
                start = Some(parse_date_arg(&val, DateBoundary::Start)?);
            }
            "--end" => {
                let val = args
                    .next()
                    .context("--end requires DATE (YYYY-MM-DD) or RFC3339")?;
                end = Some(parse_date_arg(&val, DateBoundary::End)?);
            }
            "--last-days" => {
                let val = args.next().context("--last-days requires N")?;
                last_days = Some(val.parse::<i64>().context("invalid --last-days")?);
            }
            "--cursor-usage" => {
                include_cursor_usage = true;
            }
            "--log" => {
                let val = args.next().context("--log requires PATH")?;
                log_path = Some(PathBuf::from(val));
            }
            "--out" => {
                let val = args.next().context("--out requires PATH")?;
                out_path = Some(PathBuf::from(val));
            }
            "--html" => {
                let val = args.next().context("--html requires PATH")?;
                html_path = Some(PathBuf::from(val));
            }
            "--top" => {
                let val = args.next().context("--top requires N")?;
                top_n = val.parse::<usize>().context("invalid --top")?;
            }
            other => {
                anyhow::bail!("unknown arg: {other} (use --help)");
            }
        }
    }

    if last_days.is_some() && (start.is_some() || end.is_some()) {
        anyhow::bail!("--last-days cannot be combined with --start/--end");
    }

    if last_days.is_some() && year.is_some() {
        anyhow::bail!("--last-days cannot be combined with --year");
    }

    if (start.is_some() || end.is_some()) && year.is_some() {
        anyhow::bail!("--start/--end cannot be combined with --year");
    }

    if let Some(days) = last_days {
        if days <= 0 {
            anyhow::bail!("--last-days must be a positive integer");
        }
        let range_end = Utc::now();
        let range_start = range_end - chrono::Duration::days(days);
        start = Some(range_start);
        end = Some(range_end);
    }

    let year = year.unwrap_or_else(|| {
        end.as_ref()
            .map(|d| d.year())
            .or_else(|| start.as_ref().map(|d| d.year()))
            .unwrap_or_else(|| Local::now().year())
    });
    let log_path = log_path.unwrap_or_else(default_log_path);
    let start_filter = start;
    let end_filter = end;
    let mut wrapup = compute_wrapup(&log_path, year, start_filter, end_filter, top_n)?;

    if include_cursor_usage {
        let (cursor_start, cursor_end) = resolve_cursor_usage_range(
            year,
            start_filter,
            end_filter,
            wrapup.range_start,
            wrapup.range_end,
        )?;
        let cursor_usage = fetch_cursor_usage(cursor_start, cursor_end)?;

        wrapup.tokens.total_tokens = wrapup
            .tokens
            .total_tokens
            .saturating_add(cursor_usage.total_input_tokens)
            .saturating_add(cursor_usage.total_output_tokens);
        wrapup.tokens.prompt_tokens = wrapup
            .tokens
            .prompt_tokens
            .saturating_add(cursor_usage.total_input_tokens);
        wrapup.tokens.completion_tokens = wrapup
            .tokens
            .completion_tokens
            .saturating_add(cursor_usage.total_output_tokens);
        wrapup.tokens.cached_input_tokens = wrapup
            .tokens
            .cached_input_tokens
            .saturating_add(cursor_usage.total_cache_read_tokens);

        wrapup.cursor_usage = Some(cursor_usage);
    }

    if let Some(ref html_path) = html_path {
        let html = report::generate_html_report(&wrapup);
        if let Some(dir) = html_path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("create html output dir {:?}", dir))?;
        }
        let mut file = File::create(html_path).with_context(|| format!("write {:?}", html_path))?;
        file.write_all(html.as_bytes())?;
        println!("Wrote HTML wrapup to {:?}", html_path);
    }

    let out = serde_json::to_string_pretty(&wrapup)?;
    if let Some(out_path) = out_path {
        if let Some(dir) = out_path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create output dir {:?}", dir))?;
        }
        let mut file = File::create(&out_path).with_context(|| format!("write {:?}", out_path))?;
        file.write_all(out.as_bytes())?;
        file.write_all(b"\n")?;
        println!("Wrote JSON wrapup to {:?}", out_path);
    } else if out_path.is_none() && html_path.is_none() {
        // Only print JSON to stdout if no output files specified
        println!("{out}");
    }
    Ok(())
}

fn print_help() {
    println!(
        r#"contrail wrapup

Usage:
  cargo run -p wrapup -- --year 2025
  cargo run -p wrapup -- --last-days 30

Options:
  --year YYYY     Year filter (default: current year)
  --start DATE    Range start (YYYY-MM-DD or RFC3339); cannot combine with --year/--last-days
  --end DATE      Range end (YYYY-MM-DD or RFC3339); cannot combine with --year/--last-days
  --last-days N   Range end=now, start=now-N days; cannot combine with --year/--start/--end
  --cursor-usage  Fetch Cursor token usage from Cursor backend API (requires Cursor login; uses local access token)
  --log PATH      Master log path (default: ~/.contrail/logs/master_log.jsonl or $CONTRAIL_LOG_PATH)
  --out PATH      Write JSON output to a file (default: stdout)
  --html PATH     Write HTML report to a file
  --top N         Top-N lists size (default: 10)
"#
    );
}

#[derive(Clone, Copy)]
enum DateBoundary {
    Start,
    End,
}

fn parse_date_arg(input: &str, boundary: DateBoundary) -> Result<DateTime<Utc>> {
    if let Ok(ts) = DateTime::parse_from_rfc3339(input) {
        return Ok(ts.with_timezone(&Utc));
    }

    let date = chrono::NaiveDate::parse_from_str(input, "%Y-%m-%d").context("invalid date")?;
    let time = match boundary {
        DateBoundary::Start => chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
        DateBoundary::End => chrono::NaiveTime::from_hms_nano_opt(23, 59, 59, 999_999_999).unwrap(),
    };

    Ok(DateTime::<Utc>::from_naive_utc_and_offset(
        chrono::NaiveDateTime::new(date, time),
        Utc,
    ))
}

fn default_log_path() -> PathBuf {
    if let Ok(path) = std::env::var("CONTRAIL_LOG_PATH")
        && !path.trim().is_empty()
    {
        return PathBuf::from(path);
    }
    let home = dirs::home_dir().expect("Could not find home directory");
    home.join(".contrail/logs/master_log.jsonl")
}

fn compute_wrapup(
    log_path: &Path,
    year: i32,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    top_n: usize,
) -> Result<Wrapup> {
    let file = File::open(log_path).with_context(|| format!("open {:?}", log_path))?;
    let reader = BufReader::new(file);

    let mut turns_total: u64 = 0;
    let mut roles: HashMap<String, u64> = HashMap::new();
    let mut turns_by_tool: HashMap<String, u64> = HashMap::new();
    let mut daily_turns: BTreeMap<chrono::NaiveDate, u64> = BTreeMap::new();
    let mut hourly: HashMap<u32, u64> = HashMap::new();
    let mut model_counts: HashMap<String, u64> = HashMap::new();
    let mut project_turns_by_session: HashMap<String, u64> = HashMap::new();
    let mut redacted_turns: u64 = 0;
    let mut redacted_labels: HashMap<String, u64> = HashMap::new();
    let mut clipboard_hits: u64 = 0;
    let mut file_effects: u64 = 0;
    let mut function_calls: u64 = 0;
    let mut function_call_outputs: u64 = 0;
    let mut apply_patch_calls: u64 = 0;
    let mut antigravity_images: u64 = 0;
    let mut language_counts: HashMap<String, u64> = HashMap::new();

    let mut user_turns: u64 = 0;
    let mut user_words: u64 = 0;
    let mut user_questions: u64 = 0;
    let mut user_code_hints: u64 = 0;

    let mut range_start: Option<DateTime<Utc>> = None;
    let mut range_end: Option<DateTime<Utc>> = None;

    let mut sessions: HashMap<(String, String), SessionAgg> = HashMap::new();

    // For session splitting
    let mut last_seen_map: HashMap<(String, String), DateTime<Utc>> = HashMap::new();
    let mut sub_session_index_map: HashMap<(String, String), usize> = HashMap::new();

    for line in reader.lines() {
        let line = line?;
        let log = match serde_json::from_str::<MasterLog>(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if start.is_some() || end.is_some() {
            if start.is_some_and(|s| log.timestamp < s) {
                continue;
            }
            if end.is_some_and(|e| log.timestamp > e) {
                continue;
            }
        } else if log.timestamp.year() != year {
            continue;
        }

        // Determine Effective Session ID (Time-Gap Split)
        let raw_key = (log.source_tool.clone(), log.session_id.clone());
        let last_ts = *last_seen_map.get(&raw_key).unwrap_or(&log.timestamp);

        let gap = log.timestamp.signed_duration_since(last_ts);
        if gap > chrono::Duration::minutes(30) {
            *sub_session_index_map.entry(raw_key.clone()).or_insert(0) += 1;
        }
        last_seen_map.insert(raw_key.clone(), log.timestamp);

        let sub_idx = *sub_session_index_map.get(&raw_key).unwrap_or(&0);
        let effective_session_id = if sub_idx > 0 {
            format!("{}#{}", log.session_id, sub_idx)
        } else {
            log.session_id.clone()
        };

        turns_total += 1;
        let local_ts = log.timestamp.with_timezone(&Local);
        *daily_turns.entry(local_ts.date_naive()).or_insert(0) += 1;
        *hourly.entry(local_ts.hour()).or_insert(0) += 1;

        range_start = Some(range_start.map_or(log.timestamp, |v| v.min(log.timestamp)));
        range_end = Some(range_end.map_or(log.timestamp, |v| v.max(log.timestamp)));

        *turns_by_tool.entry(log.source_tool.clone()).or_insert(0) += 1;
        *roles.entry(log.interaction.role.clone()).or_insert(0) += 1;

        if log.security_flags.has_pii {
            redacted_turns += 1;
        }
        for label in &log.security_flags.redacted_secrets {
            *redacted_labels.entry(label.clone()).or_insert(0) += 1;
        }

        let meta_obj = log.metadata.as_object();
        if let Some(obj) = meta_obj {
            if obj
                .get("copied_to_clipboard")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                clipboard_hits += 1;
            }
            if let Some(arr) = obj.get("file_effects").and_then(Value::as_array) {
                file_effects += arr.len() as u64;
                for effect in arr {
                    // Try to get path as string or object field
                    let path_str = effect
                        .as_str()
                        .or_else(|| effect.get("path").and_then(Value::as_str));

                    if let Some(path) = path_str
                        && let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str())
                    {
                        let ext = ext.to_lowercase();
                        if !matches!(
                            ext.as_str(),
                            "json" | "md" | "txt" | "csv" | "png" | "jpg" | "lock"
                        ) {
                            *language_counts.entry(ext).or_insert(0) += 1;
                        }
                    }
                }
            }
            if obj
                .get("interrupted")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let key = (log.source_tool.clone(), effective_session_id.clone());
                let sess = sessions.entry(key).or_insert_with(|| SessionAgg {
                    source_tool: log.source_tool.clone(),
                    session_id: effective_session_id.clone(),
                    ..Default::default()
                });
                sess.interrupted = true;
            }
            if let Some(model) = obj.get("model").and_then(Value::as_str) {
                let model = model.trim();
                if !model.is_empty() {
                    *model_counts.entry(model.to_string()).or_insert(0) += 1;
                }
            }

            if log.source_tool == "antigravity"
                && let Some(n) = obj
                    .get("antigravity_image_count")
                    .and_then(Value::as_u64)
                    .or_else(|| {
                        obj.get("antigravity_image_count")
                            .and_then(Value::as_i64)
                            .and_then(|v| u64::try_from(v).ok())
                    })
            {
                antigravity_images = antigravity_images.saturating_add(n);
            }
        }

        if log.interaction.role == "user" {
            user_turns += 1;
            user_words += word_count(&log.interaction.content) as u64;
            if log.interaction.content.contains('?') {
                user_questions += 1;
            }
            if looks_like_code(&log.interaction.content) {
                user_code_hints += 1;
            }
        }

        // Session aggregation
        let key = (log.source_tool.clone(), effective_session_id.clone());
        let sess = sessions.entry(key).or_insert_with(|| SessionAgg {
            source_tool: log.source_tool.clone(),
            session_id: effective_session_id.clone(),
            ..Default::default()
        });
        sess.turns += 1;
        *sess
            .project_counts
            .entry(log.project_context.clone())
            .or_insert(0) += 1;
        sess.started_at = Some(
            sess.started_at
                .map_or(log.timestamp, |v| v.min(log.timestamp)),
        );
        sess.ended_at = Some(
            sess.ended_at
                .map_or(log.timestamp, |v| v.max(log.timestamp)),
        );

        if let Some(obj) = meta_obj {
            if let Some(arr) = obj.get("file_effects").and_then(Value::as_array) {
                sess.file_effects += arr.len();
            }
            if obj
                .get("copied_to_clipboard")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                sess.clipboard_hits += 1;
            }
            if let Some(branch) = obj.get("git_branch").and_then(Value::as_str) {
                let branch = branch.trim();
                if !branch.is_empty() {
                    sess.git_branches.insert(branch.to_string());
                }
            }
            if let Some(model) = obj.get("model").and_then(Value::as_str) {
                let model = model.trim();
                if !model.is_empty() {
                    sess.models.insert(model.to_string());
                }
            }

            update_cumulative_tokens_from_metadata(sess, obj);
        }

        // Token_count events in Codex logs may be stored as raw JSON content.
        if log.source_tool == "codex-cli"
            && log.interaction.content.contains("\"token_count\"")
            && let Some(usage) = extract_token_count_from_content(&log.interaction.content)
        {
            sess.saw_token_cumulative = true;
            sess.token_cumulative_total_max = sess.token_cumulative_total_max.max(usage.total);
            sess.token_cumulative_prompt_max = sess.token_cumulative_prompt_max.max(usage.prompt);
            sess.token_cumulative_completion_max =
                sess.token_cumulative_completion_max.max(usage.completion);
            sess.token_cumulative_cached_input_max = sess
                .token_cumulative_cached_input_max
                .max(usage.cached_input);
            sess.token_cumulative_reasoning_output_max = sess
                .token_cumulative_reasoning_output_max
                .max(usage.reasoning_output);
        }

        // Count Codex function calls + apply_patch calls for “fascinating” stats.
        if log.source_tool == "codex-cli"
            && log.interaction.content.contains("\"type\"")
            && let Ok(value) = serde_json::from_str::<Value>(&log.interaction.content)
        {
            if value.get("type").and_then(Value::as_str) == Some("function_call_output") {
                function_call_outputs += 1;
            }
            if value.get("type").and_then(Value::as_str) == Some("function_call") {
                function_calls += 1;
                if let Some(args) = value.get("arguments").and_then(Value::as_str)
                    && args.contains("apply_patch")
                {
                    apply_patch_calls += 1;
                }
            }
        }
    }

    let sessions_total = sessions.len() as u64;

    let sessions_by_tool = top_entries(
        sessions
            .values()
            .fold(HashMap::<String, u64>::new(), |mut acc, sess| {
                *acc.entry(sess.source_tool.clone()).or_insert(0) += 1;
                acc
            }),
        top_n,
    );
    let turns_by_tool = top_entries(turns_by_tool, top_n);
    let roles = top_entries(roles, top_n);
    let top_models = top_entries(model_counts, top_n);
    let redacted_labels = top_entries(redacted_labels, top_n);

    let active_days = daily_turns.len() as u64;
    let longest_streak_days = longest_streak(daily_turns.keys().copied().collect::<Vec<_>>());

    let (busiest_day, busiest_day_turns) = daily_turns
        .iter()
        .max_by_key(|(_, c)| *c)
        .map(|(d, c)| (Some(d.to_string()), Some(*c)))
        .unwrap_or((None, None));

    let (peak_hour_local, peak_hour_turns) = hourly
        .iter()
        .max_by_key(|(_, c)| *c)
        .map(|(h, c)| (Some(*h), Some(*c)))
        .unwrap_or((None, None));

    let mut project_sessions: HashMap<String, u64> = HashMap::new();
    for sess in sessions.values() {
        let project = pick_project_context(&sess.project_counts);
        if is_generic_project_context(&project) {
            continue;
        }
        *project_sessions.entry(project.clone()).or_insert(0) += 1;
        *project_turns_by_session.entry(project).or_insert(0) += sess.turns as u64;
    }

    let unique_projects = project_turns_by_session.len() as u64;
    let top_projects_by_turns = top_entries(project_turns_by_session, top_n);
    let top_projects_by_sessions = top_entries(project_sessions, top_n);

    let (longest_session_by_duration, longest_session_by_turns) =
        compute_longest_sessions(&sessions);

    let tokens = summarize_tokens(&sessions);

    // Aggregates
    let total_interrupts = sessions.values().filter(|s| s.interrupted).count() as u64;

    let mut hourly_activity = vec![0u64; 24];
    for (hour, count) in hourly {
        if hour < 24 {
            hourly_activity[hour as usize] = count;
        }
    }

    let daily_activity: Vec<(String, u64)> = daily_turns
        .into_iter()
        .map(|(d, c)| (d.format("%Y-%m-%d").to_string(), c))
        .collect();

    Ok(Wrapup {
        year,
        range_start,
        range_end,
        turns_total,
        sessions_total,
        turns_by_tool,
        sessions_by_tool,
        roles,
        active_days,
        longest_streak_days,
        busiest_day,
        busiest_day_turns,
        peak_hour_local,
        peak_hour_turns,
        top_projects_by_turns,
        top_projects_by_sessions,
        top_models,
        tokens,
        cursor_usage: None,
        redacted_turns,
        redacted_labels,
        clipboard_hits,
        file_effects,
        function_calls,
        function_call_outputs,
        apply_patch_calls,
        antigravity_images,
        unique_projects,
        longest_session_by_duration,
        longest_session_by_turns,
        user_turns,
        user_avg_words: rate(user_words, user_turns),
        user_question_rate: pct(user_questions, user_turns),
        user_code_hint_rate: pct(user_code_hints, user_turns),
        hourly_activity,
        daily_activity,
        total_interrupts,
        languages: top_entries(language_counts, top_n),
    })
}

fn resolve_cursor_usage_range(
    year: i32,
    requested_start: Option<DateTime<Utc>>,
    requested_end: Option<DateTime<Utc>>,
    observed_start: Option<DateTime<Utc>>,
    observed_end: Option<DateTime<Utc>>,
) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    if let (Some(start), Some(end)) = (requested_start, requested_end) {
        anyhow::ensure!(end >= start, "cursor usage range end must be >= start");
        return Ok((start, end));
    }

    if requested_start.is_some() || requested_end.is_some() {
        anyhow::bail!("--cursor-usage requires both --start and --end (or use --last-days)");
    }

    if let (Some(start), Some(end)) = (observed_start, observed_end) {
        anyhow::ensure!(end >= start, "cursor usage range end must be >= start");
        return Ok((start, end));
    }

    let start = DateTime::<Utc>::from_naive_utc_and_offset(
        chrono::NaiveDate::from_ymd_opt(year, 1, 1)
            .context("invalid year")?
            .and_hms_opt(0, 0, 0)
            .unwrap(),
        Utc,
    );
    let end = DateTime::<Utc>::from_naive_utc_and_offset(
        chrono::NaiveDate::from_ymd_opt(year, 12, 31)
            .context("invalid year")?
            .and_hms_nano_opt(23, 59, 59, 999_999_999)
            .unwrap(),
        Utc,
    );
    Ok((start, end))
}

#[derive(Debug, Deserialize)]
struct CursorAggregatedUsageResponse {
    #[serde(default)]
    aggregations: Vec<CursorAggregatedModelUsage>,
    #[serde(default, rename = "totalInputTokens")]
    total_input_tokens: String,
    #[serde(default, rename = "totalOutputTokens")]
    total_output_tokens: String,
    #[serde(default, rename = "totalCacheWriteTokens")]
    total_cache_write_tokens: String,
    #[serde(default, rename = "totalCacheReadTokens")]
    total_cache_read_tokens: String,
    #[serde(default, rename = "totalCostCents")]
    total_cost_cents: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct CursorAggregatedModelUsage {
    #[serde(default, rename = "modelIntent")]
    model_intent: String,
    #[serde(default, rename = "inputTokens")]
    input_tokens: Option<String>,
    #[serde(default, rename = "outputTokens")]
    output_tokens: Option<String>,
    #[serde(default, rename = "cacheWriteTokens")]
    cache_write_tokens: Option<String>,
    #[serde(default, rename = "cacheReadTokens")]
    cache_read_tokens: Option<String>,
    #[serde(default, rename = "totalCents")]
    total_cents: Option<f64>,
    #[serde(default, rename = "requestCost")]
    request_cost: Option<f64>,
    #[serde(default)]
    tier: Option<u32>,
}

fn fetch_cursor_usage(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<CursorUsageSummary> {
    let token = read_cursor_access_token()?;
    let client = reqwest::blocking::Client::new();

    let resp = client
        .post("https://api2.cursor.sh/aiserver.v1.DashboardService/GetAggregatedUsageEvents")
        .bearer_auth(token)
        .header("Connect-Protocol-Version", "1")
        .json(&serde_json::json!({
            "teamId": 0,
            "startDate": start.timestamp_millis().to_string(),
            "endDate": end.timestamp_millis().to_string(),
        }))
        .send()
        .context("Cursor usage request failed")?;

    if !resp.status().is_success() {
        anyhow::bail!("Cursor usage request failed: HTTP {}", resp.status());
    }

    let parsed: CursorAggregatedUsageResponse = resp.json().context("parse Cursor usage JSON")?;

    let by_model = parsed
        .aggregations
        .into_iter()
        .map(|m| CursorModelUsage {
            model_intent: m.model_intent,
            input_tokens: parse_u64_opt(m.input_tokens),
            output_tokens: parse_u64_opt(m.output_tokens),
            cache_write_tokens: parse_u64_opt(m.cache_write_tokens),
            cache_read_tokens: parse_u64_opt(m.cache_read_tokens),
            total_cents: m.total_cents,
            request_cost: m.request_cost,
            tier: m.tier,
        })
        .collect();

    Ok(CursorUsageSummary {
        team_id: 0,
        start,
        end,
        total_input_tokens: parse_u64(&parsed.total_input_tokens),
        total_output_tokens: parse_u64(&parsed.total_output_tokens),
        total_cache_write_tokens: parse_u64(&parsed.total_cache_write_tokens),
        total_cache_read_tokens: parse_u64(&parsed.total_cache_read_tokens),
        total_cost_cents: parsed.total_cost_cents,
        by_model,
    })
}

fn read_cursor_access_token() -> Result<String> {
    let home = dirs::home_dir().context("could not resolve home directory")?;
    let db_path = home.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb");

    let conn = rusqlite::Connection::open(&db_path)
        .with_context(|| format!("open Cursor globalStorage DB: {:?}", db_path))?;

    let mut stmt = conn
        .prepare("SELECT value FROM ItemTable WHERE key = 'cursorAuth/accessToken'")
        .context("prepare Cursor access token query")?;

    let token = stmt
        .query_row([], |row| {
            use rusqlite::types::ValueRef;
            let value = row.get_ref(0)?;
            let data_type = value.data_type();
            match value {
                ValueRef::Text(s) => Ok(String::from_utf8_lossy(s).into_owned()),
                ValueRef::Blob(b) => Ok(String::from_utf8_lossy(b).into_owned()),
                _ => Err(rusqlite::Error::InvalidColumnType(
                    0,
                    "value".to_string(),
                    data_type,
                )),
            }
        })
        .context("cursorAuth/accessToken not found (are you logged into Cursor?)")?;

    anyhow::ensure!(!token.trim().is_empty(), "cursorAuth/accessToken was empty");

    Ok(token)
}

fn parse_u64(s: &str) -> u64 {
    s.trim().parse::<u64>().unwrap_or(0)
}

fn parse_u64_opt(s: Option<String>) -> u64 {
    s.as_deref().map(parse_u64).unwrap_or(0)
}

fn is_generic_project_context(project_context: &str) -> bool {
    matches!(
        project_context,
        "Imported History" | "Codex Session" | "Unknown" | "Claude Global" | "Antigravity Brain"
    )
}

fn top_entries(map: HashMap<String, u64>, top_n: usize) -> Vec<TopEntry> {
    let mut items: Vec<(String, u64)> = map.into_iter().collect();
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    items
        .into_iter()
        .take(top_n)
        .map(|(k, v)| TopEntry { key: k, count: v })
        .collect()
}

fn longest_streak(mut dates: Vec<chrono::NaiveDate>) -> u64 {
    if dates.is_empty() {
        return 0;
    }
    dates.sort();
    let mut best = 1u64;
    let mut current = 1u64;
    for w in dates.windows(2) {
        let prev = w[0];
        let next = w[1];
        if next == prev + chrono::Days::new(1) {
            current += 1;
        } else {
            best = best.max(current);
            current = 1;
        }
    }
    best.max(current)
}

fn pick_project_context(counts: &HashMap<String, usize>) -> String {
    const GENERIC: &[&str] = &[
        "Imported History",
        "Codex Session",
        "Unknown",
        "Claude Global",
        "Antigravity Brain",
    ];

    let mut entries: Vec<(&String, &usize)> = counts.iter().collect();
    entries.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

    for (ctx, _) in &entries {
        if !GENERIC.contains(&ctx.as_str()) {
            return (*ctx).clone();
        }
    }
    entries
        .first()
        .map(|(ctx, _)| (*ctx).clone())
        .unwrap_or_else(|| "Unknown".to_string())
}

fn compute_longest_sessions(
    sessions: &HashMap<(String, String), SessionAgg>,
) -> (Option<LongestSession>, Option<LongestSession>) {
    let mut best_duration: Option<LongestSession> = None;
    let mut best_turns: Option<LongestSession> = None;

    for sess in sessions.values() {
        let (Some(start), Some(end)) = (sess.started_at, sess.ended_at) else {
            continue;
        };
        let duration_seconds = (end - start).num_seconds();
        let project = pick_project_context(&sess.project_counts);

        let candidate = LongestSession {
            source_tool: sess.source_tool.clone(),
            session_id: sess.session_id.clone(),
            project_context: project,
            started_at: start,
            ended_at: end,
            duration_seconds,
            turns: sess.turns as u64,
        };

        if best_duration
            .as_ref()
            .is_none_or(|b| candidate.duration_seconds > b.duration_seconds)
        {
            best_duration = Some(candidate.clone());
        }
        if best_turns
            .as_ref()
            .is_none_or(|b| candidate.turns > b.turns)
        {
            best_turns = Some(candidate);
        }
    }

    (best_duration, best_turns)
}

fn summarize_tokens(sessions: &HashMap<(String, String), SessionAgg>) -> TokensSummary {
    let mut sessions_with = 0u64;
    let mut total = 0u64;
    let mut prompt = 0u64;
    let mut completion = 0u64;
    let mut cached_input = 0u64;
    let mut reasoning_output = 0u64;

    for sess in sessions.values() {
        // Cumulative tokens (Codex style)
        if sess.saw_token_cumulative && sess.token_cumulative_total_max > 0 {
            sessions_with += 1;
            total += sess.token_cumulative_total_max;
            prompt += sess.token_cumulative_prompt_max;
            completion += sess.token_cumulative_completion_max;
            cached_input += sess.token_cumulative_cached_input_max;
            reasoning_output += sess.token_cumulative_reasoning_output_max;
        }
        // Per-turn tokens (Claude Code style) - sum them up
        else if sess.saw_token_per_turn
            && (sess.token_sum_prompt > 0 || sess.token_sum_completion > 0)
        {
            sessions_with += 1;
            let session_total = sess.token_sum_prompt + sess.token_sum_completion;
            total += session_total;
            prompt += sess.token_sum_prompt;
            completion += sess.token_sum_completion;
            cached_input += sess.token_sum_cached_input;
            // Note: cache_creation tokens are counted separately, not added to cached_input
        }
    }

    TokensSummary {
        sessions_with_token_counts: sessions_with,
        total_tokens: total,
        prompt_tokens: prompt,
        completion_tokens: completion,
        cached_input_tokens: cached_input,
        reasoning_output_tokens: reasoning_output,
    }
}

fn update_cumulative_tokens_from_metadata(
    sess: &mut SessionAgg,
    meta: &serde_json::Map<String, Value>,
) {
    let read_u64 = |key: &str| {
        meta.get(key).and_then(Value::as_u64).or_else(|| {
            meta.get(key)
                .and_then(Value::as_i64)
                .and_then(|n| u64::try_from(n).ok())
        })
    };

    // Cumulative tokens (Codex style) - take max
    let total = read_u64("usage_cumulative_total_tokens").unwrap_or(0);
    if total > 0 {
        sess.saw_token_cumulative = true;
        sess.token_cumulative_total_max = sess.token_cumulative_total_max.max(total);
        sess.token_cumulative_prompt_max = sess
            .token_cumulative_prompt_max
            .max(read_u64("usage_cumulative_prompt_tokens").unwrap_or(0));
        sess.token_cumulative_completion_max = sess
            .token_cumulative_completion_max
            .max(read_u64("usage_cumulative_completion_tokens").unwrap_or(0));
        sess.token_cumulative_cached_input_max = sess
            .token_cumulative_cached_input_max
            .max(read_u64("usage_cumulative_cached_input_tokens").unwrap_or(0));
        sess.token_cumulative_reasoning_output_max = sess
            .token_cumulative_reasoning_output_max
            .max(read_u64("usage_cumulative_reasoning_output_tokens").unwrap_or(0));
    }

    // Per-turn tokens (Claude Code style) - sum across session
    let prompt_turn = read_u64("usage_prompt_tokens").unwrap_or(0);
    let completion_turn = read_u64("usage_completion_tokens").unwrap_or(0);
    if prompt_turn > 0 || completion_turn > 0 {
        sess.saw_token_per_turn = true;
        sess.token_sum_prompt += prompt_turn;
        sess.token_sum_completion += completion_turn;
        sess.token_sum_cached_input += read_u64("usage_cached_input_tokens").unwrap_or(0);
        sess.token_sum_cache_creation += read_u64("usage_cache_creation_tokens").unwrap_or(0);
    }
}

#[derive(Debug)]
struct TokenCountUsage {
    total: u64,
    prompt: u64,
    completion: u64,
    cached_input: u64,
    reasoning_output: u64,
}

fn extract_token_count_from_content(content: &str) -> Option<TokenCountUsage> {
    let value = serde_json::from_str::<Value>(content).ok()?;
    if value.get("type").and_then(Value::as_str)? != "event_msg" {
        return None;
    }
    if value.pointer("/payload/type").and_then(Value::as_str)? != "token_count" {
        return None;
    }
    let total = value
        .pointer("/payload/info/total_token_usage/total_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            value
                .pointer("/payload/info/total_token_usage/total_tokens")
                .and_then(Value::as_i64)
                .and_then(|n| u64::try_from(n).ok())
        })?;

    let prompt = value
        .pointer("/payload/info/total_token_usage/input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion = value
        .pointer("/payload/info/total_token_usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached_input = value
        .pointer("/payload/info/total_token_usage/cached_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reasoning_output = value
        .pointer("/payload/info/total_token_usage/reasoning_output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    Some(TokenCountUsage {
        total,
        prompt,
        completion,
        cached_input,
        reasoning_output,
    })
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

fn looks_like_code(text: &str) -> bool {
    if text.contains("```") {
        return true;
    }
    if text.contains("\n    ") || text.contains("\n\t") {
        return true;
    }
    for token in ["::", "->", "=>", "{", "}", ";", "&&", "||", "==", "!="] {
        if text.contains(token) {
            return true;
        }
    }
    false
}

fn rate(total_words: u64, n: u64) -> Option<f64> {
    if n == 0 {
        return None;
    }
    Some(total_words as f64 / n as f64)
}

fn pct(n: u64, d: u64) -> Option<f64> {
    if d == 0 {
        return None;
    }
    Some(100.0 * n as f64 / d as f64)
}
