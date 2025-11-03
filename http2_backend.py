#!/usr/bin/env python3
"""
Simple HTTP/2 backend server for testing RAUTA worker pools.
"""
from http.server import HTTPServer, BaseHTTPRequestHandler
import ssl
import json

class HTTP2Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        response = {
            'status': 'ok',
            'message': 'HTTP/2 backend response',
            'path': self.path
        }
        self.wfile.write(json.dumps(response).encode())

    def do_POST(self):
        content_length = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(content_length)

        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        response = {
            'status': 'ok',
            'message': 'POST received',
            'body_length': len(body)
        }
        self.wfile.write(json.dumps(response).encode())

    def log_message(self, format, *args):
        # Suppress HTTP logs for clean output
        pass

if __name__ == '__main__':
    # Note: Python's http.server doesn't natively support HTTP/2
    # For true HTTP/2, we'd need hypercorn or similar
    # This is HTTP/1.1 fallback - let's use Rust hyper instead
    print("⚠️  Python's http.server doesn't support HTTP/2")
    print("Use the Rust hyper backend server instead")
    print("Run: cargo run --example http2_backend")
