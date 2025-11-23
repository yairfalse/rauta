#!/bin/bash
# Install git hooks for RAUTA development

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GIT_HOOKS_DIR="$(git rev-parse --git-dir)/hooks"

echo "📦 Installing RAUTA git hooks..."

# Install pre-commit hook
cp "$SCRIPT_DIR/pre-commit" "$GIT_HOOKS_DIR/pre-commit"
chmod +x "$GIT_HOOKS_DIR/pre-commit"

echo "✅ Pre-commit hook installed"
echo ""
echo "🛡️  The pre-commit hook will block unwrap()/expect()/panic!() in production code"
echo "   (Test code is allowed to use these)"
echo ""
echo "To bypass the hook if needed: git commit --no-verify"
