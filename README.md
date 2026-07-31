<p align="center">
  <img width="480" alt="clix logo" src="https://github.com/user-attachments/assets/f630492e-2af6-46af-9d18-7507dc26a902" />
</p>

<p align="center">
  <strong>A fast, local-first control plane for developer agents.</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/Rust-2024-orange?logo=rust&logoColor=white" alt="Rust 2024">
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey" alt="Platform">
  <img src="https://img.shields.io/badge/PRs-welcome-brightgreen.svg" alt="PRs welcome">
</p>

---

`clix` discovers, inspects, and controls developer agents running on your machine. Its local-first core is complemented by durable RSS sync, GitHub exports, and focused content-preservation tools.

It supports **Dual Invocation**: every tool is accessible via the unified dispatcher (`clix <service> <command>`) or as a standalone binary (`clix-<service>-<command>`).

---

## 🌟 Key Features

- 🤖 **Agent Control (`clix agent`)**: Discover Claude Code, Codex, Gemini CLI, OpenCode, Pi, Oh My Pi, Cursor Agent, and configured custom agents; inspect processes and saved sessions, tail logs, watch resources interactively, stop a process safely, or resume a saved session.
- 📰 **RSS Tools (`clix rss`)**: Incrementally sync RSS, Atom, or JSON Feed entries into redb, inspect the local archive, and reliably deliver new or changed entries into Lark Base.
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
│   ├── clix-agent/        # Local agent process discovery, session inspection, and control
│   ├── clix-gh-stars/     # GitHub starred-repository exporter
│   ├── clix-lark-base/    # Reusable authenticated Lark Base upsert interface
│   ├── clix-media/        # Bounded concurrent media download and atomic persistence
│   ├── clix-rss-api/      # Shared RSS selection, fetching, and normalization
│   ├── clix-rss-delivery/ # Internal reliable delivery to configured destinations
│   ├── clix-rss-list/     # Compact terminal view of stored RSS entries
│   ├── clix-rss-store/    # Shared redb persistence and archive querying
│   ├── clix-rss-sync/     # Fetch, local redb commit, and configured remote delivery
│   ├── clix-wx-read/      # WeChat Official Account article reader
│   ├── clix-x-api/        # Shared X auth, GraphQL parsing, and content/media types
│   ├── clix-x-bookmarks/  # X bookmarks exporter
│   └── clix-x-read/       # Single X status/article reader
└── src/                   # Unified `clix` dispatcher
```

---

## ✨ Available Tools

### 🤖 Agent Control (`clix agent`)

`clix agent` is a local process and session control layer. It does not require a daemon and does not upload process or conversation data. Built-in adapters recognize Claude Code, Codex, Gemini CLI, OpenCode, Pi, Oh My Pi (`omp`), and the Cursor Agent CLI.

```sh
# List running agents. The ID column is accepted by inspect, logs, and stop.
clix agent ps
clix agent ps --json

# Interactive CPU/memory view: ↑/↓ or j/k select, Enter/i shows details,
# r refreshes immediately, and q exits. Pipes/non-terminals render once.
clix agent top
clix agent top --interval 3 --iterations 5

# List every saved local session, including sessions with no running process.
# TARGET values can be passed directly to inspect, logs, or resume.
clix agent sessions
clix agent sessions --provider claude --limit 20
clix agent sessions --json

# Inspect a live process or an archived session.
clix agent inspect codex:12345
clix agent inspect codex:019abcde-session-id

# Show a compact, one-line event tail; opt into original JSONL records.
clix agent logs claude:session-id -n 100
clix agent logs claude:session-id -n 20 --raw

# SIGTERM after PID/start-time/type revalidation; --force uses a forceful kill.
clix agent stop claude:12345
clix agent stop claude:12345 --force

# Open an interactive saved-session picker, or select the newest session.
clix agent resume
clix agent resume --last
clix agent resume --list --limit 20

# Resume a known target through the provider's native CLI, without a shell.
clix agent resume codex:session-id
clix agent resume omp:session-id
clix agent resume cursor:chat-id
```

The process table exposes `ID`, `AGENT`, `PROJECT`, `STATUS`, `DURATION`, `TOKENS`, and `COST`; `top` adds CPU and memory. Status is derived from the operating system process state. clix associates a live process with its newest matching local session and uses the session project when a GUI host reports an unhelpful root working directory. A `-` means no session could be associated; `n/a` means a session was associated but the provider did not persist an exact cost. clix never estimates cost from public model prices.

Session metadata and logs are read from each provider's native local store. `sessions` indexes lightweight metadata without requiring a live process, so closed Claude Code, Codex, Pi, and Oh My Pi sessions remain discoverable and resumable. Token/cost usage is calculated only for associated or explicitly inspected sessions, and the interactive view caches usage until a session file changes. `logs` bounds individual record size and its retained tail, while `--raw` deliberately exposes the original selected records.

Custom agents use exact executable names plus optional argv markers. The resume command is an argv array with a required `{session}` placeholder, so it never passes configuration through a shell:

```toml
[agent.custom.my_agent]
executables = ["my-agent", "my-agent.exe"]
command_contains = ["--agent-mode"]
resume = ["my-agent", "resume", "{session}"]
```

Then use the same stable selectors:

```sh
clix agent inspect my_agent:12345
clix agent stop my_agent:12345
clix agent resume my_agent:session-id
```

Custom process discovery, stop, and resume are supported. Custom archived-session inspection and logs require a future session-store adapter and therefore fail with an explicit message rather than guessing a path.

---

### 📰 RSS Utilities (`clix rss`)

#### 🔄 `clix rss sync` / `clix-rss-sync` — Incremental redb Sync

`sync` is the only network-facing RSS command. Declare subscriptions in `~/.config/clix/config.toml`, then run one poll to fetch and persist normalized entries. The parser supports RSS 0.x, RSS 1.0, RSS 2.0, Atom, and JSON Feed.

```toml
[rss]
# Optional. Defaults to ~/.config/clix/rss.redb.
state = "/absolute/path/to/rss.redb"
limit = 20

[[rss.feeds]]
name = "Rust Blog"
url = "https://blog.rust-lang.org/feed.xml"

[[rss.feeds]]
name = "GitHub Blog"
url = "https://github.blog/feed/"
enabled = true
```

An entry is identified by its feed `source_url` plus feed-provided `entry.id`:

- a missing key is inserted;
- an existing key whose normalized content changed is updated while preserving `first_seen_at`;
- an unchanged key is not rewritten;
- entries that disappear from the latest feed response remain in the database as history.

The default database is `~/.config/clix/rss.redb` (or `$XDG_CONFIG_HOME/clix/rss.redb`). Override it with `[rss].state` or `--state`; CLI flags take precedence.

```sh
# Poll every enabled subscription once and sync the latest 20 entries per feed
clix rss sync

# Poll selected subscriptions with a larger lookback window
clix rss sync --feed "Rust Blog,GitHub Blog" --limit 100

# Use a separate database
clix rss sync --state ./feeds.redb

# Deliver only to explicitly selected destinations after the local sync
clix rss sync --push-to news,archive

# Sync locally without running configured remote deliveries
clix rss sync --no-push

# Equivalent standalone binary
clix-rss-sync --feed "Rust Blog"
```

When `[rss].push_to` is configured, `sync` first commits fetched entries to redb and then delivers every named destination sequentially from one local archive snapshot. All destinations are attempted. A remote failure leaves the local commit intact, records the failure checkpoint, and makes the command exit unsuccessfully so a scheduler can alert or retry it.

`sync` performs one poll and exits; it does not stay resident. Run it manually, from cron, launchd, systemd, or another scheduler. For example, this cron entry polls every day at 08:00:

```cron
0 8 * * * /absolute/path/to/clix rss sync >> /absolute/path/to/rss-sync.log 2>&1
```

Choose the polling interval and `--limit` together. If a source publishes more than `limit` entries between two polls, older unseen entries may no longer be present in the feed and cannot be recovered by the next sync. The database stores normalized feed metadata, the article URL, and the feed-provided summary/content excerpt—not the full linked article.

##### Lark Base delivery

Lark Base is the first supported destination. Delivery is an internal phase of `sync`; there is currently no separate public delivery command. Create the required fields in a Base table, grant the custom app access to that Base, and configure it:

```toml
[lark.accounts.default]
app_id = "cli_xxxxxxxxxxxxxxxx"
app_secret = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"

[lark.bases.rss_news]
account = "default"
app_token = "bascnxxxxxxxxxxxx"
table_id = "tblxxxxxxxxxxxx"

[rss]
state = "/absolute/path/to/rss.redb"
push_to = ["news"]

[rss.destinations.news]
type = "lark_base"
base = "rss_news"
key_field = "RSS Key"
hash_field = "Payload Hash"

[rss.destinations.news.fields]
title = "标题"
url = "原文链接"
subscription = "订阅源"
source_url = "Feed 地址"
entry_id = "Entry ID"
published_at = "发布时间"
authors = "作者"
categories = "分类"
summary = "摘要"
first_seen_at = "首次发现时间"
```

Use these Lark field types:

| Destination field | Lark Base type |
|---|---|
| `key_field`, `hash_field`, title, subscription, entry ID, summary | Text |
| entry URL, source URL, site URL | URL |
| publication, first-seen, and feed-update times | Date |
| authors and categories | Multi-select |

`key_field` must not contain duplicate values in existing remote records. The client uses the stable local key (`source_url + entry.id`) and the managed payload hash to choose create, update, or unchanged without making callers understand Lark API details.

```sh
# Sync locally, then deliver pending entries to [rss.destinations.news]
clix rss sync --push-to news
```

Mapped fields are owned by the projection; an absent optional RSS value clears its mapped remote field. Unmapped Base fields are left untouched, and records are never deleted remotely. Delivery metadata is stored in the same RSS record under `extra.deliveries.<destination>`—including payload hash, target fingerprint, remote record ID, attempts, timestamps, and the latest error. Confirmed record IDs are revalidated in bounded batches of 100 instead of scanning the whole table; missing IDs automatically fall back to key-based reconciliation. A failed record remains eligible for retry, and changing the destination mapping automatically makes its checkpoint stale.

Configuration and schema failures name the exact missing table, key, or Base field. For example:

```text
missing `[rss.destinations.news]` ... Add that table or remove `news` from `[rss].push_to`
missing `[lark.bases.rss_news]` ... referenced by `[rss.destinations.news].base`
`[lark.accounts.default].app_secret` must not be blank
Lark Base schema is incompatible: missing required text field `RSS Key` (type 1); ...
```

#### 📚 `clix rss list` / `clix-rss-list` — Local Archive View

Read the local redb database without requesting any RSS source and print a compact newest-first view:

```sh
# Show the newest 20 stored entries
clix rss list

# Filter by stored subscription name and change the global display limit
clix rss list --feed "Hacker News" --limit 50

# Read a non-default database
clix-rss-list --state ./feeds.redb
```

The heading reports how many entries are displayed, how many matched the filter before the limit, and how many are stored in the whole database.

The current RSS workflow is deliberately small:

```sh
clix rss sync
clix rss list
```

The former public `fetch`, `push`, and `export` commands are not currently supported. Their responsibilities can be reintroduced later without coupling them to the local store or the shared Lark client.

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

Custom Agent definitions, credentials, RSS subscriptions, and sync defaults are externalized to a central config file at `~/.config/clix/config.toml` (honors `XDG_CONFIG_HOME`). Bootstrap a commented template in one step:

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

# [agent.custom.my_agent]
# executables = ["my-agent"]
# command_contains = ["--agent-mode"]
# resume = ["my-agent", "resume", "{session}"]

# [lark.accounts.default]
# app_id = "cli_xxxxxxxxxxxxxxxx"
# app_secret = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"

# [lark.bases.rss_news]
# account = "default"
# app_token = "bascnxxxxxxxxxxxx"
# table_id = "tblxxxxxxxxxxxx"

[rss]
# Incremental sync database.
# state = "/absolute/path/to/rss.redb"
# Maximum entries retained from each feed.
# limit = 20
# Destinations automatically delivered after a successful local sync.
# push_to = ["news"]

# [[rss.feeds]]
# name = "Rust Blog"
# url = "https://blog.rust-lang.org/feed.xml"
# enabled = true

# [rss.destinations.news]
# type = "lark_base"
# base = "rss_news"
# key_field = "RSS Key"
# hash_field = "Payload Hash"
#
# [rss.destinations.news.fields]
# title = "标题"
# url = "原文链接"
# subscription = "订阅源"
# source_url = "Feed 地址"
# entry_id = "Entry ID"
# published_at = "发布时间"
# authors = "作者"
# categories = "分类"
# summary = "摘要"
# first_seen_at = "首次发现时间"
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

Incremental state is stored in [redb](https://github.com/cberner/redb) key-value databases with transactional writes. RSS delivery checkpoints live in the extensible `extra` object of each existing entry rather than a second table. RSS `list` opens the existing database read-only and never creates a missing archive silently.

| Option | Default | Description |
|--------|---------|-------------|
| `clix rss sync --state <path>` | `~/.config/clix/rss.redb` | Normalized RSS entry history |
| `clix x bookmarks --state <path>` | `~/.config/clix/bookmarks.redb` | Bookmark dedup state |

> **Bookmark migration:** Existing `<output>.state.json` files are detected and imported into the bookmark redb store automatically on the first run; the original JSON is renamed to `<output>.state.json.bak`. No manual steps required.
---

## 🚀 Building & Installation

```sh
# Clone and build all workspace binaries in release mode
git clone https://github.com/sxwedo/clix.git && cd clix
cargo build --workspace --release

# Binaries generated in target/release/:
# - clix               (Unified CLI dispatcher)
# - clix-agent         (Standalone local developer-agent control)
# - clix-rss-list      (Standalone local RSS archive list)
# - clix-rss-sync      (Standalone RSS redb sync and configured delivery)
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
