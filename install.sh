#!/bin/bash

echo "✈️  Installing Contrail + memex..."

# Check for Rust
if ! command -v cargo &> /dev/null; then
    echo "❌ Rust is not installed. Please install it from https://rustup.rs"
    exit 1
fi

# Build Release
echo "📦 Building core_daemon..."
cargo build --release -p core_daemon
echo "📦 Building dashboard..."
cargo build --release -p dashboard
echo "📦 Building importer..."
cargo build --release -p importer
echo "📦 Building memex..."
cargo build --release -p memex

if [ $? -eq 0 ]; then
    echo "✅ Build successful!"
    echo ""
    echo "Contrail (telemetry daemon):"
    echo "  ./target/release/core_daemon"
    echo ""
    echo "Dashboard:"
    echo "  ./target/release/dashboard"
    echo "  (Then open http://localhost:3000)"
    echo ""
    echo "memex (context layer):"
    echo "  ./target/release/memex init     # in any repo"
    echo "  ./target/release/memex sync     # pull session transcripts"
    echo ""
    echo "Import History:"
    echo "  ./target/release/importer"
else
    echo "❌ Build failed."
    exit 1
fi
