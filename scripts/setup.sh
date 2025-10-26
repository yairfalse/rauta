#!/bin/bash
#
# RAUTA Development Setup (Cross-Platform)
#
# Detects your platform and installs the right tools:
# - Linux: Full native toolchain (LLVM, bpf-linker, everything)
# - macOS: Hybrid workflow (Rust for dev, Docker for BPF builds)

set -e

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${GREEN}[SETUP]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[SETUP]${NC} $1"
}

log_platform() {
    echo -e "${BLUE}[SETUP]${NC} $1"
}

log_info "=========================================="
log_info "RAUTA Development Setup"
log_info "=========================================="
log_info ""

# Detect platform
if [[ "$OSTYPE" == "linux-gnu"* ]]; then
    log_platform "🐧 Linux detected - Installing full native toolchain"
    log_platform "   You'll be able to compile BPF programs natively!"
    log_info ""
    exec "$(dirname "$0")/scripts/setup_linux.sh"

elif [[ "$OSTYPE" == "darwin"* ]]; then
    log_platform "🍎 macOS detected - Installing hybrid workflow"
    log_platform "   Fast unit tests on macOS, Docker for BPF builds"
    log_info ""
    exec "$(dirname "$0")/scripts/setup_macos.sh"

else
    log_warn "Unknown platform: $OSTYPE"
    log_warn "Supported platforms: Linux, macOS"
    log_warn ""
    log_warn "You can still use Docker for development:"
    log_warn "  ./docker/build.sh"
    log_warn "  ./docker/test.sh"
    exit 1
fi
