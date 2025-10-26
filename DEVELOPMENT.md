# RAUTA Development Guide (Cross-Platform)

Complete guide for developing RAUTA on Linux and macOS.

## Overview

RAUTA supports two development workflows:

- **Linux 🐧**: Full native toolchain - compile everything locally
- **macOS 🍎**: Hybrid workflow - fast tests on macOS, Docker for BPF builds

Both provide excellent development experiences!

## Quick Setup

### One-Command Setup (Recommended)

```bash
./setup.sh
```

This auto-detects your platform and installs the right tools.

---

## Linux Development 🐧

### Setup (One Time)

```bash
./setup.sh
```

This installs:
- Rust + nightly + rust-src
- LLVM and Clang
- bpf-linker
- Linux headers
- cargo-watch (optional)

**Time**: ~5-10 minutes

### Daily Development

```bash
# Build BPF program (native!)
cd bpf
cargo +nightly build --release --target=bpfel-unknown-none

# Build control plane
cd control
cargo build --release

# Run tests
cd common
cargo test

# Auto-rebuild on changes
./watch_bpf.sh
```

**Build time**: ~5-10 seconds (incremental)

### Platform-Specific Notes

**Ubuntu/Debian**:
```bash
# Manually install if needed:
sudo apt-get install build-essential llvm clang libelf-dev linux-headers-$(uname -r)
```

**Fedora/RHEL**:
```bash
# Manually install if needed:
sudo dnf install gcc llvm clang elfutils-libelf-devel kernel-devel
```

**Arch Linux**:
```bash
# Manually install if needed:
sudo pacman -S base-devel llvm clang libelf linux-headers
```

### Why Linux is Best

- ✅ Native BPF compilation (no Docker needed)
- ✅ Fastest iteration speed
- ✅ Can run XDP programs directly (with sudo)
- ✅ Matches production environment exactly

---

## macOS Development 🍎

### Setup (One Time)

```bash
./setup.sh
```

This installs:
- Rust + nightly + rust-src
- ⏭️ Skips bpf-linker (Docker handles it)
- ⏭️ Skips LLVM (Docker handles it)

**Time**: ~2-3 minutes (faster than Linux!)

### Daily Development

**Fast Path (90% of work)**:
```bash
# Edit code
vim bpf/src/main.rs

# Run unit tests (instant!)
cd common
cargo test

# Check types
cd control
cargo check
```

**When you need BPF builds**:
```bash
# Build in Docker
./docker/build.sh

# Full integration test
./docker/test.sh
```

### Why Hybrid Workflow?

- ✅ Fast unit tests (no Docker overhead)
- ✅ Reliable BPF builds (Linux environment)
- ✅ No LLVM compatibility issues
- ✅ Standard approach used by Aya/Cilium/Katran teams

### macOS Advantages

- ✅ 90% of development is instant
- ✅ Great IDE support (VS Code, IntelliJ)
- ✅ Docker only when you need it
- ✅ No dual-boot or VM needed

---

## Development Workflow (Both Platforms)

### Quick Commands

```bash
# Build everything
./build_local.sh              # Linux: includes BPF
                              # macOS: common + control only

# Build just BPF
cd bpf && cargo +nightly build --release --target=bpfel-unknown-none  # Linux only
./docker/build.sh             # macOS or Linux

# Build just control plane
cd control && cargo build --release

# Run tests
cd common && cargo test

# Auto-rebuild
./watch_bpf.sh                # Linux only (requires cargo-watch)
```

### TDD Workflow (Both Platforms)

```bash
# 1. Write test (RED)
cd common
vim tests/http_parsing_tests.rs

# 2. Run test (should fail)
cargo test test_new_feature
# ❌ test_new_feature ... FAILED

# 3. Implement feature (GREEN)
vim src/lib.rs

# 4. Run test again
cargo test test_new_feature
# ✅ test_new_feature ... ok

# 5. Refactor if needed
# Tests stay green!
```

### VS Code Integration (Both Platforms)

**Tasks (Cmd+Shift+B / Ctrl+Shift+B)**:
- `Build All` - Build BPF + control plane
- `Build BPF` - Build only BPF program (Linux: native, macOS: Docker)
- `Build Control` - Build only control plane
- `Test Common` - Run unit tests
- `Docker Test` - Run integration tests

**Run Tasks**: Press `Cmd+Shift+P` / `Ctrl+Shift+P` → "Tasks: Run Task"

---

## Typical Development Session

### Linux Developer

```bash
# Morning: Add new HTTP method parsing
cd common
vim src/lib.rs                    # Edit
cargo test                        # Test (2 seconds) ✅

# Add XDP forwarding logic
cd ../bpf
vim src/forwarding.rs             # Edit
cargo +nightly build --release --target=bpfel-unknown-none  # Build (5 seconds) ✅

# Test locally (with sudo)
sudo ./run_local.sh               # Test on real interface ✅
```

### macOS Developer

```bash
# Morning: Add new HTTP method parsing
cd common
vim src/lib.rs                    # Edit
cargo test                        # Test (2 seconds) ✅

# Add XDP forwarding logic
cd ../bpf
vim src/forwarding.rs             # Edit (just save, no build yet)

# Afternoon: Ready to test full stack
./docker/build.sh                 # Build (30 seconds) ✅
./docker/test.sh                  # Test (10 seconds) ✅
```

**Notice**: macOS dev saved time by batching BPF builds!

---

## Performance Tips

### Fast Iteration

**Linux**:
```bash
# Watch mode - auto-rebuild
./watch_bpf.sh
# Rebuilds in 5 seconds on save!
```

**macOS**:
```bash
# No watch mode needed - unit tests are instant
cargo test --watch              # If you have cargo-watch
# Or just: cargo test (2 seconds)
```

### Incremental Compilation

Rust's incremental compilation makes subsequent builds fast:
- First build: ~2 minutes
- Incremental: ~5 seconds (Linux) or instant tests (macOS)

**Tip**: Don't run `cargo clean` unless necessary.

### Parallel Builds

Rust builds in parallel by default. Use all cores:
```bash
# Set parallel jobs (default is # of CPUs)
export CARGO_BUILD_JOBS=8
```

---

## Troubleshooting

### Linux Issues

**Error**: `linux/bpf.h: No such file or directory`
```bash
# Install kernel headers
sudo apt-get install linux-headers-$(uname -r)  # Ubuntu/Debian
sudo dnf install kernel-devel                   # Fedora/RHEL
```

**Error**: `bpf-linker not found`
```bash
cargo install bpf-linker
```

**Error**: Permission denied when loading BPF
```bash
# Need CAP_BPF or root
sudo ./run_local.sh
```

### macOS Issues

**Problem**: cargo-watch fails to install
```bash
# This is OK - it's optional!
# Use VS Code tasks or manual builds instead
```

**Problem**: Want to build BPF natively
```bash
# Not recommended - use Docker instead
# bpf-linker has LLVM compatibility issues on macOS
./docker/build.sh
```

**Problem**: Docker is slow
```bash
# Use OrbStack instead of Docker Desktop (much faster)
brew install orbstack
```

### Both Platforms

**Problem**: Slow builds
```bash
# Use debug builds for development
cargo build  # Fast: ~5s

# Use release builds for testing
cargo build --release  # Slow: ~20s, but optimized
```

**Problem**: rust-analyzer shows errors
```bash
# Reload VS Code window
Cmd+Shift+P → "Developer: Reload Window"
```

---

## Docker (Both Platforms)

### When to Use Docker

**Linux**:
- ✅ Integration testing (isolated environment)
- ✅ CI/CD pipelines
- ⏭️ Daily development (native is faster)

**macOS**:
- ✅ BPF compilation (required)
- ✅ Integration testing
- ✅ Full stack testing

### Docker Commands

```bash
# Build everything
./docker/build.sh

# Run integration tests
./docker/test.sh

# Benchmark performance
./docker/benchmark.sh

# Enter Docker shell (for debugging)
docker-compose run --rm rauta bash
```

---

## FAQ

**Q: Which platform is better for development?**
A: Linux gives the best experience (everything native), but macOS is excellent too (90% native, 10% Docker).

**Q: Can I develop on Linux and deploy on macOS?**
A: No - eBPF only runs on Linux. macOS is for development only.

**Q: Why not use WSL on Windows?**
A: WSL2 works! Follow the Linux guide. WSL1 won't work (no eBPF support).

**Q: Do I need Docker Desktop on macOS?**
A: Yes, or use OrbStack (recommended - much faster).

**Q: Can I skip Docker entirely on Linux?**
A: Yes! Build and test everything natively.

**Q: Why does macOS use Docker for BPF?**
A: bpf-linker has LLVM compatibility issues on macOS. Docker provides a clean Linux environment.

**Q: Is the hybrid workflow slower on macOS?**
A: No! Unit tests are instant. You only use Docker when testing BPF changes (~10% of time).

---

## Next Steps

### Linux Setup

1. **Run setup**: `./setup.sh`
2. **Build project**: `./build_local.sh`
3. **Run tests**: `cd common && cargo test`
4. **Start developing**: Edit → Build → Test (all native!)

### macOS Setup

1. **Run setup**: `./setup.sh`
2. **Run tests**: `cd common && cargo test`
3. **Edit code**: VS Code or your favorite editor
4. **Test BPF**: `./docker/build.sh` (when ready)

**Happy hacking!** 🦀⚡
