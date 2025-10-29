#!/bin/bash
# Test script to demonstrate structured logging

set -e

echo "🧪 Testing RAUTA structured logging..."
echo ""

# Start Python backend server on port 9090
echo "1. Starting Python backend on port 9090..."
python3 -m http.server 9090 --bind 127.0.0.1 > /dev/null 2>&1 &
BACKEND_PID=$!
echo "   Backend PID: $BACKEND_PID"
sleep 1

# Build and start RAUTA proxy
echo "2. Building RAUTA proxy..."
cargo build --release --quiet
echo "   Built successfully"

echo ""
echo "3. Starting RAUTA proxy on port 8080..."
echo "   Watch the structured logs below:"
echo "   =================================="
echo ""

# Start proxy in background and capture logs
RUST_LOG=info cargo run --release &
PROXY_PID=$!

# Wait for proxy to start
sleep 2

echo ""
echo "4. Sending test requests to see logging..."
echo ""

# Send test requests
curl -s http://127.0.0.1:8080/ > /dev/null
sleep 0.5

curl -s http://127.0.0.1:8080/api/users > /dev/null
sleep 0.5

curl -s http://127.0.0.1:8080/nonexistent > /dev/null
sleep 0.5

echo ""
echo "5. Cleaning up..."
kill $PROXY_PID 2>/dev/null || true
kill $BACKEND_PID 2>/dev/null || true
wait $PROXY_PID 2>/dev/null || true
wait $BACKEND_PID 2>/dev/null || true

echo ""
echo "✅ Test complete! Check the logs above to see:"
echo "   - Request received (method + path)"
echo "   - Backend selected (IP + port)"
echo "   - Request completed (status + duration)"
echo "   - No route found (404 warning)"
