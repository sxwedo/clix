<p align="center">
  <em>⚡ CLI Extensions for Daily Developer Superpowers.</em>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/Rust-2024-orange?logo=rust&logoColor=white" alt="Rust 2024">
  <img src="https://img.shields.io/badge/PRs-welcome-brightgreen.svg" alt="PRs welcome">
</p>

---

`clix` is a modular, multi-binary CLI toolset written in Rust for GitHub workflows, X exports, and local Markdown viewing.

The unified `clix` binary exposes `clix gh`, `clix x`, and `clix view`. Each tool also has a standalone binary.

---

## 🏛️ Workspace Architecture

```text
clix/
├── crates/
│   ├── clix-core/         # Shared UI, filesystem, and GitHub config helpers
│   ├── clix-gh-stars/     # GitHub starred-repository exporter
│   ├── clix-view/         # Terminal Markdown/MDX viewer
│   ├── clix-x-api/        # Shared X auth, GraphQL parsing, and content/media types
│   ├── clix-x-bookmarks/  # X bookmarks exporter
│   └── clix-x-read/       # Single X status/article reader
└── src/                   # Unified `clix` entrypoint
```

---

## ✨ Available Tools

### 🐙 GitHub Utilities (`clix gh`)

#### 🌟 `clix gh stars` / `clix-gh-stars` — GitHub Starred Repositories Exporter

Batch export all starred repositories of any GitHub user into Markdown tables, plain URL lists, or JSON files.

- 🔑 **Zero-Config Auth:** Detects `GITHUB_TOKEN`, `GH_TOKEN`, or the token from `gh auth token`.
- 👤 **User Detection:** Uses the authenticated `gh` account or an explicit
  `github.user`; it never guesses an identity from a repository owner or display name.
- 🚀 **Paginated Export:** Fetches GitHub API pages asynchronously with a live progress spinner.

##### Usage

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
- 🧭 **Structured Types:** Exports one normalized primary type plus non-exclusive relationship, media, and poll subtypes.
- 📊 **Engagement Metrics:** Includes available bookmark, like, reply, view, repost, and quote counts.
- 🔄 **Incremental Sync:** Appends newly seen bookmarks and stops after reaching a full page of previously exported tweet IDs.
- 📰 **Plain Tweet Text:** Keeps `Tweet` free of URLs. Status, Article, media, and preview links appear only in `Media / Links`; fetched Article titles become the Article link labels. `--link-only` skips the extra title requests and uses a generic link label.

The exported taxonomy is a `clix` normalization of X GraphQL response shapes, not an
official enum published by X. Primary types are `article`, `note_tweet`, and `post`.
Non-exclusive subtypes describe references, seed-post attachments, and polls:
`retweeted`, `quoted`, `replied_to`, `photo`, `video`, `animated_gif`, and `poll`.
Images embedded inside an Article body are Article content, not seed-post attachment
subtypes.

##### Usage

```sh
# Export X bookmarks using env vars
export X_AUTH_TOKEN="your_auth_token"
export X_CT0="your_ct0"
clix x bookmarks

# Export using CLI flags & limit to 50 items
clix-x-bookmarks --auth-token "..." --ct0 "..." -n 50 -f markdown -o my_x_bookmarks.md

# Daily incremental sync; the first run can bootstrap from an existing export
clix x bookmarks --incremental -o my_x_bookmarks.md

# Faster export: keep Article links in Media / Links without requesting titles
clix x bookmarks --incremental --link-only -o my_x_bookmarks.md
```

Incremental state is stored beside the output as `<output>.state.json` by default
(for example, `my_x_bookmarks.state.json`). Use `--state <path>` to override it.
Because X does not expose when a bookmark was saved, incremental sync identifies
new entries by tweet ID and bookmark timeline order, not by the tweet's publication time.

#### 📖 `clix x read` / `clix-x-read` — X Status and Article Reader

Download one X status or Article by URL or Tweet ID and convert it to Markdown, MDX, or JSON. Attached images are saved locally unless `--no-media` is supplied.

Markdown and MDX frontmatter expose the same normalized `content_type` and
`subtypes` fields as the bookmarks exporter. The historical single-value `type`
field remains temporarily available for existing consumers.

##### Usage

```sh
# Read a status through the unified CLI
clix x read https://x.com/user/status/123456789

# Produce JSON without downloading media
clix-x-read 123456789 --format json --no-media -o status.json
```

Authentication uses the same `X_AUTH_TOKEN` and `X_CT0` environment variables as the bookmarks exporter.

---

### 🖼️ Markdown Viewer (`clix view`)

Render a local Markdown or MDX file in the terminal, including supported local images.

```sh
clix view article.md
clix-view article.mdx
```

---

## 🚀 Building & Installation

```sh
# Build all binaries in release mode
git clone https://github.com/sxwedo/clix.git && cd clix
cargo build --workspace --release

# Binaries generated:
# - target/release/clix               (Unified CLI)
# - target/release/clix-gh-stars      (Standalone GitHub Stars CLI)
# - target/release/clix-view          (Standalone Markdown/MDX viewer)
# - target/release/clix-x-bookmarks   (Standalone X Bookmarks CLI)
# - target/release/clix-x-read        (Standalone X status/article reader)
```

---

## 🧪 Development

The local verification command runs formatting, workspace type-checking, all tests,
strict Clippy (`warnings`, `pedantic`, and `nursery` are denied), and rustdoc with
warnings denied:

```sh
mise run verify
```

The same gates plus a release workspace build run in CI.

---

## 📄 License

[MIT](LICENSE) © sxwedo & contributors
