# Git Hooks for RAUTA

## Installation

After cloning the repository, install the git hooks:

```bash
./scripts/git-hooks/install.sh
```

Or manually:

```bash
cp scripts/git-hooks/pre-commit .git/hooks/pre-commit
chmod +x .git/hooks/pre-commit
```

## Pre-Commit Hook

**Purpose**: Prevents unwrap(), expect(), and panic!() in production code.

**What it checks:**
- ✅ Blocks `.unwrap()` in production code
- ✅ Blocks `.expect()` in production code
- ✅ Blocks `panic!()` in production code
- ✅ Allows all of the above in test code (`#[cfg(test)]` modules)

**Why this matters:**
- Lock poisoning from `.unwrap()` on RwLock/Mutex causes cascading panics
- Metric failures shouldn't crash the gateway
- Production code should use safe_read/safe_write/safe_lock helpers

**Override if needed:**
```bash
# If you get a false positive (hook incorrectly blocks test code)
git commit --no-verify -m "your message"
```

**Rules:**
- Production code: Use `safe_read()`, `safe_write()`, `safe_lock()` for locks
- Production code: Use `Result`/`Option` with proper error handling
- Test code: `unwrap()` and `expect()` are perfectly fine!
