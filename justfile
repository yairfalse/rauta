# RAUTA - Gateway API Controller

# Show available commands (default)
default:
  @just --list

# === Quick Commands ===

# Format Rust code
fmt:
  @echo "📝 Formatting..."
  cargo fmt --all

# Run tests (fast)
test:
  @echo "🧪 Running tests..."
  cargo test --workspace

# Start Skaffold dev loop (auto-rebuild on change)
dev:
  @echo "🚀 Starting Skaffold dev..."
  skaffold dev

# === CI Checks (Before Push) ===

# Check formatting (CI)
fmt-check:
  @echo "🔍 Checking format..."
  cargo fmt --all --check

# Lint with clippy (strict)
lint:
  @echo "🔍 Running clippy..."
  cargo clippy --all-targets --all-features -- -D warnings

# Run all tests (unit + doc tests, excludes integration tests)
test-all:
  @echo "🧪 Running all tests..."
  cargo test --workspace --all-features --lib --bins

# Security audit
audit:
  @echo "🔒 Running security audit..."
  cargo audit || echo "⚠️  cargo-audit not installed (run: cargo install cargo-audit)"

# Build release binary
build:
  @echo "🔨 Building release..."
  cargo build --release --package control

# Full CI (run before pushing)
ci: fmt-check lint test-all audit build
  @echo "✅ Local CI passed! Safe to push."

# === Development Helpers ===

# Run tests with live reload (requires cargo-watch)
watch:
  @echo "👀 Watching for changes..."
  cargo watch -x test || echo "❌ cargo-watch not installed (run: cargo install cargo-watch)"

# Run specific test
test-one TEST:
  @echo "🧪 Running test: {{TEST}}"
  cargo test {{TEST}} -- --nocapture

# Check uncommitted changes
diff:
  @echo "📝 Uncommitted changes:"
  git diff
  @echo "\n📦 Staged changes:"
  git diff --cached

# === Kubernetes Workflows ===

# Deploy to Kind cluster (one-off)
deploy:
  @echo "📦 Deploying to Kind..."
  skaffold run

# Delete deployment
clean-deploy:
  @echo "🧹 Cleaning deployment..."
  skaffold delete

# View controller logs
logs:
  @echo "📋 Controller logs:"
  kubectl logs -f -l app=rauta-control -n rauta-system

# Restart controller
restart:
  @echo "🔄 Restarting controller..."
  kubectl rollout restart daemonset/rauta-control -n rauta-system

# === Cleanup ===

# Clean build artifacts
clean:
  @echo "🧹 Cleaning build artifacts..."
  cargo clean

# === Git Shortcuts ===

# Commit (runs pre-commit checks)
commit MESSAGE: fmt-check lint
  git add .
  git commit -m "{{MESSAGE}}"

# Quick commit + push (after full CI)
ship MESSAGE: ci
  git add .
  git commit -m "{{MESSAGE}}"
  git push

# === Setup ===

# Install git hooks (pre-commit + pre-push)
install-hooks:
  @echo "🔗 Installing git hooks..."
  @if [ ! -f .git/hooks/pre-commit ]; then \
    echo "⚠️  Pre-commit hook not found (expected in .git/hooks/pre-commit)"; \
  else \
    chmod +x .git/hooks/pre-commit && echo "✅ Pre-commit hook installed"; \
  fi
  @if [ ! -f .git/hooks/pre-push ]; then \
    echo "⚠️  Pre-push hook not found (expected in .git/hooks/pre-push)"; \
  else \
    chmod +x .git/hooks/pre-push && echo "✅ Pre-push hook installed"; \
  fi
  @echo "✅ Hooks ready!"

# === Meta ===

# Show tool versions
versions:
  @echo "Rust:     $(rustc --version)"
  @echo "Cargo:    $(cargo --version)"
  @echo "Just:     $(just --version)"
  @echo "Skaffold: $(skaffold version)"

# Show project stats
stats:
  @echo "📊 Project Statistics:"
  @echo "Rust files:  $(find . -name '*.rs' -not -path './target/*' | wc -l)"
  @echo "Total lines: $(find . -name '*.rs' -not -path './target/*' | xargs wc -l | tail -1)"
  @echo "Git commits: $(git rev-list --count HEAD)"
