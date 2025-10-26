#!/bin/bash
#
# Watch BPF source files and auto-rebuild on changes
#
# Uses cargo-watch to automatically rebuild when you save files.
# Great for rapid development!

set -e

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() {
    echo -e "${GREEN}[WATCH]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WATCH]${NC} $1"
}

# Check if cargo-watch is installed
if ! command -v cargo-watch &> /dev/null; then
    log_warn "cargo-watch not found."
    log_warn ""
    log_warn "Alternative: Use a manual watch loop instead:"
    log_warn "  while true; do"
    log_warn "    cargo +nightly build --release --target=bpfel-unknown-none"
    log_warn "    sleep 2"
    log_warn "  done"
    exit 1
fi

log_info "Watching bpf/ for changes..."
log_info "Press Ctrl-C to stop"
log_info ""

cd bpf

# Watch for changes and rebuild
cargo +nightly watch \
    -x 'build --release --target=bpfel-unknown-none' \
    -x 'test' \
    -c \
    -q

# Flags:
# -x: Execute command
# -c: Clear screen between runs
# -q: Quiet mode (less output)
