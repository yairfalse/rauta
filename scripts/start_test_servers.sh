#!/bin/bash

# Start test servers for load testing
set -e

echo "🚀 Starting RAUTA test environment..."

# Kill any existing processes
pkill -f "http2_backend" || true
pkill -f "proxy_server" || true
sleep 1

# Build the control binary
echo "📦 Building control binary..."
cd "$(dirname "$0")/.."
cargo build --release --bin control 2>&1 | tail -3

# Start HTTP/2 backend server on port 8082
echo "🔧 Starting HTTP/2 backend server on :8082..."
cargo run --release --example http2_backend > backend.log 2>&1 &
BACKEND_PID=$!
echo "Backend PID: $BACKEND_PID"

# Wait for backend to start
sleep 2

# Test backend
echo "✅ Testing backend..."
curl -s http://127.0.0.1:8082/health || {
    echo "❌ Backend failed to start"
    cat backend.log
    exit 1
}

# Start proxy server on port 8081
echo "🔧 Starting proxy server on :8081..."
# We need to create a simple proxy example or use the control binary
# For now, let's check what we have

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "✅ Backend server running on :8082"
echo "   PID: $BACKEND_PID"
echo "   Logs: backend.log"
echo ""
echo "⚠️  Proxy server needs to be started separately"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
