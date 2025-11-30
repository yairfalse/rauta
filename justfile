# RAUTA - Gateway API Controller
# Fast, reliable development workflow

# Show available commands (default)
default:
  @just --list

# === Quick Commands (Most Used) ===

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

# Run all tests (including integration)
test-all:
  @echo "🧪 Running all tests..."
  cargo test --workspace --all-features

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

# Run tests with output
test-verbose:
  @echo "🧪 Running tests (verbose)..."
  cargo test --workspace -- --nocapture

# Check what will be committed
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

# === Docker ===

# Build Docker image locally
build-docker:
  @echo "🐳 Building Docker image..."
  docker build -f docker/Dockerfile.control-local -t rauta-control:dev .

# Load image to Kind
load-kind: build-docker
  @echo "📦 Loading image to Kind..."
  kind load docker-image rauta-control:dev --name rauta-dev

# === Cleanup ===

# Clean build artifacts
clean:
  @echo "🧹 Cleaning build artifacts..."
  cargo clean

# Clean Kind cluster
clean-kind:
  @echo "🧹 Cleaning Kind cluster..."
  kind delete cluster --name rauta-dev

# Clean everything
clean-all: clean clean-deploy
  @echo "✅ All cleaned!"

# === Setup ===

# Install development dependencies
setup:
  @echo "📦 Installing dev dependencies..."
  cargo install cargo-watch cargo-nextest cargo-audit
  brew install just skaffold kind kubectl
  @echo "✅ Setup complete!"

# Create Kind cluster
create-cluster:
  @echo "🏗️  Creating Kind cluster..."
  kind create cluster --name rauta-dev
  kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.1.0/standard-install.yaml
  @echo "✅ Cluster ready!"

# === Git Shortcuts ===

# Stage all and commit (runs pre-commit checks)
commit MESSAGE: fmt-check lint
  git add .
  git commit -m "{{MESSAGE}}"

# Quick commit + push (after full CI)
ship MESSAGE: ci
  git add .
  git commit -m "{{MESSAGE}}"
  git push

# === Benchmarks ===

# Run benchmarks (if criterion is set up)
bench:
  @echo "⚡ Running benchmarks..."
  cargo bench || echo "⚠️  No benchmarks configured"

# === Meta ===

# Show tool versions
versions:
  @echo "Rust:     $(rustc --version)"
  @echo "Cargo:    $(cargo --version)"
  @echo "Just:     $(just --version || echo 'not installed')"
  @echo "Skaffold: $(skaffold version || echo 'not installed')"
  @echo "Kind:     $(kind version || echo 'not installed')"
  @echo "Kubectl:  $(kubectl version --client --short 2>/dev/null || echo 'not installed')"

# Show project stats
stats:
  @echo "📊 Project Statistics:"
  @echo "Rust files:  $(find . -name '*.rs' -not -path './target/*' | wc -l)"
  @echo "Total lines: $(find . -name '*.rs' -not -path './target/*' | xargs wc -l | tail -1)"
  @echo "Git commits: $(git rev-list --count HEAD)"
  @echo "Branches:    $(git branch -a | wc -l)"
