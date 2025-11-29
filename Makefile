# RAUTA Development Makefile
# Run these commands locally BEFORE pushing to save CI minutes

.PHONY: help check fmt clippy test build release clean ci-local install-act ci-act

# Default target
help:
	@echo "RAUTA Development Commands"
	@echo "══════════════════════════════════════════════════════════════"
	@echo ""
	@echo "🚀 Quick Commands (run these before committing):"
	@echo "  make check       - Fast compilation check (no codegen)"
	@echo "  make fmt         - Format code"
	@echo "  make clippy      - Run lints"
	@echo "  make test        - Run unit tests"
	@echo "  make ci-local    - Run ALL CI checks locally (recommended)"
	@echo ""
	@echo "🏗️  Build Commands:"
	@echo "  make build       - Debug build"
	@echo "  make release     - Release build (slow, LTO enabled)"
	@echo "  make clean       - Clean build artifacts"
	@echo ""
	@echo "🧪 Local CI Testing (with 'act' - no GitHub minutes):"
	@echo "  make install-act - Install 'act' (run GitHub Actions locally)"
	@echo "  make ci-act      - Run CI workflow locally with 'act'"
	@echo ""
	@echo "💡 Workflow:"
	@echo "  1. make ci-local   (catch issues before commit)"
	@echo "  2. git commit      (pre-commit hook runs)"
	@echo "  3. git push        (CI runs on GitHub)"
	@echo ""
	@echo "══════════════════════════════════════════════════════════════"

# Fast compilation check (catches 90% of issues)
check:
	@echo "🔍 Checking compilation (no codegen)..."
	@cd common && cargo check --all-targets --all-features
	@cd control && cargo check --all-targets --all-features
	@echo "✅ Compilation check passed!"

# Format code
fmt:
	@echo "🎨 Formatting code..."
	@cargo fmt --all
	@echo "✅ Code formatted!"

# Run clippy lints
clippy:
	@echo "📎 Running clippy..."
	@cd common && cargo clippy --all-targets --all-features -- -D warnings
	@cd control && cargo clippy --all-targets --all-features -- -D warnings
	@echo "✅ Clippy passed!"

# Run tests
test:
	@echo "🧪 Running tests..."
	@cd common && cargo test --all-features
	@cd control && cargo test --all-features
	@echo "✅ Tests passed!"

# Debug build
build:
	@echo "🔨 Building debug..."
	@cd control && cargo build
	@echo "✅ Debug build complete!"

# Release build (WARNING: Slow due to LTO)
release:
	@echo "🚀 Building release (this will take a while due to LTO)..."
	@cd control && cargo build --release
	@echo "✅ Release build complete!"
	@ls -lh control/target/release/control

# Clean build artifacts
clean:
	@echo "🧹 Cleaning..."
	@cargo clean
	@echo "✅ Cleaned!"

# Run ALL CI checks locally (RECOMMENDED before pushing)
ci-local:
	@echo "════════════════════════════════════════════════════════════════"
	@echo "🔄 Running CI checks locally (saves GitHub Actions minutes)"
	@echo "════════════════════════════════════════════════════════════════"
	@echo ""
	@echo "📋 [1/4] Format check..."
	@cargo fmt --all -- --check || (echo "❌ Format check failed. Run: make fmt" && exit 1)
	@echo "✅ Format check passed!"
	@echo ""
	@echo "📋 [2/4] Clippy..."
	@cd common && cargo clippy --all-targets --all-features -- -D warnings
	@cd control && cargo clippy --all-targets --all-features -- -D warnings
	@echo "✅ Clippy passed!"
	@echo ""
	@echo "📋 [3/4] Compilation check..."
	@cd common && cargo check --all-targets --all-features
	@cd control && cargo check --all-targets --all-features
	@echo "✅ Compilation check passed!"
	@echo ""
	@echo "📋 [4/4] Tests..."
	@cd common && cargo test --all-features
	@cd control && cargo test --all-features
	@echo "✅ Tests passed!"
	@echo ""
	@echo "════════════════════════════════════════════════════════════════"
	@echo "✅ ALL CI CHECKS PASSED - Safe to commit and push!"
	@echo "════════════════════════════════════════════════════════════════"

# Install 'act' (run GitHub Actions locally)
install-act:
	@echo "📦 Installing 'act' (GitHub Actions runner)..."
	@if command -v brew >/dev/null 2>&1; then \
		echo "Using Homebrew..."; \
		brew install act; \
	elif command -v curl >/dev/null 2>&1; then \
		echo "Using curl..."; \
		curl -s https://raw.githubusercontent.com/nektos/act/master/install.sh | sudo bash; \
	else \
		echo "❌ Neither brew nor curl found. Install manually from: https://github.com/nektos/act"; \
		exit 1; \
	fi
	@echo "✅ 'act' installed! Run: make ci-act"

# Run CI workflow locally with 'act' (no GitHub minutes used)
ci-act:
	@echo "🎬 Running GitHub Actions workflow locally with 'act'..."
	@echo ""
	@echo "Note: This uses Docker to simulate GitHub Actions runners"
	@echo "First run will download Docker images (~1GB)"
	@echo ""
	@if ! command -v act >/dev/null 2>&1; then \
		echo "❌ 'act' not found. Run: make install-act"; \
		exit 1; \
	fi
	@act pull_request --container-architecture linux/amd64
	@echo ""
	@echo "✅ Local CI workflow completed!"
