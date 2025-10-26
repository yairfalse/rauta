#!/bin/bash
#
# Watch control plane source files and auto-rebuild on changes

set -e

# Colors
GREEN='\033[0;32m'
NC='\033[0m'

log_info() {
    echo -e "${GREEN}[WATCH]${NC} $1"
}

# Check if cargo-watch is installed
if ! command -v cargo-watch &> /dev/null; then
    log_info "Installing cargo-watch..."
    cargo install cargo-watch
fi

log_info "Watching control/ for changes..."
log_info "Press Ctrl-C to stop"
log_info ""

cd control

# Watch for changes and rebuild
cargo watch \
    -x 'build --release' \
    -x 'test' \
    -c \
    -q
