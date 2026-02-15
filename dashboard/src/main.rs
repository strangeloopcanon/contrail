use axum::{
    Json, Router,
    extract::{Query, State},
    response::{
        Html,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::env;
use std::fs::File;
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt},
    sync::broadcast,
};
use tower_http::cors::CorsLayer;

#[tokio::main]
async fn main() {
    let log_path = env::var("CONTRAIL_LOG_PATH")
        .map(PathBuf::from)
        .ok()
        .or_else(|| dirs::home_dir().map(|h| h.join(".contrail/logs/master_log.jsonl")))
        .expect("Could not resolve CONTRAIL_LOG_PATH or home directory");

    let (live_tx, _) = broadcast::channel(2048);
    let state = Arc::new(AppState {
        log_path: log_path.clone(),
        live_tx: live_tx.clone(),
    });

    tokio::spawn(run_live_publisher(log_path, live_tx));

    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(|| async { "ok" }))
        .route("/api/logs", get(get_logs))
        .route("/api/stream", get(stream_logs))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let bind_addr = env::var("DASHBOARD_BIND").unwrap_or_else(|_| "127.0.0.1:3000".to_string());
    println!("✈️  Contrail Dashboard running at http://{bind_addr}");
    let listener = tokio::net::TcpListener::bind(&bind_addr).await.unwrap();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

#[derive(Clone)]
struct AppState {
    log_path: PathBuf,
    live_tx: broadcast::Sender<Value>,
}

const DEFAULT_LIVE_LIMIT: usize = 200;
const DEFAULT_HISTORY_LIMIT: usize = 5_000;
const MAX_LIVE_LIMIT: usize = 5_000;
const MAX_ALL_LIMIT: usize = 200_000;

async fn index() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

async fn get_logs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LogsQuery>,
) -> Json<Vec<Value>> {
    let tool_filter = query.tool.clone();
    let session_filter = query.session_id.clone();
    let window = WindowKind::parse(query.window.as_deref(), query.all.unwrap_or(false));

    if matches!(window, WindowKind::Live) {
        let live_limit = query
            .limit
            .unwrap_or(DEFAULT_LIVE_LIMIT)
            .clamp(1, MAX_LIVE_LIMIT);
        let log_path = state.log_path.clone();
        let logs = tokio::task::spawn_blocking(move || {
            load_tail_logs(&log_path, live_limit, tool_filter, session_filter)
        })
        .await
        .unwrap_or_default();
        return Json(logs);
    }

    let all_limit = Some(
        query
            .limit
            .unwrap_or(DEFAULT_HISTORY_LIMIT)
            .clamp(1, MAX_ALL_LIMIT),
    );
    let custom_from = parse_rfc3339(query.from.as_deref());
    let custom_to = parse_rfc3339(query.to.as_deref());
    let (from_ts, to_ts) = resolve_window_bounds(window, custom_from, custom_to);

    let files = scrapers::log_index::discover_logs(&state.log_path)
        .unwrap_or_else(|_| vec![state.log_path.clone()]);
    let logs = tokio::task::spawn_blocking(move || {
        load_history_logs(
            &files,
            all_limit,
            tool_filter,
            session_filter,
            from_ts,
            to_ts,
        )
    })
    .await
    .unwrap_or_default();

    Json(logs)
}

async fn stream_logs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<StreamQuery>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.live_tx.subscribe();
    let tool_filter = query.tool.clone();
    let session_filter = query.session_id.clone();

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(json) => {
                    if !matches_filters(&json, tool_filter.as_deref(), session_filter.as_deref()) {
                        continue;
                    }
                    let data = match serde_json::to_string(&json) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    yield Ok(Event::default().data(data));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::new().interval(StdDuration::from_secs(10)))
}

async fn run_live_publisher(log_path: PathBuf, tx: broadcast::Sender<Value>) {
    let (mut offset, mut follow_initialized): (u64, bool) = match fs::metadata(&log_path).await {
        Ok(meta) => (meta.len(), true),
        Err(_) => (0, false),
    };
    let mut carry: Vec<u8> = Vec::new();

    loop {
        let size = match fs::metadata(&log_path).await {
            Ok(meta) => meta.len(),
            Err(_) => {
                offset = 0;
                follow_initialized = false;
                carry.clear();
                tokio::time::sleep(StdDuration::from_secs(1)).await;
                continue;
            }
        };

        if !follow_initialized {
            offset = size;
            follow_initialized = true;
        }

        if size < offset {
            offset = 0;
            carry.clear();
        }

        if size > offset {
            match read_new_bytes(&log_path, offset).await {
                Ok(bytes) => {
                    offset = size;
                    carry.extend_from_slice(&bytes);

                    for line in drain_complete_lines(&mut carry) {
                        if line.trim().is_empty() {
                            continue;
                        }
                        if let Ok(json) = serde_json::from_str::<Value>(&line) {
                            let _ = tx.send(json);
                        }
                    }
                }
                Err(_) => {
                    // retry next interval
                }
            }
        }

        tokio::time::sleep(StdDuration::from_secs(1)).await;
    }
}

async fn read_new_bytes(path: &PathBuf, offset: u64) -> std::io::Result<Vec<u8>> {
    let mut file = fs::File::open(path).await?;
    file.seek(SeekFrom::Start(offset)).await?;
    let mut out = Vec::new();
    file.read_to_end(&mut out).await?;
    Ok(out)
}

fn drain_complete_lines(carry: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        let Some(pos) = carry.iter().position(|b| *b == b'\n') else {
            break;
        };
        let mut line = carry.drain(..=pos).collect::<Vec<_>>();
        if matches!(line.last(), Some(b'\n')) {
            line.pop();
        }
        if matches!(line.last(), Some(b'\r')) {
            line.pop();
        }
        if let Ok(text) = String::from_utf8(line) {
            out.push(text);
        }
    }
    out
}

fn load_history_logs(
    paths: &[PathBuf],
    limit: Option<usize>,
    tool_filter: Option<String>,
    session_filter: Option<String>,
    from_ts: Option<DateTime<Utc>>,
    to_ts: Option<DateTime<Utc>>,
) -> Vec<Value> {
    let mut logs: VecDeque<Value> = VecDeque::new();

    for path in paths {
        let Ok(file) = File::open(path) else {
            continue;
        };
        let reader = std::io::BufReader::new(file);
        for line in reader.lines() {
            let Ok(line) = line else {
                continue;
            };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(json) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if !matches_filters(&json, tool_filter.as_deref(), session_filter.as_deref()) {
                continue;
            }
            if !matches_time_window(&json, from_ts, to_ts) {
                continue;
            }
            logs.push_back(json);
            if let Some(max_items) = limit
                && logs.len() > max_items
            {
                let _ = logs.pop_front();
            }
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

fn matches_time_window(
    json: &Value,
    from_ts: Option<DateTime<Utc>>,
    to_ts: Option<DateTime<Utc>>,
) -> bool {
    if from_ts.is_none() && to_ts.is_none() {
        return true;
    }
    let Some(ts) = json
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
    else {
        return false;
    };

    if from_ts.is_some_and(|from| ts < from) {
        return false;
    }
    if to_ts.is_some_and(|to| ts > to) {
        return false;
    }
    true
}

fn parse_rfc3339(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

fn resolve_window_bounds(
    window: WindowKind,
    custom_from: Option<DateTime<Utc>>,
    custom_to: Option<DateTime<Utc>>,
) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let now = Utc::now();
    let preset_from = match window {
        WindowKind::Live => None,
        WindowKind::H24 => Some(now - Duration::hours(24)),
        WindowKind::D7 => Some(now - Duration::days(7)),
        WindowKind::D30 => Some(now - Duration::days(30)),
        WindowKind::D365 => Some(now - Duration::days(365)),
        WindowKind::All => None,
    };

    let from_ts = match (preset_from, custom_from) {
        (Some(a), Some(b)) => Some(std::cmp::max(a, b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    (from_ts, custom_to)
}

#[derive(Clone, Copy, Debug)]
enum WindowKind {
    Live,
    H24,
    D7,
    D30,
    D365,
    All,
}

impl WindowKind {
    fn parse(value: Option<&str>, all_flag: bool) -> Self {
        if all_flag {
            return Self::All;
        }
        match value.unwrap_or("live").to_lowercase().as_str() {
            "24h" => Self::H24,
            "7d" => Self::D7,
            "30d" => Self::D30,
            "365d" => Self::D365,
            "all" => Self::All,
            _ => Self::Live,
        }
    }
}

#[derive(Default, Deserialize)]
struct LogsQuery {
    all: Option<bool>,
    limit: Option<usize>,
    tool: Option<String>,
    session_id: Option<String>,
    window: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

#[derive(Default, Deserialize)]
struct StreamQuery {
    tool: Option<String>,
    session_id: Option<String>,
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
