# Contributing to Exoskeleton

Thank you for your interest in contributing to Exoskeleton! This document covers the
development workflow, coding standards, and submission process.

## Prerequisites

- **Rust toolchain:** 1.89.0 (pinned in `rust-toolchain.toml`)
- **cargo** with fmt, clippy components

## Building

```bash
cargo build --workspace
```

## Running Tests

```bash
# Full test suite
cargo test --workspace

# Specific crate
cargo test -p exoskeleton-core
cargo test -p exoskeleton-host

# Integration tests
cargo test --test integration -p exoskeleton-host
```

## Linting & Formatting

All code must pass these checks before merge:

```bash
cargo fmt --all -- --check
cargo clippy --all --all-targets -- -D warnings
```

Key lint thresholds (see `clippy.toml`):
- `too-many-arguments-threshold = 5`
- `too-many-lines-threshold = 80`
- `cognitive-complexity-threshold = 15`

Formatting rules (see `rustfmt.toml`):
- 4-space indent, 100-character max line width
- `group_imports = "StdExternalCrate"`

## Crate Architecture

Exoskeleton is organized as 7 workspace crates with a strict dependency DAG:

```
exoskeleton-core (pure domain types, no I/O)
 ├─ exoskeleton-memory (Context Compiler + memory tiers)
 ├─ exoskeleton-relationship (Relationship Ledger + Snapshot + Align)
 ├─ exoskeleton-threads (Thread registry, execution, context slicing)
 └─ exoskeleton-host (Vessel: dual-engine boot, master loop, LLM handler)
     ├─ exoskeleton-daemon (HTTP API)
     └─ exoskeleton-cli (CLI binary)
```

Key principles:
- **`exoskeleton-core`** has no ActionQueue/WorldInterface dependency and no I/O
- Two physically separate ActionQueue engines (Cognitive AQ + Tool AQ) — never merge them
- LLM calls are cognitive work, not tool invocations
- Threads produce recommendation artifacts only — they never invoke tools
- Use `thiserror` for error types, `tracing` for instrumentation

## Sacred Invariants

These are non-negotiable — never weaken them in a contribution:

1. No external state changes outside adapters
2. Every external action gets `run_id` (idempotent)
3. Everything replayable from both AQ WALs + artifacts
4. Least privilege by default
5. Context compiled, not accumulated (token budgets)
6. Budgets enforced independently per engine
7. Single coherent workspace (one State Snapshot, one master loop)
8. Relationship awareness is durable and explicit
9. Cognitive and tool execution isolated (dual-engine, never merge)

See the project documentation for the full invariant specification.

## Submitting Changes

1. Fork the repository and create a feature branch
2. Make your changes, ensuring all tests pass
3. Run `cargo fmt --all` and `cargo clippy` before committing
4. Open a pull request against `main` with a clear description of the change

## License

By contributing, you agree that your contributions will be licensed under the
MIT License that covers this project.
