#!/bin/bash
# Integration test for graceful shutdown
# Tests that SIGTERM properly drains active connections

set -e

echo "=========================================="
echo "Graceful Shutdown Integration Test"
echo "=========================================="

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

cleanup() {
    echo -e "\n${YELLOW}Cleaning up processes...${NC}"
    pkill -f "http2_backend" 2>/dev/null || true
    pkill -f "RAUTA_BACKEND_ADDR" 2>/dev/null || true
    rm -f /tmp/backend.pid /tmp/rauta.pid /tmp/request.result 2>/dev/null || true
}

trap cleanup EXIT

# Step 1: Start backend (Python slow backend)
echo -e "${YELLOW}[1/6] Starting slow backend server on port 9090 (3s delay)...${NC}"
python3 scripts/slow_backend.py 9090 > /tmp/backend.log 2>&1 &
BACKEND_PID=$!
echo $BACKEND_PID > /tmp/backend.pid
sleep 2

if ! ps -p $BACKEND_PID > /dev/null; then
    echo -e "${RED}✗ Backend failed to start${NC}"
    cat /tmp/backend.log
    exit 1
fi
echo -e "${GREEN}✓ Backend started (PID: $BACKEND_PID)${NC}"

# Step 2: Start RAUTA proxy
echo -e "${YELLOW}[2/6] Starting RAUTA proxy on port 8080...${NC}"
RAUTA_BACKEND_ADDR="127.0.0.1:9090" ./target/release/control > /tmp/rauta.log 2>&1 &
RAUTA_PID=$!
echo $RAUTA_PID > /tmp/rauta.pid
sleep 2

if ! ps -p $RAUTA_PID > /dev/null; then
    echo -e "${RED}✗ RAUTA failed to start${NC}"
    cat /tmp/rauta.log
    exit 1
fi
echo -e "${GREEN}✓ RAUTA started (PID: $RAUTA_PID)${NC}"

# Step 3: Test basic connectivity
echo -e "${YELLOW}[3/6] Testing basic connectivity...${NC}"
if ! curl -s -f http://127.0.0.1:8080/ > /dev/null 2>&1; then
    echo -e "${RED}✗ RAUTA not responding${NC}"
    exit 1
fi
echo -e "${GREEN}✓ RAUTA responding to requests${NC}"

# Step 4: Start a long-running request (3 seconds) in background
echo -e "${YELLOW}[4/6] Starting long-running request (3s backend delay)...${NC}"
(
    START_TIME=$(date +%s)
    RESPONSE=$(curl -s http://127.0.0.1:8080/ 2>&1)
    CURL_EXIT=$?
    END_TIME=$(date +%s)
    DURATION=$((END_TIME - START_TIME))

    if [ $CURL_EXIT -eq 0 ]; then
        echo "SUCCESS:$DURATION:$RESPONSE" > /tmp/request.result
    else
        echo "FAILED:$DURATION:$RESPONSE" > /tmp/request.result
    fi
) &
REQUEST_PID=$!
echo -e "${GREEN}✓ Long request started (PID: $REQUEST_PID)${NC}"

# Step 5: Wait for request to start, then send SIGTERM
sleep 1
echo -e "${YELLOW}[5/6] Sending SIGTERM to RAUTA (graceful shutdown)...${NC}"
SHUTDOWN_START=$(date +%s)
kill -TERM $RAUTA_PID

# Step 6: Wait for request to complete and RAUTA to exit
echo -e "${YELLOW}[6/6] Waiting for graceful shutdown (max 35s)...${NC}"
TIMEOUT=35
ELAPSED=0

while ps -p $RAUTA_PID > /dev/null 2>&1; do
    sleep 1
    ELAPSED=$((ELAPSED + 1))
    if [ $ELAPSED -ge $TIMEOUT ]; then
        echo -e "${RED}✗ RAUTA did not shutdown within ${TIMEOUT}s${NC}"
        ps aux | grep -E "(rauta|control)" | grep -v grep
        exit 1
    fi
    echo -n "."
done

SHUTDOWN_END=$(date +%s)
SHUTDOWN_DURATION=$((SHUTDOWN_END - SHUTDOWN_START))
echo ""
echo -e "${GREEN}✓ RAUTA shutdown after ${SHUTDOWN_DURATION}s${NC}"

# Wait for request to finish
wait $REQUEST_PID 2>/dev/null

# Verify results
echo ""
echo "=========================================="
echo "Test Results"
echo "=========================================="

if [ -f /tmp/request.result ]; then
    RESULT=$(cat /tmp/request.result)
    STATUS=$(echo $RESULT | cut -d: -f1)
    DURATION=$(echo $RESULT | cut -d: -f2)

    echo "Request status: $STATUS"
    echo "Request duration: ${DURATION}s"
    echo "Shutdown duration: ${SHUTDOWN_DURATION}s"

    if [ "$STATUS" = "SUCCESS" ]; then
        if [ $DURATION -ge 2 ] && [ $DURATION -le 5 ]; then
            RESPONSE_TEXT=$(echo $RESULT | cut -d: -f3-)
            echo -e "${GREEN}✓✓✓ GRACEFUL SHUTDOWN WORKS!${NC}"
            echo -e "${GREEN}    - Request completed successfully${NC}"
            echo -e "${GREEN}    - Took ${DURATION}s (expected ~3s)${NC}"
            echo -e "${GREEN}    - Response: ${RESPONSE_TEXT}${NC}"
            echo -e "${GREEN}    - Server waited for completion before exiting${NC}"
            exit 0
        else
            echo -e "${RED}✗ Request duration unexpected (got ${DURATION}s, expected 2-5s)${NC}"
            exit 1
        fi
    else
        ERROR=$(echo $RESULT | cut -d: -f3-)
        echo -e "${RED}✗ Request failed: $ERROR${NC}"
        exit 1
    fi
else
    echo -e "${RED}✗ No request result found${NC}"
    exit 1
fi
