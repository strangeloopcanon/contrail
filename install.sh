#!/bin/bash

set -euo pipefail

echo "✈️  Installing Contrail (all tools)..."

# Check for Rust
if ! command -v cargo &> /dev/null; then
    echo "❌ Rust is not installed. Please install it from https://rustup.rs"
    exit 1
fi

echo "📦 Building and installing all binaries via contrails..."
cargo install --path contrails --locked --force

echo "✅ Install successful!"
echo ""
echo "Installed commands:"
echo "  contrail    # service lifecycle (up/down/status) + history import/export"
echo "  memex       # per-repo context sync/share"
echo "  core_daemon # background capture"
echo "  dashboard   # local log UI"
echo "  analysis    # deeper local analysis UI"
echo "  importer    # same CLI as contrail (backward-compatible name)"
echo "  exporter    # filtered dataset export"
echo "  wrapup      # year-in-code report"
