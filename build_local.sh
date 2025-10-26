#!/bin/bash
#
# Build RAUTA locally on macOS
#
# This script builds all components:
# - common library
# - BPF program (cross-compiled for Linux)
# - control plane
# - CLI

set -e

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() {
    echo -e "${GREEN}[BUILD]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[BUILD]${NC} $1"
}

log_info "Building RAUTA on macOS..."
log_info ""

# Check if setup has been run
if ! command -v bpf-linker &> /dev/null; then
    log_warn "bpf-linker not found. Run ./setup_macos.sh first"
    exit 1
fi

# Build common
log_info "1/4 Building common library..."
cd common
cargo build --release
cd ..
log_info "✓ common built"

# Build BPF program
log_info ""
log_info "2/4 Building BPF program (cross-compile for Linux)..."
cd bpf
cargo +nightly build --release --target=bpfel-unknown-none
cd ..

BPF_PATH="target/bpfel-unknown-none/release/rauta"
if [ -f "$BPF_PATH" ]; then
    BPF_SIZE=$(ls -lh "$BPF_PATH" | awk '{print $5}')
    log_info "✓ BPF program built: $BPF_PATH ($BPF_SIZE)"
else
    log_warn "✗ BPF build failed"
    exit 1
fi

# Build control plane
log_info ""
log_info "3/4 Building control plane..."
cd control
cargo build --release
cd ..
log_info "✓ control plane built"

# Build CLI
log_info ""
log_info "4/4 Building CLI..."
cd cli
cargo build --release
cd ..
log_info "✓ CLI built"

# Summary
log_info ""
log_info "=========================================="
log_info "Build Complete! ✅"
log_info "=========================================="
log_info ""
log_info "Artifacts:"
log_info "  BPF:     target/bpfel-unknown-none/release/rauta"
log_info "  Control: target/release/control"
log_info "  CLI:     target/release/cli"
log_info ""
log_info "Note: BPF program can only run on Linux."
log_info "      Use Docker for testing: ./docker/test.sh"
