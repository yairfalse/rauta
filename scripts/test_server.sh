#!/bin/bash
set -e

echo "Starting RAUTA server..."
./target/debug/control &
SERVER_PID=$!

# Wait for server to start by polling the endpoint (max 10s)
for i in {1..20}; do
    if curl -s http://127.0.0.1:8080/api/users >/dev/null; then
        break
    fi
    sleep 0.5
done

echo ""
echo "Testing routes..."
echo "================="

echo ""
echo "1. Testing GET /api/users (exact match):"
curl -s http://127.0.0.1:8080/api/users
echo ""

echo ""
echo "2. Testing GET /api/users/123 (prefix match):"
curl -s http://127.0.0.1:8080/api/users/123
echo ""

echo ""
echo "3. Testing GET /api/posts (different route):"
curl -s http://127.0.0.1:8080/api/posts
echo ""

echo ""
echo "4. Testing GET /api/nonexistent (should 404):"
curl -s -w "\nStatus: %{http_code}\n" http://127.0.0.1:8080/api/nonexistent
echo ""

# Kill server
kill $SERVER_PID
wait $SERVER_PID 2>/dev/null || true

echo ""
echo "✅ Tests complete!"
