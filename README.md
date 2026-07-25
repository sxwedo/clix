<p align="center">
  <em>⚡ CLI Extensions for Daily Developer Superpowers.</em>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/Rust-2024-orange?logo=rust&logoColor=white" alt="Rust 2024">
  <img src="https://img.shields.io/badge/PRs-welcome-brightgreen.svg" alt="PRs welcome">
</p>

---

`clix` is a fast, modular, multi-binary CLI toolset written in Rust designed to streamline daily developer tasks, GitHub workflows, social media integrations, and system utilities.

Built as a **Cargo Workspace**, every tool in `clix` is organized under service namespaces (`clix gh`, `clix x`, `clix yt`, etc.) and can be used both as subcommands under the unified `clix` binary or installed as a standalone CLI tool (`clix-gh-stars`, `clix-x-bookmarks`, etc.).

---

## 🏛️ Workspace Architecture

```text
clix/
├── crates/
│   ├── clix-core/         # Shared UI styles, spinners, GitHub auth & config resolvers
│   ├── clix-gh-stars/     # Standalone binary: `clix-gh-stars`
│   └── clix-x-bookmarks/  # Standalone binary: `clix-x-bookmarks`
└── src/                   # Unified entrypoint binary: `clix`
```

---

## ✨ Available Tools

### 🐙 GitHub Utilities (`clix gh`)

#### 🌟 `clix gh stars` / `clix-gh-stars` — GitHub Starred Repositories Exporter
Batch export all starred repositories of any GitHub user into Markdown tables, plain URL lists, or JSON files.

- 🔑 **Zero-Config Auth:** Auto-detects tokens from `gh` CLI (`gh auth token`) or `GITHUB_TOKEN` env var.
- 👤 **Smart User Detection:** Auto-detects your username via `gh` or `git config`.
- 🚀 **Blazing Fast & Streaming:** Async pagination powered by Tokio & Reqwest with a live progress spinner.

##### Usage:
```sh
# Method 1: Via unified CLI
clix gh stars

# Method 2: Via standalone binary for a specific user
clix-gh-stars octocat -f urls -o urls.txt
```

---

### 🐦 X (Twitter) Utilities (`clix x`)

#### 🔖 `clix x bookmarks` / `clix-x-bookmarks` — X Bookmarked Tweets Exporter
Export your bookmarked tweets from X (Twitter) via GraphQL into Markdown, plain URL lists, or JSON files using `auth_token` and `ct0` cookies.

- 🔑 **Simple Auth:** Pass `--auth-token` and `--ct0` or set `X_AUTH_TOKEN` and `X_CT0` env vars.
- 📑 **Multiple Formats:** Supports Markdown tables, plain URLs, and JSON export.

##### Usage:
```sh
# Export X bookmarks using env vars
export X_AUTH_TOKEN="your_auth_token"
export X_CT0="your_ct0"
clix x bookmarks

# Export using CLI flags & limit to 50 items
clix-x-bookmarks --auth-token "..." --ct0 "..." -n 50 -f markdown -o my_x_bookmarks.md
```

---

## 🚀 Building & Installation

```sh
# Build all binaries in release mode
git clone https://github.com/sxwedo/clix.git && cd clix
cargo build --release

# Binaries generated:
# - target/release/clix               (Unified CLI)
# - target/release/clix-gh-stars      (Standalone GitHub Stars CLI)
# - target/release/clix-x-bookmarks   (Standalone X Bookmarks CLI)
```

---

## 📄 License

[MIT](LICENSE) © sxwedo & contributors
