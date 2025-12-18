use axum::{Json, Router, extract::State, response::Html, routing::get};
use serde::Deserialize;
use serde_json::Value;
use std::collections::VecDeque;
use std::env;
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
    let home = dirs::home_dir().expect("Could not find home directory");
    let log_path = home.join(".contrail/logs/master_log.jsonl");

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

async fn index() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

async fn get_logs(State(state): State<Arc<AppState>>) -> Json<Vec<Value>> {
    let query: LogsQuery = axum::extract::Query::<LogsQuery>::default().0;
    if query.all.unwrap_or(false)
        && let Ok(content) = fs::read_to_string(&state.log_path).await
    {
        let mut logs = Vec::new();
        for line in content.lines() {
            if let Ok(json) = serde_json::from_str::<Value>(line) {
                logs.push(json);
            }
        }
        return Json(logs);
    }

    let mut tail: VecDeque<Value> = VecDeque::with_capacity(200);

    if let Ok(file) = fs::File::open(&state.log_path).await {
        let reader = BufReader::new(file);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(json) = serde_json::from_str::<Value>(&line) {
                if tail.len() == 200 {
                    tail.pop_front();
                }
                tail.push_back(json);
            }
        }
    }

    Json(tail.into_iter().collect())
}

#[derive(Default, Deserialize)]
struct LogsQuery {
    all: Option<bool>,
}
