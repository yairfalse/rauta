# Contributing to RAUTA

Thanks for your interest in contributing! RAUTA is a learning project focused on production-grade eBPF/XDP networking.

## Quick Start

```bash
# 1. Fork the repo on GitHub
# 2. Clone your fork
git clone https://github.com/YOUR_USERNAME/rauta.git
cd rauta

# 3. Set up development environment
./setup.sh

# 4. Create a branch
git checkout -b feat/your-feature

# 5. Make changes, test, commit, push
# 6. Open a Pull Request
```

## Development Workflow

We follow **Test-Driven Development (TDD)**:

### RED → GREEN → REFACTOR

```bash
# 1. RED: Write a failing test
cd common
vim tests/your_test.rs

cargo test test_new_feature
# ❌ test_new_feature ... FAILED

# 2. GREEN: Make it pass with minimal code
vim src/lib.rs

cargo test test_new_feature
# ✅ test_new_feature ... ok

# 3. REFACTOR: Improve code quality
# - Tests stay green!
# - Run clippy and fmt
```

## Code Standards

### Before Committing

```bash
# Format code
cargo fmt --all

# Lint code (must pass!)
cargo clippy --all-targets --all-features -- -D warnings

# Run tests (must pass!)
cargo test --all

# Build (must succeed!)
./build_local.sh  # Linux
./docker/build.sh # macOS
```

### Code Quality Rules

1. **No TODOs or stubs** - Complete implementations only
2. **Tests required** - All new features need tests
3. **Inline docs** - Public functions need `///` comments
4. **Error handling** - Use `Result<T, E>`, never `unwrap()` in production paths

### Commit Messages

Format: `<type>: <description>`

**Types:**
- `feat:` - New feature
- `fix:` - Bug fix
- `perf:` - Performance improvement
- `refactor:` - Code refactoring
- `test:` - Test additions/changes
- `docs:` - Documentation only
- `chore:` - Build/tooling changes

**Examples:**
```
feat: Add HTTP/2 support to XDP parser
fix: Correct checksum calculation for fragmented packets
perf: Use SIMD for batch packet processing
docs: Update DEVELOPMENT.md with Linux setup
```

## Pull Request Process

1. **One feature per PR** - Keep changes focused
2. **Update tests** - Add/modify tests for your changes
3. **Pass CI checks** - fmt, clippy, tests must pass
4. **Update docs** - README/DEVELOPMENT.md if behavior changes
5. **Request review** - Tag maintainers when ready

### PR Template

```markdown
## Summary
Brief description of what this PR does.

## Changes
- Bullet list of changes

## Testing
How did you test this? (unit tests, integration tests, manual)

## Checklist
- [ ] Tests added/updated
- [ ] `cargo fmt` run
- [ ] `cargo clippy` passes
- [ ] Documentation updated
- [ ] Commit messages follow format
```

## Project Structure

```
rauta/
├── common/       # Shared types (Pod-compatible for BPF)
├── bpf/          # XDP program (kernel space)
├── control/      # Control plane (userspace)
├── cli/          # CLI tool (future)
├── tests/        # Integration tests
└── docker/       # Docker environment
```

### Where to Contribute

**Good First Issues:**
- Add new HTTP methods (TRACE, CONNECT)
- Improve error messages
- Add integration tests
- Documentation improvements

**Advanced:**
- Maglev consistent hashing implementation
- Connection tracking for graceful draining
- TC-BPF Tier 2 implementation
- Prometheus metrics exporter

## Questions?

- **Issues**: Open a GitHub issue
- **Discussions**: Use GitHub Discussions for questions
- **Documentation**: Check [DEVELOPMENT.md](DEVELOPMENT.md) first

## License

By contributing, you agree that your contributions will be licensed under the Apache License 2.0.

---

**Happy hacking!** 🦀⚡
