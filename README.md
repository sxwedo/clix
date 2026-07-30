<p align="center">
  <img width="480" alt="clix logo" src="https://github.com/user-attachments/assets/f630492e-2af6-46af-9d18-7507dc26a902" />
</p>

<p align="center">
  <strong>Blazing-Fast Developer Superpower CLI Suite written in Rust.</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/Rust-2024-orange?logo=rust&logoColor=white" alt="Rust 2024">
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey" alt="Platform">
  <img src="https://img.shields.io/badge/PRs-welcome-brightgreen.svg" alt="PRs welcome">
</p>

---

`clix` is a modular, high-performance CLI suite written in Rust designed to empower daily developer workflows, RSS subscription snapshots, social media content preservation, GitHub exports, and WeChat article downloading.

It supports **Dual Invocation**: every tool is accessible via the unified dispatcher (`clix <service> <command>`) or as a standalone binary (`clix-<service>-<command>`).

---

## 🌟 Key Features

- 📰 **RSS Subscription Fetcher (`clix rss fetch`)**: Configure RSS, Atom, or JSON Feed subscriptions once and fetch their latest entries into one Markdown or JSON snapshot.
- 🟢 **WeChat Article Reader (`clix wx read`)**: Convert WeChat Official Account articles into clean Markdown/MDX files. Downloads images locally, bypasses hotlink protection (`403 Forbidden`), and intelligently cleans up WeChat code blocks and pseudo-headings.
- 🐦 **X (Twitter) Bookmarks & Reader (`clix x`)**: Incremental sync of X bookmarks to Markdown/JSON, and download single X posts/articles with local media assets.
- 🐙 **Zero-Config GitHub Star Exporter (`clix gh stars`)**: Asynchronously export starred repositories with auto-detected `gh` auth credentials or a central config file.
- 🚀 **Bounded & Efficient**: Powered by Rust 2024 and Tokio async I/O, with explicit concurrency and response-size limits on network-heavy paths.

---

## 🏛️ Workspace Architecture

```text
clix/
├── crates/
│   ├── clix-core/         # Shared UI, filesystem, config loading, and GitHub auth helpers
│   ├── clix-gh-stars/     # GitHub starred-repository exporter
│   ├── clix-rss-fetch/    # Config-driven RSS/Atom/JSON Feed snapshot exporter
│   ├── clix-wx-read/      # WeChat Official Account article reader
│   ├── clix-x-api/        # Shared X auth, GraphQL parsing, and content/media types
│   ├── clix-x-bookmarks/  # X bookmarks exporter
│   └── clix-x-read/       # Single X status/article reader
└── src/                   # Unified `clix` dispatcher
```

---

## ✨ Available Tools

### 📰 RSS Utilities (`clix rss`)

#### 🔄 `clix rss fetch` / `clix-rss-fetch` — Subscription Fetcher

Declare subscriptions in `~/.config/clix/config.toml`, then fetch their latest entries into a single Markdown or JSON snapshot. The parser supports RSS 0.x, RSS 1.0, RSS 2.0, Atom, and JSON Feed.

- ⚙️ **Config-driven subscriptions:** Each `[[rss.feeds]]` block has a stable name, URL, and optional `enabled` flag.
- 🔎 **Selective fetches:** `--feed` can be repeated or receive comma-separated names.
- 🧭 **Normalized output:** Feed title, source, entry link, publication time, author, categories, and summary use one schema across feed formats.
- ⚡ **Concurrent and resilient:** Feeds are fetched concurrently; one failed source is reported without discarding successful sources.
- 🛡️ **Safe summaries:** Active HTML is sanitized before conversion to Markdown.
- 💾 **Safe snapshots:** Output directories are created as needed and snapshots are atomically replaced.

Configure subscriptions:

```toml
[rss]
output = "rss.md"
limit = 20

[[rss.feeds]]
name = "Rust Blog"
url = "https://blog.rust-lang.org/feed.xml"

[[rss.feeds]]
name = "GitHub Blog"
url = "https://github.blog/feed/"
enabled = true
```

Fetch them:

```sh
# Fetch every enabled subscription using the configured output and limit
clix rss fetch

# Fetch one or more named subscriptions
clix rss fetch --feed "Rust Blog"
clix rss fetch --feed "Rust Blog,GitHub Blog"

# CLI values override fetch defaults; .json output also infers JSON format
clix rss fetch --limit 10 --format json -o latest-rss.json

# Equivalent standalone binary
clix-rss-fetch --feed "Rust Blog" -o rust-news.md
```

`[rss].output` is resolved from the current working directory when it is relative. `--format` takes precedence over the output extension; without `--format`, a `.json` output selects JSON and every other extension selects Markdown.

Each entry contains normalized metadata, its article link, and a summary of at most 1,200 characters. The feed-provided `summary` is preferred; when it is absent, the feed-provided content body is used as the summary. This command does not crawl the linked website for the full article.

---

### 🟢 WeChat Utilities (`clix wx`)

#### 📖 `clix wx read` / `clix-wx-read` — WeChat Article Exporter

Download any WeChat Official Account article by URL or ID and convert it into a standalone Markdown, MDX, or JSON file.

- 🖼️ **Anti-Hotlinking Local Image Downloads**: Downloads all embedded images concurrently into a `./media/` directory and updates Markdown image paths to relative local links.
- 🧠 **Smart Code Block & Heading Restoration**: Cleans up WeChat's custom code snippets (`<pre class="code-snippet">`, `<ul class="code-snippet__list">`), restores code line breaks, and intelligently elevates WeChat pseudo-headings into clean ATX Markdown headings (`##`, `###`).
- 📑 **Multiple Output Formats**: Export as `.md`, `.mdx`, or `.json`.
- ⚡ **Zero-Config Public Scraping**: Works out-of-the-box for public WeChat articles without needing scans, logins, or tokens.

##### Usage

```sh
# Export a WeChat article to Markdown (with local images) via unified CLI
clix wx read "https://mp.weixin.qq.com/s/abcdef123456"

# Via standalone binary with custom output path
clix-wx-read "https://mp.weixin.qq.com/s/abcdef123456" -o ./my_article.md

# Export as MDX format without downloading images
clix wx read "https://mp.weixin.qq.com/s/abcdef123456" --format mdx --no-media
```

---

### 🐙 GitHub Utilities (`clix gh`)

#### 🌟 `clix gh stars` / `clix-gh-stars` — GitHub Starred Repositories Exporter

Batch export all starred repositories of any GitHub user into Markdown tables, plain URL lists, or JSON files.

- 🔑 **Flexible Auth:** `gh auth token` autodetect, explicit flags, a central config file, or `GITHUB_TOKEN`/`GH_TOKEN` env vars.
- 👤 **User Detection:** Uses the authenticated `gh` account, an explicit `github.user`, or `git config`.
- 🚀 **Paginated Export:** Fetches GitHub API pages asynchronously with a live progress spinner.

##### Usage

```sh
# Export logged-in user's starred repos via unified CLI
clix gh stars

# Standalone binary for a specific user as URL list
clix-gh-stars octocat -f urls -o urls.txt
```

---

### 🐦 X (Twitter) Utilities (`clix x`)

#### 🔖 `clix x bookmarks` / `clix-x-bookmarks` — X Bookmarked Tweets Exporter

Export your bookmarked tweets from X (Twitter) via GraphQL into Markdown, plain URL lists, or JSON files.

- 🔑 **Flexible Auth:** Pass `--auth-token`/`--ct0`, declare them in `~/.config/clix/config.toml`, or set `X_AUTH_TOKEN`/`X_CT0` env vars.
- 🗄️ **Durable State:** Incremental dedup state persists in a redb database at `~/.config/clix/bookmarks.redb` (O(1) upserts, crash-safe); legacy `<output>.state.json` files auto-migrate on first run.
- 🔄 **Incremental Sync:** Appends newly seen bookmarks and stops after reaching previously exported IDs (`--incremental`).

##### Usage

```sh
# One-time: create and edit the config with your X credentials
clix config init
# then edit ~/.config/clix/config.toml → [x] auth_token / ct0

# Incremental export to Markdown (state auto-tracked in bookmarks.redb)
clix x bookmarks --incremental -o my_bookmarks.md

# Or pass credentials inline per run
clix x bookmarks --auth-token "<token>" --ct0 "<ct0>" --incremental
```

#### 📖 `clix x read` / `clix-x-read` — X Status and Article Reader

Download one X status or Article by URL or Tweet ID and convert it to Markdown, MDX, or JSON with local media files.

##### Usage

```sh
clix x read https://x.com/user/status/123456789
```

---

## ⚙️ Configuration

Credentials, RSS subscriptions, and fetch defaults are externalized to a central config file at `~/.config/clix/config.toml` (honors `XDG_CONFIG_HOME`). Bootstrap a commented template in one step:

```sh
clix config init   # creates ~/.config/clix/config.toml (mode 0600)
```

The generated file is self-documenting:

```toml
[github]
# GitHub personal access token. Leave unset to fall back to `gh auth token`.
# token = "ghp_xxxxxxxxxxxxxxxxxxxx"
# GitHub username. Leave unset to fall back to `gh api user` / git config.
# username = "your-name"

[x]
# X (Twitter) login cookie `auth_token`.
# auth_token = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
# X (Twitter) CSRF cookie `ct0`.
# ct0 = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"

[rss]
# Default output path. A .json extension selects JSON.
# output = "rss.md"
# Maximum entries retained from each feed.
# limit = 20

# [[rss.feeds]]
# name = "Rust Blog"
# url = "https://blog.rust-lang.org/feed.xml"
# enabled = true
```

### Credential Resolution Priority

Each credential is resolved with the following priority (highest first):

| Priority | Source | Example |
|:--------:|--------|---------|
| 1 | **CLI flags** | `--token`, `--auth-token`, `--ct0` |
| 2 | **Config file** | `~/.config/clix/config.toml` |
| 3 | **Environment vars** | `GITHUB_TOKEN`, `X_AUTH_TOKEN`, ... |
| 4 | **GitHub autodetect** | `gh auth token` / `gh api user` |

> GitHub autodetect keeps the zero-config experience intact: leave credentials unset and clix falls back to your local `gh` CLI.

### Persistent State

Incremental bookmark dedup state is stored in a [redb](https://github.com/cberner/redb) key-value database at `~/.config/clix/bookmarks.redb` — crash-safe, with O(1) per-bookmark upserts instead of rewriting a whole JSON sidecar on every sync.

| Option | Default | Description |
|--------|---------|-------------|
| `--state <path>` | `~/.config/clix/bookmarks.redb` | Location of the redb state database |

> **Migration:** Existing `<output>.state.json` files are detected and imported into the redb store automatically on the first run; the original JSON is renamed to `<output>.state.json.bak`. No manual steps required.
---

## 🚀 Building & Installation

```sh
# Clone and build all workspace binaries in release mode
git clone https://github.com/sxwedo/clix.git && cd clix
cargo build --workspace --release

# Binaries generated in target/release/:
# - clix               (Unified CLI dispatcher)
# - clix-rss-fetch     (Standalone RSS subscription fetcher)
# - clix-wx-read       (Standalone WeChat Article CLI)
# - clix-gh-stars      (Standalone GitHub Stars CLI)
# - clix-x-bookmarks   (Standalone X Bookmarks CLI)
# - clix-x-read        (Standalone X Status/Article CLI)
```

---

## 🧪 Verification & Development

Run all formatting, type-checking, unit tests, strict Clippy (`warnings`, `pedantic`, `nursery`), and documentation gates:

```sh
mise run verify
```

---

## 📄 License

[MIT](LICENSE) © sxwedo & contributors
