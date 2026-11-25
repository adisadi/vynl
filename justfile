# Unified developer workflow for vynl (https://github.com/casey/just)
set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# Run full local verification suite (fmt, clippy, test)
check: fmt-check lint test

# Format all Rust code
fmt:
    cargo fmt --all

# Check formatting without modifying files
fmt-check:
    cargo fmt --all -- --check

# Run strict workspace clippy lints
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Run tests (uses cargo-nextest when installed, falls back to cargo test)
test:
    @if command -v cargo-nextest >/dev/null 2>&1; then \
        cargo nextest run --workspace; \
    else \
        cargo test --workspace; \
    fi

# Check all feature combinations (requires cargo-hack)
feature-powerset:
    cargo hack check --workspace --feature-powerset --no-dev-deps

# Run supply-chain & license checks (requires cargo-deny)
deny:
    cargo deny check

# Review UI snapshot updates (requires cargo-insta)
snapshot-review:
    cargo insta review

# Generate LLVM code coverage summary (requires cargo-llvm-cov)
cov:
    cargo llvm-cov --workspace --lcov --output-path lcov.info

# Run vynl diagnostics in the current terminal
doctor:
    cargo run -p vynl-tui -- doctor

# Build and serve mdBook documentation locally
book:
    mdbook serve docs/book --open

# Record terminal demo GIFs with VHS
demo:
    vhs docs/demos/hero.tape
