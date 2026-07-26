# AGENTS.md

This file provides guidance to Claude Code (claude.ai/code) and AI agent assistants when working with code in this repository.

## Overview

`clix` is a fast, modular CLI suite written in Rust designed to streamline daily developer tasks, GitHub workflows, social media tools, and system utilities.

It is structured as a **Cargo Workspace**:
- **Dual Invocation**: User-facing tools run through the unified CLI (`clix gh stars`, `clix x bookmarks`, `clix x read`, `clix md view`) or a standalone binary.
- **Zero-Config GitHub Auth**: Detects `GITHUB_TOKEN`, `GH_TOKEN`, or `gh auth token`; usernames come from the authenticated `gh` account or an explicit `github.user`.

## Commands

All verification gates are defined as `mise` tasks in `.mise.toml`:

```sh
mise run verify        # fmt + check + test + clippy + docs (full local gate)
mise run fmt           # cargo fmt --all -- --check
mise run check         # cargo check --workspace --locked
mise run test          # cargo test --workspace --locked
mise run clippy        # warnings + Clippy pedantic/nursery are denied
mise run docs          # rustdoc with warnings denied
mise run build         # cargo build --workspace --release --locked
```

Run a single test: `cargo test <name_substring> -- --nocapture`

Rust toolchain is pinned to `1.96.1` (Rust edition 2024) via `.mise.toml`.

## Architecture

The project follows a Cargo Workspace layout under `crates/`:

```
clix/
  ├── Cargo.toml              # Root workspace manifest and shared dependencies
  ├── .mise.toml              # Verification and build tasks
  ├── crates/
  │   ├── clix-core/          # Shared UI, filesystem, and GitHub config helpers
  │   ├── clix-gh-stars/      # GitHub stars exporter
  │   ├── clix-view/          # Terminal Markdown/MDX viewer
  │   ├── clix-x-api/         # Shared X auth, GraphQL parsing, and content/media types
  │   ├── clix-x-bookmarks/   # X bookmarks exporter
  │   └── clix-x-read/        # Single X status/article reader
  └── src/main.rs             # Unified `clix` dispatcher
```

### Key Design Points

- **`clix-core` (Shared Infrastructure):** Provides shared terminal UI, atomic filesystem writes, and GitHub token/username resolution.
- **`clix-x-api` (Shared X Infrastructure):** Owns X credentials, HTTP client setup, GraphQL parsing, media helpers, and the common content taxonomy.
- **Feature Crates:** Each user-facing tool exposes its argument type and `run` entrypoint while retaining a standalone binary.
- **Root Binary (`clix`):** Uses `clap` to dispatch unified subcommands directly to feature crates.

## Adding a New Tool

To add a new tool `clix-<service>-<name>` to the workspace:

1. Create a new crate in `crates/clix-<service>-<name>`:
   ```sh
   mkdir -p crates/clix-<service>-<name>/src
   ```
2. Add `"crates/clix-<service>-<name>"` to `[workspace.members]` in root `Cargo.toml`.
3. Inherit common package/dependency values from the root workspace and add only the required path dependencies.
4. Implement the tool in `crates/clix-<service>-<name>/src/lib.rs`, exposing its argument type and a `run` entrypoint (async only when the work requires it).
5. Add `crates/clix-<service>-<name>/src/main.rs` for standalone binary execution (`clix-<service>-<name>`).
6. Register the subcommand in root `src/main.rs` under `enum Commands` and the service-specific enum (`enum GhCommands`, `enum XCommands`, etc.).
