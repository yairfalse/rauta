#!/bin/bash

# Progressive Load Test Series for RAUTA
# Saves results to load_test_results.txt to avoid CLI output limits

OUTPUT_FILE="load_test_results.txt"
PROXY_URL="http://127.0.0.1:8081/api/test"

echo "🔥 PROGRESSIVE LOAD TEST SERIES 🔥" > "$OUTPUT_FILE"
echo "Testing RAUTA HTTP/2 proxy performance" >> "$OUTPUT_FILE"
echo "Target: $PROXY_URL" >> "$OUTPUT_FILE"
echo "Started: $(date)" >> "$OUTPUT_FILE"
echo "" >> "$OUTPUT_FILE"

# Test 1: Baseline (10 connections, 5s)
echo "━━━ TEST 1: BASELINE (10 connections, 5s) ━━━" | tee -a "$OUTPUT_FILE"
wrk -t2 -c10 -d5s "$PROXY_URL" 2>&1 | tee -a "$OUTPUT_FILE"
echo "" >> "$OUTPUT_FILE"

# Test 2: Light load (50 connections, 10s)
echo "━━━ TEST 2: LIGHT LOAD (50 connections, 10s) ━━━" | tee -a "$OUTPUT_FILE"
wrk -t4 -c50 -d10s "$PROXY_URL" 2>&1 | tee -a "$OUTPUT_FILE"
echo "" >> "$OUTPUT_FILE"

# Test 3: Medium load (100 connections, 10s)
echo "━━━ TEST 3: MEDIUM LOAD (100 connections, 10s) ━━━" | tee -a "$OUTPUT_FILE"
wrk -t4 -c100 -d10s "$PROXY_URL" 2>&1 | tee -a "$OUTPUT_FILE"
echo "" >> "$OUTPUT_FILE"

# Test 4: Heavy load (200 connections, 10s)
echo "━━━ TEST 4: HEAVY LOAD (200 connections, 10s) ━━━" | tee -a "$OUTPUT_FILE"
wrk -t8 -c200 -d10s "$PROXY_URL" 2>&1 | tee -a "$OUTPUT_FILE"
echo "" >> "$OUTPUT_FILE"

# Test 5: Stress test (400 connections, 10s)
echo "━━━ TEST 5: STRESS TEST (400 connections, 10s) ━━━" | tee -a "$OUTPUT_FILE"
wrk -t12 -c400 -d10s "$PROXY_URL" 2>&1 | tee -a "$OUTPUT_FILE"
echo "" >> "$OUTPUT_FILE"

# Test 6: Maximum load (800 connections, 10s)
echo "━━━ TEST 6: MAXIMUM LOAD (800 connections, 10s) ━━━" | tee -a "$OUTPUT_FILE"
wrk -t12 -c800 -d10s "$PROXY_URL" 2>&1 | tee -a "$OUTPUT_FILE"
echo "" >> "$OUTPUT_FILE"

echo "Completed: $(date)" >> "$OUTPUT_FILE"
echo "" >> "$OUTPUT_FILE"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" >> "$OUTPUT_FILE"
echo "Full results saved to: $OUTPUT_FILE" >> "$OUTPUT_FILE"

# Extract summary
echo ""
echo "📊 SUMMARY (Requests/sec):"
grep "Requests/sec:" "$OUTPUT_FILE" | nl

echo ""
echo "📁 Full results: $OUTPUT_FILE"
