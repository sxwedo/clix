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

Built as a **Cargo Workspace**, every tool in `clix` is organized under service namespaces (`clix gh`, `clix x`, `clix yt`, etc.) and can be used both as subcommands under the unified `clix` binary or installed as a standalone CLI tool (`clix-gh-stars`, etc.).

---

## 🏛️ Workspace Architecture

```text
clix/
├── crates/
│   ├── clix-core/      # Shared UI styles, spinners, GitHub auth & config resolvers
│   └── clix-gh-stars/  # Standalone binary: `clix-gh-stars`
└── src/                # Unified entrypoint binary: `clix`
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
# Method 1: Via unified CLI (default output: <username>_starred_repos.md)
clix gh stars

# Method 2: Via standalone binary for a specific user (default output: octocat_starred_repos.md)
clix-gh-stars octocat

# Export a specific user's stars as plain URLs
clix-gh-stars octocat -f urls -o urls.txt

# Export as JSON
clix gh stars -f json -o stars.json
```

---

## 🚀 Building & Installation

```sh
# Build all binaries in release mode
git clone https://github.com/sxwedo/clix.git && cd clix
cargo build --release

# Binaries generated:
# - target/release/clix            (Unified CLI)
# - target/release/clix-gh-stars   (Standalone GitHub Stars CLI)
```

---

## 🗺️ Roadmap

- [x] `clix gh stars` — Fast GitHub starred repos batch exporter.

---

## 📄 License

[MIT](LICENSE) © sxwedo & contributors
