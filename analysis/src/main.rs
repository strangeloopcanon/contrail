mod ingest;
mod models;
mod memory;
mod llm;
mod salience;
mod search;

use crate::models::ScoredTurn;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::NaiveDate;
use models::{
    Dataset, ProbeResponse, SalientResponse, SalientSession, SessionsResponse, TurnSummary,
};
use memory::{append_memory, read_memories, MemoryRecord};
use serde::Deserialize;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;

#[derive(Clone)]
struct AppState {
    log_path: PathBuf,
    memory_path: PathBuf,
    data: Arc<RwLock<Dataset>>,
    llm: Option<llm::LlmClient>,
}

#[derive(Debug)]
struct ApiError(anyhow::Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = format!("error: {}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, Deserialize)]
struct DayLimitQuery {
    day: Option<String>,
    limit: Option<usize>,
    refresh: Option<bool>,
    q: Option<String>,
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

    let initial_dataset = ingest::load_dataset(&log_path, None)?;
    let state = AppState {
        log_path,
        memory_path,
        data: Arc::new(RwLock::new(initial_dataset)),
        llm: llm::LlmClient::from_env()?,
    };

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/api/sessions", get(get_sessions))
        .route("/api/salient", get(get_salient))
        .route("/api/probe", get(get_probe))
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
    axum::serve(listener, app).await?;
    Ok(())
}

async fn get_sessions(
    State(state): State<AppState>,
    Query(query): Query<DayLimitQuery>,
) -> ApiResult<Json<SessionsResponse>> {
    let day = parse_day(&query.day)?;
    let dataset = ensure_dataset(&state, day, query.refresh.unwrap_or(false)).await?;
    let mut sessions: Vec<_> = dataset
        .sessions
        .iter()
        .map(|s| s.summary.clone())
        .collect();
    sessions.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(Json(SessionsResponse {
        sessions,
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
        .ok_or_else(|| ApiError(anyhow::anyhow!("probe requires ?q=<query>")))?;

    let matches = search::probe(&dataset, &probe, day, limit);
    let prompt_suggestion = search::build_probe_prompt(&probe, &matches);
    Ok(Json(ProbeResponse {
        query: probe,
        matches,
        prompt_suggestion,
        day: dataset.day_filter,
    }))
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

    append_memory(&state.memory_path, &record).map_err(ApiError)?;
    Ok(Json(record))
}

async fn list_memories(State(state): State<AppState>) -> ApiResult<Json<models::MemoriesResponse>> {
    let records = read_memories(&state.memory_path).map_err(ApiError)?;
    Ok(Json(models::MemoriesResponse { memories: records }))
}

async fn create_memory_with_llm(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<AutoProbeBody>,
) -> ApiResult<Json<MemoryRecord>> {
    let llm = state
        .llm
        .clone()
        .ok_or_else(|| ApiError(anyhow::anyhow!("LLM not configured (set OPENAI_API_KEY)")))?;

    let day = parse_day(&body.day)?;
    let dataset = ensure_dataset(&state, day, false).await?;
    let limit = body.limit.unwrap_or(12).clamp(1, 100);
    let matches = search::probe(&dataset, &body.q, day, limit);
    let prompt = search::build_probe_prompt(&body.q, &matches)
        .ok_or_else(|| ApiError(anyhow::anyhow!("no matches found for probe")))?;

    let llm_response = llm
        .chat(&prompt, body.model.clone(), body.temperature)
        .await
        .map_err(ApiError)?;

    let record = MemoryRecord {
        id: uuid::Uuid::new_v4(),
        created_at: chrono::Utc::now(),
        query: body.q.clone(),
        day: day.map(|d| d.to_string()),
        matches,
        prompt: Some(prompt),
        llm_response: Some(llm_response),
    };

    append_memory(&state.memory_path, &record).map_err(ApiError)?;
    Ok(Json(record))
}

async fn run_default_autoprobes(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<DefaultAutoProbeBody>,
) -> ApiResult<Json<Vec<MemoryRecord>>> {
    let llm = state
        .llm
        .clone()
        .ok_or_else(|| ApiError(anyhow::anyhow!("LLM not configured (set OPENAI_API_KEY)")))?;
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
            .map_err(ApiError)?;
        let record = MemoryRecord {
            id: uuid::Uuid::new_v4(),
            created_at: chrono::Utc::now(),
            query: q.clone(),
            day: day.map(|d| d.to_string()),
            matches,
            prompt: Some(prompt),
            llm_response: Some(llm_response),
        };
        append_memory(&state.memory_path, &record).map_err(ApiError)?;
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
        let reloaded =
            ingest::load_dataset(&state.log_path, day).map_err(|e| ApiError(e.context("reload")))?;
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
            .map_err(|e| ApiError(anyhow::anyhow!("invalid day param: {e}")))?;
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
