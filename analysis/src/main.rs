mod context_pack;
mod ingest;
mod llm;
mod memory;
mod memory_blocks;
mod models;
mod salience;
mod search;

use crate::models::ScoredTurn;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post, put},
};
use chrono::NaiveDate;
use context_pack::ContextPackResponse;
use contrail_types::MasterLog;
use memory::{MemoryRecord, append_memory, read_memories};
use memory_blocks::{MemoryBlock, MemoryBlockUpdate};
use models::{
    Dataset, ProbeResponse, ProjectSummary, ProjectsResponse, SalientResponse, SalientSession,
    SessionsResponse, TurnSummary,
};
use serde::Deserialize;
use std::env;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tower_http::cors::CorsLayer;

#[derive(Clone)]
struct AppState {
    log_path: PathBuf,
    memory_path: PathBuf,
    memory_blocks_path: PathBuf,
    data: Arc<RwLock<Dataset>>,
    memory_io_lock: Arc<Mutex<()>>,
    llm: Option<llm::LlmClient>,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    err: anyhow::Error,
}

impl ApiError {
    fn bad_request(err: anyhow::Error) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            err,
        }
    }

    fn not_found(err: anyhow::Error) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            err,
        }
    }

    fn internal(err: anyhow::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            err,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = format!("error: {}", self.err);
        (self.status, body).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, Deserialize)]
struct DayLimitQuery {
    day: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
    sort: Option<String>,
    tool: Option<String>,
    project: Option<String>,
    refresh: Option<bool>,
    q: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MemoryBlockBody {
    label: String,
    value: String,
    project_context: Option<String>,
    source_tool: Option<String>,
    tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ContextPackQuery {
    day: Option<String>,
    refresh: Option<bool>,
    session_limit: Option<usize>,
    memory_limit: Option<usize>,
    include_memories: Option<bool>,
    include_memory_blocks: Option<bool>,
    format: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionEventsQuery {
    source_tool: String,
    session_id: String,
    max_content_chars: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct MemoryBody {
    q: String,
    limit: Option<usize>,
    day: Option<String>,
    llm_response: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct AutoProbeBody {
    q: String,
    limit: Option<usize>,
    day: Option<String>,
    model: Option<String>,
    temperature: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct DefaultAutoProbeBody {
    queries: Option<Vec<String>>,
    limit: Option<usize>,
    day: Option<String>,
    model: Option<String>,
    temperature: Option<f32>,
}

#[derive(Debug, serde::Serialize)]
struct ImportHistoryResponse {
    imported: usize,
    skipped: usize,
    errors: usize,
}

#[derive(Debug, Deserialize)]
struct ImportClaudeSetupBody {
    repo_root: Option<PathBuf>,
    source: Option<PathBuf>,
    scope: Option<String>,
    #[serde(default)]
    include_global: bool,
    #[serde(default)]
    dry_run: bool,
}

const DEFAULT_PROBES: &[&str] = &[
    "apply patch failed",
    "error",
    "panic",
    "exception",
    "interrupted",
    "rate limit",
    "function_call_output failed",
];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let log_path = env::var("CONTRAIL_LOG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .expect("Could not find home directory")
                .join(".contrail/logs/master_log.jsonl")
        });
    let memory_path = env::var("CONTRAIL_MEMORY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .expect("Could not find home directory")
                .join(".contrail/analysis/memories.jsonl")
        });
    let memory_blocks_path = env::var("CONTRAIL_MEMORY_BLOCKS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .expect("Could not find home directory")
                .join(".contrail/analysis/memory_blocks.json")
        });

    let initial_dataset = ingest::load_dataset(&log_path, None)?;
    let state = AppState {
        log_path,
        memory_path,
        memory_blocks_path,
        data: Arc::new(RwLock::new(initial_dataset)),
        memory_io_lock: Arc::new(Mutex::new(())),
        llm: llm::LlmClient::from_env()?,
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(|| async { "ok" }))
        .route("/api/sessions", get(get_sessions))
        .route("/api/projects", get(get_projects))
        .route("/api/salient", get(get_salient))
        .route("/api/probe", get(get_probe))
        .route("/api/session_events", get(get_session_events))
        .route("/api/context_pack", get(get_context_pack))
        .route("/api/import_history", post(import_history))
        .route("/api/import_claude_setup", post(import_claude_setup))
        .route(
            "/api/memory_blocks",
            get(list_memory_blocks).post(create_memory_block),
        )
        .route(
            "/api/memory_blocks/:id",
            put(update_memory_block).delete(delete_memory_block),
        )
        .route("/api/memories", get(list_memories).post(create_memory))
        .route("/api/memories/autoprobe", post(create_memory_with_llm))
        .route(
            "/api/memories/autoprobe/defaults",
            post(run_default_autoprobes),
        )
        .with_state(state)
        .layer(CorsLayer::permissive());

    let bind_addr = env::var("ANALYSIS_BIND").unwrap_or_else(|_| "127.0.0.1:3210".to_string());
    println!("✈️  Contrail Analysis running at http://{bind_addr}");
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn index() -> Html<&'static str> {
    Html(include_str!("ade.html"))
}

async fn import_history(State(state): State<AppState>) -> ApiResult<Json<ImportHistoryResponse>> {
    let config = scrapers::config::ContrailConfig::from_env().map_err(ApiError::internal)?;
    let log_path = state.log_path.clone();

    let stats =
        tokio::task::spawn_blocking(move || scrapers::history_import::import_history(&config))
            .await
            .map_err(|e| ApiError::internal(anyhow::anyhow!("join error: {e}")))?
            .map_err(ApiError::internal)?;

    // Best-effort: match core_daemon behavior so it won't re-import.
    if let Some(home) = dirs::home_dir() {
        let marker_path = home.join(".contrail/state/history_import_done.json");
        if let Some(dir) = marker_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let completed_at_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
        let body = serde_json::json!({
            "completed_at_unix": completed_at_unix,
            "imported": stats.imported,
            "skipped": stats.skipped,
            "errors": stats.errors,
            "log_path": log_path,
            "note": "written by analysis /api/import_history",
        });
        let _ = std::fs::write(
            marker_path,
            serde_json::to_string_pretty(&body).unwrap_or_default(),
        );
    }

    Ok(Json(ImportHistoryResponse {
        imported: stats.imported,
        skipped: stats.skipped,
        errors: stats.errors,
    }))
}

async fn import_claude_setup(
    Json(body): Json<ImportClaudeSetupBody>,
) -> ApiResult<Json<scrapers::claude_profile_import::SetupReport>> {
    // Normalise: empty string → None, expand ~ for web callers
    let repo_root = body
        .repo_root
        .clone()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| expand_tilde_path(&p));
    let target = if let Some(root) = repo_root {
        scrapers::claude_profile_import::ImportTarget::Repo { repo_root: root }
    } else {
        scrapers::claude_profile_import::ImportTarget::Global
    };
    let scope = match body.scope.as_deref() {
        Some("broad") => scrapers::claude_profile_import::ImportScope::Broad,
        Some("full") => scrapers::claude_profile_import::ImportScope::Full,
        _ => scrapers::claude_profile_import::ImportScope::Curated,
    };

    let request = scrapers::claude_profile_import::SetupRequest {
        target,
        source: body.source.clone(),
        scope,
        include_global: body.include_global,
        dry_run: body.dry_run,
    };

    let report = tokio::task::spawn_blocking(move || {
        scrapers::claude_profile_import::setup_claude_profile(&request)
    })
    .await
    .map_err(|e| ApiError::internal(anyhow::anyhow!("join error: {e}")))?
    .map_err(ApiError::internal)?;

    Ok(Json(report))
}

async fn get_sessions(
    State(state): State<AppState>,
    Query(query): Query<DayLimitQuery>,
) -> ApiResult<Json<SessionsResponse>> {
    let day = parse_day(&query.day)?;
    let dataset = ensure_dataset(&state, day, query.refresh.unwrap_or(false)).await?;

    let tool_filter = query
        .tool
        .clone()
        .filter(|t| !t.trim().is_empty() && t != "all");

    let project_filter = query
        .project
        .clone()
        .filter(|p| !p.trim().is_empty() && p != "all");

    let mut sessions: Vec<_> = dataset
        .sessions
        .iter()
        .map(|s| s.summary.clone())
        .filter(|s| match tool_filter.as_deref() {
            Some(t) => s.source_tool == t,
            None => true,
        })
        .filter(|s| match project_filter.as_deref() {
            Some(p) => s.project_context == p,
            None => true,
        })
        .collect();

    let sort = query.sort.as_deref().unwrap_or("score");
    match sort {
        "recent" => sessions.sort_by(|a, b| b.ended_at.cmp(&a.ended_at)),
        _ => sessions.sort_by(|a, b| b.score.total_cmp(&a.score)),
    }

    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(200).clamp(1, 5000);
    let sessions = sessions.into_iter().skip(offset).take(limit).collect();

    Ok(Json(SessionsResponse {
        sessions,
        day: dataset.day_filter,
    }))
}

async fn get_projects(
    State(state): State<AppState>,
    Query(query): Query<DayLimitQuery>,
) -> ApiResult<Json<ProjectsResponse>> {
    let day = parse_day(&query.day)?;
    let dataset = ensure_dataset(&state, day, query.refresh.unwrap_or(false)).await?;

    let tool_filter = query
        .tool
        .clone()
        .filter(|t| !t.trim().is_empty() && t != "all");

    let mut map: std::collections::HashMap<String, ProjectSummary> =
        std::collections::HashMap::new();
    for bundle in &dataset.sessions {
        let summary = &bundle.summary;
        if let Some(t) = tool_filter.as_deref()
            && summary.source_tool != t
        {
            continue;
        }

        let entry = map
            .entry(summary.project_context.clone())
            .or_insert(ProjectSummary {
                project_context: summary.project_context.clone(),
                session_count: 0,
                turn_count: 0,
                last_ended_at: summary.ended_at,
            });
        entry.session_count += 1;
        entry.turn_count += summary.turn_count;
        if summary.ended_at > entry.last_ended_at {
            entry.last_ended_at = summary.ended_at;
        }
    }

    let mut projects: Vec<ProjectSummary> = map.into_values().collect();
    projects.sort_by(|a, b| b.last_ended_at.cmp(&a.last_ended_at));

    Ok(Json(ProjectsResponse {
        projects,
        day: dataset.day_filter,
    }))
}

async fn get_salient(
    State(state): State<AppState>,
    Query(query): Query<DayLimitQuery>,
) -> ApiResult<Json<SalientResponse>> {
    let day = parse_day(&query.day)?;
    let dataset = ensure_dataset(&state, day, query.refresh.unwrap_or(false)).await?;
    let limit = query.limit.unwrap_or(5).clamp(1, 50);

    let mut bundles: Vec<_> = dataset.sessions.clone();
    bundles.sort_by(|a, b| b.summary.score.total_cmp(&a.summary.score));
    bundles.truncate(limit);

    let mut sessions = Vec::new();
    for bundle in bundles {
        let top_turns = pick_salient_turns(&bundle.turns);
        sessions.push(SalientSession {
            session: bundle.summary,
            top_turns,
        });
    }

    Ok(Json(SalientResponse {
        sessions,
        day: dataset.day_filter,
    }))
}

async fn get_probe(
    State(state): State<AppState>,
    Query(query): Query<DayLimitQuery>,
) -> ApiResult<Json<ProbeResponse>> {
    let day = parse_day(&query.day)?;
    let dataset = ensure_dataset(&state, day, query.refresh.unwrap_or(false)).await?;
    let limit = query.limit.unwrap_or(12).clamp(1, 100);
    let probe = query
        .q
        .clone()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request(anyhow::anyhow!("probe requires ?q=<query>")))?;

    let matches = search::probe(&dataset, &probe, day, limit);
    let prompt_suggestion = search::build_probe_prompt(&probe, &matches);
    Ok(Json(ProbeResponse {
        query: probe,
        matches,
        prompt_suggestion,
        day: dataset.day_filter,
    }))
}

async fn get_session_events(
    State(state): State<AppState>,
    Query(query): Query<SessionEventsQuery>,
) -> ApiResult<Json<Vec<MasterLog>>> {
    if query.source_tool.trim().is_empty() || query.session_id.trim().is_empty() {
        return Err(ApiError::bad_request(anyhow::anyhow!(
            "session_events requires source_tool and session_id"
        )));
    }

    let max_chars = query
        .max_content_chars
        .unwrap_or(20_000)
        .clamp(200, 200_000);
    let log_path = state.log_path.clone();
    let source_tool = query.source_tool.clone();
    let session_id = query.session_id.clone();

    let logs = tokio::task::spawn_blocking(move || {
        read_session_events(&log_path, &source_tool, &session_id, max_chars)
    })
    .await
    .map_err(|e| ApiError::internal(anyhow::anyhow!("join error: {e}")))?
    .map_err(ApiError::internal)?;

    Ok(Json(logs))
}

async fn create_memory(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<MemoryBody>,
) -> ApiResult<Json<MemoryRecord>> {
    let day = parse_day(&body.day)?;
    let dataset = ensure_dataset(&state, day, false).await?;
    let limit = body.limit.unwrap_or(12).clamp(1, 100);
    let matches = search::probe(&dataset, &body.q, day, limit);
    let prompt = search::build_probe_prompt(&body.q, &matches);

    let record = MemoryRecord {
        id: uuid::Uuid::new_v4(),
        created_at: chrono::Utc::now(),
        query: body.q.clone(),
        day: day.map(|d| d.to_string()),
        matches,
        prompt,
        llm_response: body.llm_response,
    };

    let _guard = state.memory_io_lock.lock().await;
    append_memory(&state.memory_path, &record).map_err(ApiError::internal)?;
    Ok(Json(record))
}

async fn list_memories(State(state): State<AppState>) -> ApiResult<Json<models::MemoriesResponse>> {
    let _guard = state.memory_io_lock.lock().await;
    let records = read_memories(&state.memory_path).map_err(ApiError::internal)?;
    Ok(Json(models::MemoriesResponse { memories: records }))
}

async fn get_context_pack(
    State(state): State<AppState>,
    Query(query): Query<ContextPackQuery>,
) -> ApiResult<Response> {
    let day = parse_day(&query.day)?;
    let dataset = ensure_dataset(&state, day, query.refresh.unwrap_or(false)).await?;

    let session_limit = query.session_limit.unwrap_or(5).clamp(1, 20);
    let memory_limit = query.memory_limit.unwrap_or(5).clamp(0, 50);
    let include_memories = query.include_memories.unwrap_or(true);
    let include_memory_blocks = query.include_memory_blocks.unwrap_or(true);
    let format = query.format.unwrap_or_else(|| "json".to_string());

    let max_chars = env::var("CONTRAIL_CONTEXT_PACK_MAX_CHARS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(12_000)
        .clamp(1_000, 120_000);

    let mut bundles = dataset.sessions.clone();
    bundles.sort_by(|a, b| b.summary.score.total_cmp(&a.summary.score));
    bundles.truncate(session_limit);

    let mut top_sessions = Vec::new();
    for bundle in bundles {
        let mut top_turns = pick_salient_turns(&bundle.turns);
        top_turns.truncate(5);
        top_sessions.push(SalientSession {
            session: bundle.summary,
            top_turns,
        });
    }

    let memory_blocks = if include_memory_blocks {
        let _guard = state.memory_io_lock.lock().await;
        let mut blocks =
            memory_blocks::read_blocks(&state.memory_blocks_path).map_err(ApiError::internal)?;
        blocks.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        blocks.truncate(25);
        blocks
    } else {
        Vec::new()
    };

    let recent_memories = if include_memories && memory_limit > 0 {
        let _guard = state.memory_io_lock.lock().await;
        let records = read_memories(&state.memory_path).map_err(ApiError::internal)?;
        context_pack::to_memory_snippets(records, memory_limit, day)
    } else {
        Vec::new()
    };

    let (prompt, flags) = context_pack::build_prompt(
        day,
        &memory_blocks,
        &top_sessions,
        &recent_memories,
        max_chars,
    );

    if format == "text" {
        return Ok(prompt.into_response());
    }

    let resp = ContextPackResponse {
        generated_at: chrono::Utc::now(),
        day: dataset.day_filter,
        prompt,
        security_flags: flags,
        memory_blocks,
        top_sessions,
        recent_memories,
    };

    Ok(Json(resp).into_response())
}

async fn list_memory_blocks(State(state): State<AppState>) -> ApiResult<Json<Vec<MemoryBlock>>> {
    let _guard = state.memory_io_lock.lock().await;
    let blocks =
        memory_blocks::read_blocks(&state.memory_blocks_path).map_err(ApiError::internal)?;
    Ok(Json(blocks))
}

async fn create_memory_block(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<MemoryBlockBody>,
) -> ApiResult<Json<MemoryBlock>> {
    if body.label.trim().is_empty() {
        return Err(ApiError::bad_request(anyhow::anyhow!(
            "label cannot be empty"
        )));
    }
    if body.value.trim().is_empty() {
        return Err(ApiError::bad_request(anyhow::anyhow!(
            "value cannot be empty"
        )));
    }

    let sentry = scrapers::sentry::Sentry::new();
    let (value, flags) = sentry.scan_and_redact(&body.value);

    let block = MemoryBlock {
        id: uuid::Uuid::new_v4(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        label: body.label,
        value,
        security_flags: flags,
        project_context: body.project_context,
        source_tool: body.source_tool,
        tags: body.tags,
    };

    let _guard = state.memory_io_lock.lock().await;
    let created = memory_blocks::insert_block(&state.memory_blocks_path, block)
        .map_err(ApiError::internal)?;
    Ok(Json(created))
}

async fn update_memory_block(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    axum::Json(mut update): axum::Json<MemoryBlockUpdate>,
) -> ApiResult<Json<MemoryBlock>> {
    if let Some(label) = update.label.as_ref()
        && label.trim().is_empty()
    {
        return Err(ApiError::bad_request(anyhow::anyhow!(
            "label cannot be empty"
        )));
    }

    if let Some(value) = update.value.as_ref() {
        if value.trim().is_empty() {
            return Err(ApiError::bad_request(anyhow::anyhow!(
                "value cannot be empty"
            )));
        }
        let sentry = scrapers::sentry::Sentry::new();
        let (redacted, flags) = sentry.scan_and_redact(value);
        update.value = Some(redacted);
        update.security_flags = Some(flags);
    }

    let _guard = state.memory_io_lock.lock().await;
    let updated =
        memory_blocks::update_block(&state.memory_blocks_path, id, update).map_err(|e| {
            if e.to_string().contains("not found") {
                ApiError::not_found(e)
            } else {
                ApiError::internal(e)
            }
        })?;

    Ok(Json(updated))
}

async fn delete_memory_block(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<StatusCode> {
    let _guard = state.memory_io_lock.lock().await;
    memory_blocks::delete_block(&state.memory_blocks_path, id).map_err(|e| {
        if e.to_string().contains("not found") {
            ApiError::not_found(e)
        } else {
            ApiError::internal(e)
        }
    })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_memory_with_llm(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<AutoProbeBody>,
) -> ApiResult<Json<MemoryRecord>> {
    let llm = state.llm.clone().ok_or_else(|| {
        ApiError::bad_request(anyhow::anyhow!("LLM not configured (set OPENAI_API_KEY)"))
    })?;

    let day = parse_day(&body.day)?;
    let dataset = ensure_dataset(&state, day, false).await?;
    let limit = body.limit.unwrap_or(12).clamp(1, 100);
    let matches = search::probe(&dataset, &body.q, day, limit);
    let prompt = search::build_probe_prompt(&body.q, &matches)
        .ok_or_else(|| ApiError::not_found(anyhow::anyhow!("no matches found for probe")))?;

    let llm_response = llm
        .chat(&prompt, body.model.clone(), body.temperature)
        .await
        .map_err(ApiError::internal)?;

    let record = MemoryRecord {
        id: uuid::Uuid::new_v4(),
        created_at: chrono::Utc::now(),
        query: body.q.clone(),
        day: day.map(|d| d.to_string()),
        matches,
        prompt: Some(prompt),
        llm_response: Some(llm_response),
    };

    let _guard = state.memory_io_lock.lock().await;
    append_memory(&state.memory_path, &record).map_err(ApiError::internal)?;
    Ok(Json(record))
}

async fn run_default_autoprobes(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<DefaultAutoProbeBody>,
) -> ApiResult<Json<Vec<MemoryRecord>>> {
    let llm = state.llm.clone().ok_or_else(|| {
        ApiError::bad_request(anyhow::anyhow!("LLM not configured (set OPENAI_API_KEY)"))
    })?;
    let day = parse_day(&body.day)?;
    let dataset = ensure_dataset(&state, day, false).await?;
    let limit = body.limit.unwrap_or(12).clamp(1, 100);
    let queries = body
        .queries
        .clone()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_PROBES.iter().map(|s| s.to_string()).collect());

    let mut records = Vec::new();
    for q in queries {
        let matches = search::probe(&dataset, &q, day, limit);
        let Some(prompt) = search::build_probe_prompt(&q, &matches) else {
            continue;
        };
        let llm_response = llm
            .chat(&prompt, body.model.clone(), body.temperature)
            .await
            .map_err(ApiError::internal)?;
        let record = MemoryRecord {
            id: uuid::Uuid::new_v4(),
            created_at: chrono::Utc::now(),
            query: q.clone(),
            day: day.map(|d| d.to_string()),
            matches,
            prompt: Some(prompt),
            llm_response: Some(llm_response),
        };
        let _guard = state.memory_io_lock.lock().await;
        append_memory(&state.memory_path, &record).map_err(ApiError::internal)?;
        records.push(record);
    }

    Ok(Json(records))
}

async fn ensure_dataset(
    state: &AppState,
    day: Option<NaiveDate>,
    refresh: bool,
) -> ApiResult<Dataset> {
    let needs_reload = {
        let guard = state.data.read().await;
        refresh || guard.day_filter != day
    };

    if needs_reload {
        let reloaded = ingest::load_dataset(&state.log_path, day)
            .map_err(|e| ApiError::internal(e.context("reload")))?;
        let mut guard = state.data.write().await;
        *guard = reloaded.clone();
        return Ok(reloaded);
    }

    let guard = state.data.read().await;
    Ok(guard.clone())
}

fn parse_day(raw: &Option<String>) -> ApiResult<Option<NaiveDate>> {
    if let Some(day_str) = raw {
        if day_str.trim().is_empty() {
            return Ok(None);
        }
        let parsed = NaiveDate::parse_from_str(day_str, "%Y-%m-%d")
            .map_err(|e| ApiError::bad_request(anyhow::anyhow!("invalid day param: {e}")))?;
        Ok(Some(parsed))
    } else {
        Ok(None)
    }
}

fn pick_salient_turns(turns: &[ScoredTurn]) -> Vec<TurnSummary> {
    if turns.is_empty() {
        return Vec::new();
    }
    let mut sorted = turns.to_vec();
    sorted.sort_by(|a, b| b.salience.total_cmp(&a.salience));
    let mut picks = Vec::new();
    // Always include first and last
    picks.push(turns.first().unwrap().turn.clone());
    if turns.len() > 1 {
        picks.push(turns.last().unwrap().turn.clone());
    }
    for t in sorted.into_iter().take(3) {
        if picks.iter().any(|p| p.event_id == t.turn.event_id) {
            continue;
        }
        picks.push(t.turn);
    }
    picks
}

fn expand_tilde_path(p: &std::path::Path) -> PathBuf {
    if let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    p.to_path_buf()
}

fn read_session_events(
    log_path: &PathBuf,
    source_tool: &str,
    session_id: &str,
    max_content_chars: usize,
) -> anyhow::Result<Vec<MasterLog>> {
    let file =
        std::fs::File::open(log_path).map_err(|e| anyhow::anyhow!("open {:?}: {}", log_path, e))?;
    let reader = BufReader::new(file);
    let sentry = scrapers::sentry::Sentry::new();

    let mut out = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let Ok(mut log) = serde_json::from_str::<MasterLog>(&line) else {
            continue;
        };
        if log.source_tool != source_tool || log.session_id != session_id {
            continue;
        }

        let mut clipped = String::new();
        for c in log.interaction.content.chars().take(max_content_chars) {
            clipped.push(c);
        }

        let (redacted, flags) = sentry.scan_and_redact(&clipped);
        if redacted != log.interaction.content {
            log.interaction.content = redacted;
        }
        if flags.has_pii || !flags.redacted_secrets.is_empty() {
            log.security_flags.has_pii |= flags.has_pii;
            let mut merged: std::collections::HashSet<String> =
                log.security_flags.redacted_secrets.into_iter().collect();
            merged.extend(flags.redacted_secrets);
            let mut merged: Vec<String> = merged.into_iter().collect();
            merged.sort();
            log.security_flags.redacted_secrets = merged;
        }

        out.push(log);
    }

    out.sort_by_key(|l| l.timestamp);
    Ok(out)
}
