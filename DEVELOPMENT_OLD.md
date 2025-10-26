# RAUTA Development Guide (macOS)

Complete guide for developing RAUTA on macOS.

## Overview

You can **develop and build** RAUTA entirely on macOS:
- ✅ Edit code natively (VS Code, any editor)
- ✅ Compile BPF program (cross-compile for Linux)
- ✅ Compile control plane
- ✅ Run unit tests
- ✅ Fast iteration (no VM overhead)

You only need Linux/Docker for:
- 🐧 Running the XDP program (eBPF requires Linux kernel)
- 🐧 Integration testing with real HTTP traffic

## Quick Setup

### One-Command Setup

```bash
./setup_macos.sh
```

This installs:
- LLVM (via Homebrew)
- Rust + bpfel-unknown-none target
- bpf-linker
- cargo-watch (auto-rebuild)

**Time**: ~5-10 minutes (downloads LLVM)

### Manual Setup (if you prefer)

```bash
# 1. Install Homebrew (if not already installed)
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# 2. Install LLVM
brew install llvm

# 3. Add LLVM to PATH
echo 'export PATH="$(brew --prefix llvm)/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc

# 4. Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 5. Install Rust nightly (required for BPF compilation)
rustup toolchain install nightly

# 6. Install bpf-linker (handles BPF target compilation)
cargo install bpf-linker

# 7. Install cargo-watch (OPTIONAL, for auto-rebuild)
cargo install cargo-watch

# Note: cargo-watch may fail on some macOS versions due to
#       notification library issues. It's optional - you can
#       rebuild manually or use VS Code tasks instead.

# Note: You don't need to add bpfel-unknown-none via rustup.
#       The bpf-linker handles BPF compilation directly.
```

## Development Workflow

### Quick Commands

```bash
# Build everything
./build_local.sh

# Build just BPF
cd bpf && cargo +nightly build --release --target=bpfel-unknown-none

# Build just control plane
cd control && cargo build --release

# Run tests
cd common && cargo test

# Auto-rebuild BPF on file changes
./watch_bpf.sh

# Auto-rebuild control on file changes
./watch_control.sh

# Test in Docker (requires Docker Desktop)
./docker/test.sh
```

### VS Code Integration

If you use VS Code, the project includes:

**Tasks (Cmd+Shift+B)**:
- `Build All` - Build BPF + control plane
- `Build BPF` - Build only BPF program
- `Build Control` - Build only control plane
- `Test Common` - Run unit tests
- `Watch BPF` - Auto-rebuild on changes
- `Docker Test` - Run integration tests

**Run Tasks**: Press `Cmd+Shift+P` → "Tasks: Run Task"

### Project Structure for Development

```
rauta/
├── common/          # Start here - shared types
│   ├── src/lib.rs
│   └── tests/
│
├── bpf/             # XDP program (Linux target)
│   ├── src/
│   │   ├── main.rs       # Entry point
│   │   └── forwarding.rs # Packet forwarding
│   └── Cargo.toml
│
├── control/         # Control plane (native macOS)
│   ├── src/
│   │   ├── main.rs
│   │   ├── error.rs
│   │   └── routes.rs
│   └── Cargo.toml
│
└── cli/             # CLI tool (future)
```

## Typical Development Flow

### 1. Edit Code (macOS native)

```bash
# Open in VS Code
code .

# Or your favorite editor
vim bpf/src/forwarding.rs
```

### 2. Build Locally (fast!)

```bash
# Build all components
./build_local.sh

# Or build specific component
cd bpf
cargo +nightly build --release --target=bpfel-unknown-none
```

**Build time**: ~5-10 seconds (incremental builds)

### 3. Run Tests (macOS)

```bash
# Unit tests (run on macOS)
cd common
cargo test

# All tests
cargo test
```

**Test time**: <1 second

### 4. Test in Docker (when ready)

```bash
# Full integration test (requires Docker)
./docker/test.sh
```

**Test time**: ~30 seconds (first run slower due to image build)

## Advanced Workflows

### Watch Mode (Auto-rebuild)

**Option 1: Using cargo-watch** (if installed):
```bash
# Terminal 1 - Watch BPF
./watch_bpf.sh

# Terminal 2 - Watch control
./watch_control.sh
```

**Option 2: Using VS Code tasks** (recommended if cargo-watch fails):
- Press `Cmd+Shift+B` → "Build All"
- Or: `Cmd+Shift+P` → "Tasks: Run Task" → "Watch BPF"

**Option 3: Manual rebuild**:
```bash
# Just rebuild when needed
cd bpf && cargo +nightly build --release --target=bpfel-unknown-none
```

Now edit any `.rs` file and it rebuilds automatically (Option 1-2)!

### Testing Workflow

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

### Debugging Control Plane

macOS can run the control plane (but XDP won't work):

```bash
cd control

# Build with debug symbols
cargo build

# Run with debug logging
RUST_LOG=debug ./target/debug/control

# Note: XDP attach will fail on macOS (expected)
# This is useful for testing logic, not XDP itself
```

### Cross-Platform Tips

**What works on macOS**:
- ✅ Editing code
- ✅ Compiling BPF (targeting Linux)
- ✅ Compiling control plane
- ✅ Unit tests
- ✅ Linting (clippy)
- ✅ Formatting (rustfmt)

**What needs Linux**:
- 🐧 Loading BPF program into kernel
- 🐧 Attaching XDP to network interface
- 🐧 Integration tests with real packets

## Troubleshooting

### Build Errors

**Error**: `bpf-linker not found`
```bash
cargo install bpf-linker
```

**Error**: `linking with cc failed`
```bash
# Ensure LLVM is in PATH
export PATH="$(brew --prefix llvm)/bin:$PATH"

# Add to ~/.zshrc for persistence
echo 'export PATH="$(brew --prefix llvm)/bin:$PATH"' >> ~/.zshrc
```

**Error**: `target bpfel-unknown-none not found` or `component unavailable`
```bash
# You don't need to install the target via rustup
# Just ensure you have nightly and bpf-linker:
rustup toolchain install nightly
cargo install bpf-linker

# The bpf-linker handles BPF compilation directly
```

### Slow Builds

**Problem**: First build takes 5+ minutes

**Solution**: This is normal. Subsequent builds are fast (~5s).

**Tips**:
- Use `cargo build` (debug) for development
- Use `cargo build --release` only for testing
- Use `./watch_bpf.sh` to avoid manual rebuilds

### Docker Issues

**Problem**: Docker is slow on macOS

**Solution**: You don't need Docker for development!
- Develop entirely on macOS
- Build on macOS
- Test on macOS (unit tests)
- Use Docker only for final integration testing

**Alternative**: Use OrbStack instead of Docker Desktop
```bash
brew install orbstack
```
OrbStack is much faster than Docker Desktop on macOS.

### Rust Analyzer Issues

**Problem**: rust-analyzer shows errors for BPF code

**Solution**: Configure workspace properly

`.vscode/settings.json`:
```json
{
  "rust-analyzer.cargo.target": "bpfel-unknown-none",
  "rust-analyzer.check.targets": ["bpfel-unknown-none"]
}
```

Restart VS Code after changing settings.

## Performance Tips

### Fast Iteration

**Use watch mode**:
```bash
./watch_bpf.sh
```
Rebuilds automatically when you save. No manual commands!

**Use debug builds for development**:
```bash
cargo build  # Fast: ~5s
```

**Use release builds for testing**:
```bash
cargo build --release  # Slow: ~20s, but optimized
```

### Incremental Compilation

Rust's incremental compilation makes subsequent builds fast:
- First build: ~2 minutes
- Incremental: ~5 seconds

**Tip**: Don't run `cargo clean` unless necessary.

### Parallel Builds

Rust builds in parallel by default. Use all cores:
```bash
# Set parallel jobs (default is # of CPUs)
export CARGO_BUILD_JOBS=8
```

## IDE Setup

### VS Code (Recommended)

Extensions:
- **rust-analyzer** - Rust language support
- **CodeLLDB** - Debugging support
- **Even Better TOML** - Cargo.toml editing

Settings included in `.vscode/settings.json`.

### Other Editors

**Vim/Neovim**:
```bash
# Install rust.vim
git clone https://github.com/rust-lang/rust.vim ~/.vim/pack/plugins/start/rust.vim
```

**IntelliJ IDEA**:
- Install Rust plugin
- Configure LLVM path in settings

## Continuous Integration

### Pre-commit Checks

Before committing:
```bash
# Format code
cargo fmt --all

# Lint code
cargo clippy --all-targets --all-features

# Run tests
cargo test --all

# Build all
./build_local.sh
```

### Git Hooks

Create `.git/hooks/pre-commit`:
```bash
#!/bin/bash
cargo fmt --all -- --check
cargo clippy -- -D warnings
cargo test --all
```

Make it executable:
```bash
chmod +x .git/hooks/pre-commit
```

## FAQ

**Q: Do I need a Linux VM for development?**
A: No! Develop entirely on macOS. Use Docker only for testing.

**Q: Is the BPF program compatible with macOS?**
A: BPF code compiles on macOS but only runs on Linux (kernel requirement).

**Q: How fast is cross-compilation?**
A: Very fast! ~5-10 seconds for incremental builds.

**Q: Can I use Colima instead of Docker Desktop?**
A: Yes, but setup script and Docker Compose work with any Docker-compatible runtime.

**Q: What about OrbStack?**
A: OrbStack works great! Much faster than Docker Desktop on macOS.

**Q: Do I need to restart my Mac after setup?**
A: No. Just restart your terminal: `source ~/.zshrc`

## Next Steps

1. **Run setup**: `./setup_macos.sh` (one time)
2. **Build project**: `./build_local.sh`
3. **Start developing**: Edit code, auto-rebuild with `./watch_bpf.sh`
4. **Test locally**: `cd common && cargo test`
5. **Test in Docker**: `./docker/test.sh` (when ready)

**Happy hacking!** 🦀⚡
