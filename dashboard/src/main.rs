use axum::{Json, Router, extract::Query, extract::State, response::Html, routing::get};
use serde::Deserialize;
use serde_json::Value;
use std::collections::VecDeque;
use std::env;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::{
    fs,
    io::{AsyncBufReadExt, BufReader},
};
use tower_http::cors::CorsLayer;

#[tokio::main]
async fn main() {
    // Determine log path
    let log_path = env::var("CONTRAIL_LOG_PATH")
        .map(PathBuf::from)
        .ok()
        .or_else(|| dirs::home_dir().map(|h| h.join(".contrail/logs/master_log.jsonl")))
        .expect("Could not resolve CONTRAIL_LOG_PATH or home directory");

    let app_state = Arc::new(AppState { log_path });

    // Build our application with a route
    let app = Router::new()
        .route("/", get(index))
        .route("/api/logs", get(get_logs))
        .layer(CorsLayer::permissive())
        .with_state(app_state);

    let bind_addr = env::var("DASHBOARD_BIND").unwrap_or_else(|_| "127.0.0.1:3000".to_string());
    println!("✈️  Contrail Dashboard running at http://{bind_addr}");
    let listener = tokio::net::TcpListener::bind(&bind_addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

struct AppState {
    log_path: PathBuf,
}

const DEFAULT_ALL_LIMIT: usize = 5_000;
const MAX_ALL_LIMIT: usize = 20_000;

async fn index() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

async fn get_logs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LogsQuery>,
) -> Json<Vec<Value>> {
    let limit = query.limit.unwrap_or(200).clamp(1, 5000);
    let tool_filter = query.tool.clone();
    let session_filter = query.session_id.clone();

    if query.all.unwrap_or(false) {
        let all_limit = query
            .limit
            .unwrap_or(DEFAULT_ALL_LIMIT)
            .clamp(1, MAX_ALL_LIMIT);
        return Json(load_all_logs(&state.log_path, all_limit, tool_filter, session_filter).await);
    }

    let log_path = state.log_path.clone();
    let logs = tokio::task::spawn_blocking(move || {
        load_tail_logs(&log_path, limit, tool_filter, session_filter)
    })
    .await
    .unwrap_or_default();
    Json(logs)
}

async fn load_all_logs(
    path: &PathBuf,
    limit: usize,
    tool_filter: Option<String>,
    session_filter: Option<String>,
) -> Vec<Value> {
    let Ok(file) = fs::File::open(path).await else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let mut logs: VecDeque<Value> = VecDeque::with_capacity(limit.min(MAX_ALL_LIMIT));

    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(json) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if !matches_filters(&json, tool_filter.as_deref(), session_filter.as_deref()) {
            continue;
        }
        logs.push_back(json);
        if logs.len() > limit {
            let _ = logs.pop_front();
        }
    }

    logs.into_iter().collect()
}

fn load_tail_logs(
    path: &PathBuf,
    limit: usize,
    tool_filter: Option<String>,
    session_filter: Option<String>,
) -> Vec<Value> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Vec::new();
    };
    let file_size = meta.len();
    if file_size == 0 {
        return Vec::new();
    }

    let mut collected: VecDeque<Value> = VecDeque::with_capacity(limit.min(5000));
    let mut chunk_size = 256 * 1024usize;
    let max_chunk_size = 16 * 1024 * 1024usize;

    while chunk_size <= max_chunk_size {
        let start = file_size.saturating_sub(chunk_size as u64);
        let Ok(mut file) = File::open(path) else {
            return Vec::new();
        };
        if file.seek(SeekFrom::Start(start)).is_err() {
            return Vec::new();
        }

        let mut buf = Vec::new();
        if file.read_to_end(&mut buf).is_err() {
            return Vec::new();
        }

        let text = String::from_utf8_lossy(&buf);
        for line in text.lines().rev() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(json) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if !matches_filters(&json, tool_filter.as_deref(), session_filter.as_deref()) {
                continue;
            }
            collected.push_front(json);
            if collected.len() >= limit {
                return collected.into_iter().collect();
            }
        }

        if start == 0 {
            break;
        }
        chunk_size *= 2;
    }

    collected.into_iter().collect()
}

fn matches_filters(json: &Value, tool: Option<&str>, session_id: Option<&str>) -> bool {
    if let Some(tool_filter) = tool
        && json
            .get("source_tool")
            .and_then(Value::as_str)
            .is_some_and(|t| t != tool_filter)
    {
        return false;
    }

    if let Some(session_filter) = session_id
        && json
            .get("session_id")
            .and_then(Value::as_str)
            .is_some_and(|s| s != session_filter)
    {
        return false;
    }

    true
}

#[derive(Default, Deserialize)]
struct LogsQuery {
    all: Option<bool>,
    limit: Option<usize>,
    tool: Option<String>,
    session_id: Option<String>,
}
