# Building RAUTA

RAUTA requires Linux to build because eBPF is Linux-only. This guide covers building on **macOS** (using Docker) and **Linux** (native).

## 🚨 Important: Platform Requirements

- **eBPF/XDP**: Linux kernel only (no macOS/Windows support)
- **Aya library**: Uses Linux-specific syscalls (`SYS_bpf`, netlink)
- **Development**: Write code on any OS, build in Linux (Docker/CI)

## Quick Start (macOS with Docker) ⚡

### Option 1: Quick Build Test (Fastest)
```bash
# Build everything in Docker (5-10 minutes first time)
cd docker
./build-quick.sh
```

### Option 2: Full Docker Image
```bash
# Build production Docker image with all components
cd docker
./build.sh
```

### Option 3: GitHub Actions (Zero Setup)
```bash
# Just push code - CI builds automatically!
git push

# Check build status at:
# https://github.com/yairfalse/rauta/actions
```

## Build Components

RAUTA has 3 build targets:

### 1. Common Library (Rust, no_std)
Shared types between eBPF and userspace.

```bash
cd common
cargo build --release
cargo test
```

**Platforms**: ✅ Linux, ✅ macOS, ✅ Windows

### 2. BPF Program (Rust → eBPF bytecode)
XDP program that runs in the Linux kernel.

```bash
cd bpf
cargo +nightly build \
  --release \
  --target=bpfel-unknown-none \
  -Z build-std=core
```

**Requirements**:
- Rust nightly
- `bpf-linker` (`cargo install bpf-linker`)
- LLVM/Clang
- Linux (or Docker on macOS)

**Platforms**: ✅ Linux, 🐳 macOS (Docker only)

### 3. Control Plane (Rust, userspace)
Loads BPF program and manages routes.

```bash
cd control
cargo build --release
```

**Requirements**:
- Aya 0.13 (stable)
- Linux headers

**Platforms**: ✅ Linux, ❌ macOS (Aya uses Linux syscalls)

## Development Workflow

### On macOS (Recommended)

```bash
# 1. Write code in your editor
vim common/src/lib.rs
vim bpf/src/main.rs
vim control/src/main.rs

# 2. Run tests locally (common library only)
cd common && cargo test

# 3. Build in Docker when ready
cd docker && ./build-quick.sh

# 4. Or just push and let CI build
git add -A
git commit -m "feat: Add new feature"
git push  # CI builds automatically
```

### On Linux (Native)

```bash
# Install dependencies
sudo apt-get install -y \
  clang llvm libelf-dev \
  linux-headers-$(uname -r) \
  pkg-config build-essential

# Install bpf-linker
cargo install bpf-linker

# Build everything
cargo build --release                           # Common
cd bpf && cargo +nightly build --release \
  --target=bpfel-unknown-none -Z build-std=core # BPF
cd ../control && cargo build --release          # Control
```

## Continuous Integration (GitHub Actions)

Every push to `main`, `dev`, or `feat/*` branches triggers:

### ✅ Quick Checks (Fast - 2 minutes)
- `cargo fmt --check` (format)
- `cargo clippy` (lints)
- `cargo test` (unit tests)

### ✅ Build BPF (5 minutes)
- Compiles BPF program for kernel
- Uploads artifact for testing

### ✅ Build Control Plane (3 minutes)
- Compiles userspace control plane
- Uploads binary artifact

### ✅ Security Audit
- Runs `cargo audit` for vulnerabilities

**Status**: Check https://github.com/yairfalse/rauta/actions

## Docker Images

### Development Image (Full Toolchain)
```dockerfile
FROM rust:1.75-bookworm
# Includes: Rust, bpf-linker, LLVM, kernel headers
```

**Use case**: Building and testing RAUTA

### Runtime Image (Minimal)
```dockerfile
FROM ubuntu:22.04
# Includes: Only runtime dependencies (iproute2, bpftool)
```

**Use case**: Deploying RAUTA in Kubernetes

## Troubleshooting

### "cannot find `SYS_bpf` in crate `libc`" (macOS)

**Cause**: Aya uses Linux-specific syscalls

**Fix**: Use Docker
```bash
cd docker && ./build-quick.sh
```

### "unable to find LLVM shared lib" (macOS)

**Cause**: bpf-linker can't find LLVM on macOS

**Fix**: Set LLVM path or use Docker
```bash
# Option 1: Set LLVM path
export LLVM_SYS_PREFIX="/opt/homebrew/opt/llvm"

# Option 2: Use Docker (easier)
cd docker && ./build-quick.sh
```

### "could not read `target/bpfel-unknown-none/release/rauta`"

**Cause**: BPF binary not built yet

**Fix**: Build BPF first, then control plane
```bash
# In Docker
cd docker && ./build-quick.sh

# Or manually
cd bpf && cargo +nightly build --release \
  --target=bpfel-unknown-none -Z build-std=core
cd ../control && cargo build --release
```

## Build Artifacts

After successful build:

```
target/
├── bpfel-unknown-none/release/
│   └── rauta                      # eBPF binary (kernel)
├── release/
│   └── control                    # Control plane (userspace)
└── debug/
    └── common-*.rlib              # Common library
```

## Next Steps

- **Run Tests**: `./docker/test.sh`
- **Deploy**: See [DEPLOYMENT.md](DEPLOYMENT.md)
- **Develop**: See [CONTRIBUTING.md](CONTRIBUTING.md)

---

**TL;DR**:
- **macOS**: Use `docker/build-quick.sh` or push to GitHub (CI builds)
- **Linux**: `cargo build --release` (after installing deps)
- **Testing**: Let CI handle it automatically
