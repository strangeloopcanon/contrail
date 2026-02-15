#!/bin/bash

set -euo pipefail

STATE_DIR="/tmp/contrail-dev"
CORE_PID_FILE="$STATE_DIR/core_daemon.pid"
DASH_PID_FILE="$STATE_DIR/dashboard.pid"
ANALYSIS_PID_FILE="$STATE_DIR/analysis.pid"
CORE_LOG="$STATE_DIR/core_daemon.log"
DASH_LOG="$STATE_DIR/dashboard.log"
ANALYSIS_LOG="$STATE_DIR/analysis.log"

mkdir -p "$STATE_DIR"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

is_running() {
    local pid_file="$1"
    if [ ! -f "$pid_file" ]; then
        return 1
    fi
    local pid
    pid="$(cat "$pid_file")"
    kill -0 "$pid" 2>/dev/null
}

start_proc() {
    local name="$1"
    local pid_file="$2"
    local log_file="$3"
    shift 3

    if is_running "$pid_file"; then
        echo "$name already running (pid $(cat "$pid_file"))"
        return 0
    fi

    "$@" >"$log_file" 2>&1 &
    local pid=$!
    echo "$pid" >"$pid_file"
    echo "started $name (pid $pid)"
}

stop_proc() {
    local name="$1"
    local pid_file="$2"

    if ! is_running "$pid_file"; then
        rm -f "$pid_file"
        echo "$name not running"
        return 0
    fi

    local pid
    pid="$(cat "$pid_file")"
    kill "$pid" 2>/dev/null || true
    rm -f "$pid_file"
    echo "stopped $name"
}

wait_for_health() {
    local url="$1"
    local name="$2"
    for _ in {1..30}; do
        if curl -fsS "$url" >/dev/null 2>&1; then
            echo "$name healthy at $url"
            return 0
        fi
        sleep 1
    done
    echo "warning: $name did not become healthy at $url"
    return 1
}

start_all() {
    cargo build --workspace
    start_proc "core_daemon" "$CORE_PID_FILE" "$CORE_LOG" cargo run -p core_daemon
    start_proc "dashboard" "$DASH_PID_FILE" "$DASH_LOG" cargo run -p contrail-dashboard
    start_proc "analysis" "$ANALYSIS_PID_FILE" "$ANALYSIS_LOG" cargo run -p analysis
    wait_for_health "http://127.0.0.1:3000/health" "dashboard" || true
    wait_for_health "http://127.0.0.1:3210/health" "analysis" || true
}

stop_all() {
    stop_proc "analysis" "$ANALYSIS_PID_FILE"
    stop_proc "dashboard" "$DASH_PID_FILE"
    stop_proc "core_daemon" "$CORE_PID_FILE"
}

status_all() {
    if is_running "$CORE_PID_FILE"; then
        echo "core_daemon: running (pid $(cat "$CORE_PID_FILE"))"
    else
        echo "core_daemon: stopped"
    fi

    if is_running "$DASH_PID_FILE"; then
        echo "dashboard: running (pid $(cat "$DASH_PID_FILE"))"
    else
        echo "dashboard: stopped"
    fi

    if is_running "$ANALYSIS_PID_FILE"; then
        echo "analysis: running (pid $(cat "$ANALYSIS_PID_FILE"))"
    else
        echo "analysis: stopped"
    fi
}

logs_all() {
    touch "$CORE_LOG" "$DASH_LOG" "$ANALYSIS_LOG"
    tail -n 150 -f "$CORE_LOG" "$DASH_LOG" "$ANALYSIS_LOG"
}

usage() {
    cat <<EOF
Usage: ./scripts/dev.sh {start|stop|status|logs|check}
  start   Build workspace and launch core_daemon/dashboard/analysis
  stop    Stop all managed processes
  status  Show process status
  logs    Tail process logs
  check   Run make check && make test
EOF
}

case "${1:-start}" in
    start)
        start_all
        ;;
    stop)
        stop_all
        ;;
    status)
        status_all
        ;;
    logs)
        logs_all
        ;;
    check)
        make check
        make test
        ;;
    *)
        usage
        exit 1
        ;;
esac
