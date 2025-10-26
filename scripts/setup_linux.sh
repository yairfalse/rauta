#!/bin/bash
#
# RAUTA Development Setup for Linux
#
# Installs full native toolchain for eBPF development

set -e

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

log_info() {
    echo -e "${GREEN}[SETUP]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[SETUP]${NC} $1"
}

log_error() {
    echo -e "${RED}[SETUP]${NC} $1"
}

log_info "=========================================="
log_info "RAUTA Linux Setup"
log_info "=========================================="
log_info ""

# Detect Linux distribution
if [ -f /etc/os-release ]; then
    . /etc/os-release
    DISTRO=$ID
else
    log_error "Cannot detect Linux distribution"
    exit 1
fi

log_info "Distribution: $DISTRO"
log_info ""

# 1. Install system dependencies
log_info "Step 1: Installing system dependencies..."
case $DISTRO in
    ubuntu|debian)
        sudo apt-get update
        sudo apt-get install -y \
            build-essential \
            llvm \
            clang \
            libelf-dev \
            linux-headers-$(uname -r) \
            pkg-config \
            curl
        log_info "✓ Installed via apt"
        ;;

    fedora|rhel|centos)
        sudo dnf install -y \
            gcc \
            llvm \
            clang \
            elfutils-libelf-devel \
            kernel-devel \
            pkg-config \
            curl
        log_info "✓ Installed via dnf"
        ;;

    arch)
        sudo pacman -S --needed \
            base-devel \
            llvm \
            clang \
            libelf \
            linux-headers \
            pkgconf \
            curl
        log_info "✓ Installed via pacman"
        ;;

    *)
        log_warn "Unknown distribution. Please install manually:"
        log_warn "  - build-essential / gcc"
        log_warn "  - llvm"
        log_warn "  - clang"
        log_warn "  - libelf-dev"
        log_warn "  - linux-headers"
        ;;
esac

# 2. Install Rust
log_info ""
log_info "Step 2: Checking Rust..."
if ! command -v rustc &> /dev/null; then
    log_info "Installing Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
else
    log_info "✓ Rust installed: $(rustc --version)"
fi

# 3. Install Rust nightly
log_info ""
log_info "Step 3: Installing Rust nightly toolchain..."
if rustup toolchain list | grep -q "nightly"; then
    log_info "✓ nightly toolchain already installed"
else
    rustup toolchain install nightly
    log_info "✓ Installed nightly toolchain"
fi

# 4. Install rust-src component
log_info ""
log_info "Step 4: Installing rust-src component..."
if rustup component list --toolchain nightly | grep -q "rust-src (installed)"; then
    log_info "✓ rust-src already installed"
else
    rustup +nightly component add rust-src
    log_info "✓ Added rust-src component"
fi

# 5. Install bpf-linker
log_info ""
log_info "Step 5: Installing bpf-linker..."
if command -v bpf-linker &> /dev/null; then
    log_info "✓ bpf-linker already installed: $(bpf-linker --version)"
else
    log_info "Installing bpf-linker (this may take a few minutes)..."
    cargo install bpf-linker
    log_info "✓ bpf-linker installed"
fi

# 6. Install cargo-watch (optional)
log_info ""
log_info "Step 6: Installing development tools (optional)..."
if ! command -v cargo-watch &> /dev/null; then
    log_info "Installing cargo-watch..."
    if cargo install cargo-watch 2>/dev/null; then
        log_info "✓ cargo-watch installed"
    else
        log_warn "⚠ cargo-watch installation failed (optional tool)"
        log_warn "  You can still build manually"
    fi
else
    log_info "✓ cargo-watch already installed"
fi

# 7. Test build
log_info ""
log_info "Step 7: Testing build..."

# Go to project root
cd "$(dirname "$0")/.."

log_info "Building common library..."
cd common
if cargo build --release; then
    log_info "✓ common builds successfully"
else
    log_error "✗ common build failed"
    exit 1
fi
cd ..

log_info ""
log_info "Building BPF program (native!)..."
cd bpf
if cargo +nightly build --release --target=bpfel-unknown-none; then
    log_info "✓ BPF program builds successfully"
    log_info "   Output: $(pwd)/../target/bpfel-unknown-none/release/rauta"
else
    log_error "✗ BPF build failed"
    exit 1
fi
cd ..

log_info ""
log_info "Building control plane..."
cd control
if cargo build --release; then
    log_info "✓ control plane builds successfully"
else
    log_error "✗ control build failed"
    exit 1
fi
cd ..

# 8. Summary
log_info ""
log_info "=========================================="
log_info "Setup Complete! 🐧🎉"
log_info "=========================================="
log_info ""
log_info "You can now develop RAUTA natively on Linux!"
log_info ""
log_info "Quick commands:"
log_info ""
log_info "  Build BPF:       cd bpf && cargo +nightly build --release --target=bpfel-unknown-none"
log_info "  Build control:   cd control && cargo build --release"
log_info "  Run tests:       cd common && cargo test"
log_info "  Auto-rebuild:    ./watch_bpf.sh"
log_info ""
log_info "Helper scripts:"
log_info ""
log_info "  ./build_local.sh      # Build all components"
log_info "  ./watch_bpf.sh        # Auto-rebuild BPF on file changes"
log_info "  ./docker/test.sh      # Test in Docker (optional)"
log_info ""
log_info "Native Linux = Best experience! ⚡"
log_info ""
