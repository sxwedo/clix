# AGENTS.md

This file provides guidance to Claude Code (claude.ai/code) and AI agent assistants when working with code in this repository.

## Overview

`clix` is a fast, modular CLI suite written in Rust designed to streamline daily developer tasks, GitHub workflows, social media tools, and system utilities.

It is structured as a **Cargo Workspace**:
- **Dual Invocation**: Every tool can be run via the unified CLI (`clix gh stars`) or directly as a standalone binary (`clix-gh-stars`).
- **Zero-Config Auth**: Automatically detects tokens via `gh auth token` or `GITHUB_TOKEN` env var, and usernames via `gh` or `git config`.

## Commands

All verification gates are defined as `mise` tasks in `.mise.toml`:

```sh
mise run verify        # fmt + check + test + clippy (full local gate)
mise run fmt           # cargo fmt -- --check
mise run check         # cargo check --workspace
mise run test          # cargo test --workspace
mise run clippy        # cargo clippy --workspace --all-targets -- -D warnings
mise run build         # cargo build --release
```

Run a single test: `cargo test <name_substring> -- --nocapture`

Rust toolchain is pinned to `1.96.1` (Rust edition 2024) via `.mise.toml`.

## Architecture

The project follows a Cargo Workspace layout under `crates/`:

```
clix/
  ├── Cargo.toml                    # Root workspace manifest
  ├── .mise.toml                    # Task runner & toolchain configuration
  ├── crates/
  │     ├── clix-core/              # Shared UI (anstyle/indicatif) & config (token/user detection)
  │     │     ├── src/lib.rs
  │     │     ├── src/ui.rs
  │     │     └── src/config.rs
  │     └── clix-gh-stars/          # GitHub stars exporter (lib + binary)
  │           ├── Cargo.toml
  │           └── src/
  │                 ├── lib.rs     # Exportable logic & StarsArgs
  │                 └── main.rs    # Standalone `clix-gh-stars` binary entrypoint
  └── src/
        └── main.rs                 # Unified `clix` CLI dispatcher
```

### Key Design Points

- **`clix-core` (Shared Infrastructure):** Contains shared terminal styling (`anstyle`), animated spinners (`indicatif`), and authentication/username detection helpers (`config::resolve_token`, `config::resolve_username`).
- **`clix-gh-stars` (Feature Crate):** Implements async pagination over GitHub's API via `reqwest` + `tokio`. Export formats (Markdown, URLs, JSON) are defined in `write_output`.
- **Root Binary (`clix`):** Uses `clap` to route subcommands (`clix gh stars`) directly to feature crate functions (`clix_gh_stars::run(args)`).

## Adding a New Tool

To add a new tool `clix-<service>-<name>` to the workspace:

1. Create a new crate in `crates/clix-<service>-<name>`:
   ```sh
   mkdir -p crates/clix-<service>-<name>/src
   ```
2. Add `"crates/clix-<service>-<name>"` to `[workspace.members]` in root `Cargo.toml`.
3. Reference `clix-core = { path = "../clix-core" }` in `crates/clix-<service>-<name>/Cargo.toml`.
4. Implement the tool in `crates/clix-<service>-<name>/src/lib.rs`, exposing a `pub struct Args` and `pub async fn run(args: Args) -> Result<()>`.
5. Add `crates/clix-<service>-<name>/src/main.rs` for standalone binary execution (`clix-<service>-<name>`).
6. Register the subcommand in root `src/main.rs` under `enum Commands` and the service-specific enum (`enum GhCommands`, `enum XCommands`, etc.).
