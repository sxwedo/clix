# AGENTS.md

This file provides guidance to Claude Code (claude.ai/code) and AI agent assistants when working with code in this repository.

## Overview

`clix` is a fast, local-first control plane for developer agents, with focused RSS, GitHub, social media, and content utilities.

It is structured as a **Cargo Workspace**:
- **Dual Invocation**: User-facing tools run through the unified CLI (`clix agent ps`, `clix agent inspect`, `clix gh stars`, `clix rss sync`, `clix rss list`, `clix x bookmarks`, `clix x read`, `clix wx read`) or a standalone binary.
- **Zero-Config GitHub Auth**: Falls back through the config file, `GITHUB_TOKEN`/`GH_TOKEN`, then `gh auth token`; usernames come from the config file, the authenticated `gh` account, or `github.user`.
- **Externalized Configuration**: Credentials and RSS subscriptions live in `~/.config/clix/config.toml` (`clix config init` generates a 0600 template). Credential resolution priority: CLI flags > config file > environment variables > GitHub autodetect.

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
  │   ├── clix-agent/         # Local agent process/session discovery, inspection, and control
  │   ├── clix-core/          # Shared UI, filesystem, config loading (settings.rs), and GitHub auth helpers
  │   ├── clix-gh-stars/      # GitHub stars exporter
  │   ├── clix-lark-base/     # Shared authenticated Lark Base schema and upsert interface
  │   ├── clix-media/         # Bounded concurrent media download and atomic persistence
  │   ├── clix-rss-api/       # Shared subscription selection, bounded fetching, and normalized models
  │   ├── clix-rss-delivery/  # Internal reliable delivery to configured destinations
  │   ├── clix-rss-list/      # Compact terminal view of stored RSS entries
  │   ├── clix-rss-store/     # Shared redb persistence, filtering, and archive ordering
  │   ├── clix-rss-sync/      # Local RSS sync followed by configured destination delivery
  │   ├── clix-wx-read/       # WeChat Official Account article reader
  │   ├── clix-x-api/         # Shared X auth, GraphQL parsing, and content/media types
  │   ├── clix-x-bookmarks/   # X bookmarks exporter (redb-backed incremental state)
  │   └── clix-x-read/        # Single X status/article reader
  └── src/main.rs             # Unified `clix` dispatcher
```

### Key Design Points

- **`clix-agent` (Local Agent Control):** Recognizes built-in (including Pi and Oh My Pi) and configured custom developer-agent processes, lists native provider sessions independently of live processes, associates live processes with persisted project/usage data, provides interactive top and resume pickers, reports only persisted token/cost data, revalidates process identity before termination, and resumes sessions with native argv without a shell.
- **`clix-core` (Shared Infrastructure):** Provides shared terminal UI, atomic filesystem writes, `settings.rs` for `~/.config/clix/config.toml` loading, and credential resolution that merges CLI flags, the config file, and GitHub autodetect.
- **`clix-media` (Shared Media Infrastructure):** Owns bounded concurrent downloads, per-request headers, 32 MiB response limits, local-file reuse, atomic persistence off the async executor, and best-effort failure reporting behind one batch interface.
- **`clix-rss-api` (Shared RSS Infrastructure):** Owns subscription selection, URL validation, bounded concurrent fetching, active-HTML sanitization, and normalized feed/entry models.
- **`clix-lark-base` (Shared Lark Infrastructure):** Owns tenant authentication, schema discovery, checkpoint-aware record lookup, paginated fallback reconciliation, sequential bounded batches, transient-write retries, and create/update/unchanged planning behind one typed upsert interface.
- **`clix-rss-store` (RSS Persistence):** Hides the redb table and serialization schema behind `open`, `open_or_create`, `upsert_feeds`, `query`, and delivery checkpoint updates. Each record's extensible `extra` envelope carries per-destination delivery state while RSS refreshes preserve it.
- **`clix-rss-delivery` (Internal RSS Delivery):** Maps one shared snapshot of canonical stored RSS fields into configured destinations sequentially after a local sync. Stable entry keys, payload hashes, target fingerprints, and redb checkpoints provide idempotent retry behavior; Lark Base is the first adapter. It intentionally exposes no public CLI binary.
- **`clix-rss-sync` (RSS Incremental State):** Upserts shared normalized entries into `~/.config/clix/rss.redb` by `source_url + entry.id`, then delivers `[rss].push_to` destinations after the local commit. New and changed records are written transactionally, unchanged records are skipped, and disappeared entries remain as history. `--state <path>` overrides the location.
- **`clix-rss-list` (RSS Read View):** Reads the shared store without network access and prints a compact terminal view.
- **`clix-x-api` (Shared X Infrastructure):** Owns X credentials (resolving CLI flags + config file + env vars), HTTP client setup, GraphQL parsing, media helpers, and the common content taxonomy.
- **`clix-x-bookmarks` (Incremental State):** Dedup state persists in a redb database (`state.rs`) at `~/.config/clix/bookmarks.redb` by default; legacy JSON sidecars auto-migrate. `--state <path>.redb` overrides the location.
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
