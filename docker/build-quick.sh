#!/bin/bash
#
# Quick Docker build test (without full multi-stage)
#
# This builds just the BPF and control plane to verify they compile
# Faster than full Dockerfile for development iteration

set -e

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

echo -e "${GREEN}[RAUTA]${NC} Quick build test in Docker..."

# Use rust:latest image
docker run --rm -v "$(pwd):/rauta" -w /rauta rust:1.75-bookworm bash -c "
set -e

echo '${YELLOW}Installing dependencies...${NC}'
apt-get update -qq
apt-get install -y -qq clang llvm libelf-dev linux-headers-generic > /dev/null 2>&1

echo '${YELLOW}Installing bpf-linker...${NC}'
cargo install bpf-linker --quiet 2>&1 | grep -v 'Compiling' || true

echo '${YELLOW}Adding BPF target...${NC}'
rustup target add bpfel-unknown-none

echo '${GREEN}Building common library...${NC}'
cd common && cargo build --release

echo '${GREEN}Building BPF program...${NC}'
cd ../bpf && cargo +nightly build --release --target=bpfel-unknown-none -Z build-std=core

echo '${GREEN}Building control plane...${NC}'
cd ../control && cargo build --release

echo ''
echo -e '${GREEN}✅ All builds successful!${NC}'
echo ''
echo 'Built artifacts:'
ls -lh target/bpfel-unknown-none/release/rauta 2>/dev/null || echo '  ⚠️  BPF: Build successful but binary location may vary'
ls -lh control/target/release/control 2>/dev/null || echo '  ⚠️  Control: Build successful'
"

echo ""
echo -e "${GREEN}[RAUTA]${NC} Quick build test complete!"
