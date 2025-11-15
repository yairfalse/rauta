# Building RAUTA

RAUTA is a pure Rust userspace HTTP proxy. This guide covers building on **macOS**, **Linux**, and **Windows**.

## 🚨 Important: Platform Requirements

- **Pure Rust**: Works on Linux, macOS, and Windows
- **Development**: Write and build on any OS
- **Runtime**: Deploy on Linux (Kubernetes target platform)

## Quick Start ⚡

### Option 1: Direct Build (All Platforms)
```bash
# Build everything (common + control)
cargo build --release

# Run tests
cargo test
```

### Option 2: Docker Build
```bash
# Build in Docker (ensures Linux target)
cd docker
./build-quick.sh
```

### Option 3: GitHub Actions (Zero Setup)
```bash
# Just push code - CI builds automatically!
git push

# Check build status at:
# https://github.com/yairfalse/rauta/actions
```

## Build Components

RAUTA has 2 build targets:

### 1. Common Library (Rust, no_std)
Shared types and utilities (Maglev, HTTP parsing, etc).

```bash
cd common
cargo build --release
cargo test
```

**Platforms**: ✅ Linux, ✅ macOS, ✅ Windows

### 2. Control Plane (Rust, userspace)
HTTP proxy and Kubernetes Gateway API controller.

```bash
cd control
cargo build --release
cargo test
```

**Requirements**:
- Rust stable (1.75+)
- tokio, hyper, kube-rs

**Platforms**: ✅ Linux, ✅ macOS, ✅ Windows

## Development Workflow

### On Any Platform (Recommended)

```bash
# 1. Write code in your editor
vim common/src/lib.rs
vim control/src/main.rs

# 2. Run tests locally
cargo test

# 3. Build release binary
cargo build --release

# 4. Or just push and let CI build
git add -A
git commit -m "feat: Add new feature"
git push  # CI builds automatically
```

### On Linux (Native Kubernetes Development)

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build everything
cargo build --release

# Run in kind cluster
kind create cluster --name rauta-dev
kubectl apply -f deploy/
```

## Continuous Integration (GitHub Actions)

Every push to `main`, `dev`, or `feat/*` branches triggers:

### ✅ Quick Checks (Fast - 2 minutes)
- `cargo fmt --check` (format)
- `cargo clippy` (lints)
- `cargo test` (unit tests)

### ✅ Build Release (3 minutes)
- Compiles optimized release binary
- Runs integration tests
- Uploads binary artifact

### ✅ Security Audit
- Runs `cargo audit` for vulnerabilities

**Status**: Check https://github.com/yairfalse/rauta/actions

## Docker Images

### Development Image (Full Toolchain)
```dockerfile
FROM rust:1.75-bookworm
# Includes: Rust stable, build tools
```

**Use case**: Building and testing RAUTA

### Runtime Image (Minimal)
```dockerfile
FROM debian:bookworm-slim
# Includes: Only runtime dependencies
```

**Use case**: Deploying RAUTA in Kubernetes

## Troubleshooting

### "failed to compile OpenSSL"

**Cause**: Missing OpenSSL development libraries (needed for kube-rs TLS)

**Fix**: Install OpenSSL dev package
```bash
# Ubuntu/Debian
sudo apt-get install -y pkg-config libssl-dev

# macOS
brew install openssl pkg-config

# Set PKG_CONFIG_PATH if needed
export PKG_CONFIG_PATH="/opt/homebrew/opt/openssl/lib/pkgconfig"
```

### "linker `cc` not found"

**Cause**: Missing C compiler (needed for some dependencies)

**Fix**: Install build tools
```bash
# Ubuntu/Debian
sudo apt-get install -y build-essential

# macOS
xcode-select --install

# Windows
# Install Visual Studio Build Tools
```

### Tests fail with "address already in use"

**Cause**: Another test or process is using test ports

**Fix**: Run tests serially or kill conflicting processes
```bash
# Run tests one at a time
cargo test -- --test-threads=1

# Or kill processes on test ports
lsof -ti:8080,9090 | xargs kill -9
```

## Build Artifacts

After successful build:

```
target/
├── release/
│   ├── control                    # RAUTA binary (HTTP proxy + K8s controller)
│   └── libcommon-*.rlib          # Common library
└── debug/
    └── ...                        # Debug builds
```

## Performance Builds

For maximum performance:

```bash
# Release with LTO and optimizations
cargo build --release

# Profile-guided optimization (advanced)
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

The workspace `Cargo.toml` already includes:
- `lto = true` (Link-Time Optimization)
- `opt-level = 3` (Maximum optimization)
- `codegen-units = 1` (Better optimization, slower compile)

## Next Steps

- **Run Tests**: `cargo test`
- **Deploy**: See `deploy/README.md`
- **Develop**: See [CONTRIBUTING.md](CONTRIBUTING.md)

---

**TL;DR**:
- **Any OS**: `cargo build --release && cargo test`
- **Docker**: `docker/build-quick.sh`
- **CI**: Push to GitHub, builds automatically
