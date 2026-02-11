#!/bin/bash

set -euo pipefail

echo "✈️  Installing Contrail + memex..."

# Check for Rust
if ! command -v cargo &> /dev/null; then
    echo "❌ Rust is not installed. Please install it from https://rustup.rs"
    exit 1
fi

echo "📦 Installing memex..."
cargo install --path tools/memex --locked --force
echo "📦 Installing contrail CLI..."
cargo install --path tools/contrail --locked --force
echo "📦 Installing importer (backward-compatible command)..."
cargo install --path importer --bin importer --locked --force
echo "📦 Installing core_daemon..."
cargo install --path core_daemon --locked --force
echo "📦 Installing dashboard..."
cargo install --path dashboard --locked --force
echo "📦 Installing analysis..."
cargo install --path analysis --locked --force

echo "✅ Install successful!"
echo ""
echo "Installed commands:"
echo "  contrail    # history import + cross-machine export/merge"
echo "  importer    # same CLI as contrail (backward-compatible name)"
echo "  core_daemon # background capture"
echo "  dashboard   # local log UI"
echo "  analysis    # deeper local analysis UI"
echo "  memex       # per-repo context sync/share"
