#!/usr/bin/env python3
"""
Simple HTTP server that delays responses for testing graceful shutdown
"""
import time
from http.server import HTTPServer, BaseHTTPRequestHandler
import sys

class SlowHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        # Delay for 3 seconds
        print(f"[Backend] Request started at {time.time()}", flush=True)
        time.sleep(3)
        print(f"[Backend] Request completed at {time.time()}", flush=True)

        self.send_response(200)
        self.send_header('Content-Type', 'text/plain')
        self.end_headers()
        self.wfile.write(b'OK after 3 second delay\n')

    def log_message(self, format, *args):
        # Suppress default logging
        pass

if __name__ == '__main__':
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 9090
    print(f"[Backend] Starting slow backend on port {port}", flush=True)
    server = HTTPServer(('127.0.0.1', port), SlowHandler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("[Backend] Shutting down", flush=True)
        server.shutdown()
