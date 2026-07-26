use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    time::SystemTime,
};

use chrono::{DateTime, Utc};
use clap::{Args, ValueEnum};
use clix_x_api::{ContentSubtype, ContentType};
use serde::{Deserialize, Serialize};

pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Markdown,
    Urls,
    Json,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TweetMetrics {
    pub bookmarks: Option<u64>,
    pub likes: Option<u64>,
    pub replies: Option<u64>,
    pub views: Option<u64>,
    pub reposts: Option<u64>,
    pub quotes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkPreview {
    pub title: Option<String>,
    pub url: String,
    pub expanded_url: Option<String>,
}

#[derive(Debug, Args)]
pub struct BookmarksArgs {
    /// X `auth_token` cookie (or set `X_AUTH_TOKEN` / `TWITTER_AUTH_TOKEN`)
    #[arg(long)]
    pub auth_token: Option<String>,

    /// X `ct0` (CSRF) cookie (or set `X_CT0` / `TWITTER_CT0`)
    #[arg(long)]
    pub ct0: Option<String>,

    /// Output file path (default: `x_bookmarks.md`)
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Output format: markdown, urls, or json
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Markdown)]
    pub format: OutputFormat,

    /// Maximum number of bookmarks to fetch (default: all)
    #[arg(short = 'n', long, conflicts_with = "incremental")]
    pub count: Option<usize>,

    /// Download attached media (images/photos) locally into a `media/` folder
    #[arg(long)]
    pub download_media: bool,

    /// Append only bookmarks not present in the previous successful export
    #[arg(long)]
    pub incremental: bool,

    /// Incremental state file (default: `<output>.state.json`)
    #[arg(long)]
    pub state: Option<PathBuf>,

    /// Skip X Article titles; keep the article link in Media / Links only
    #[arg(long)]
    pub link_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TweetBookmark {
    pub id: String,
    pub content_type: ContentType,
    #[serde(default)]
    pub subtypes: Vec<ContentSubtype>,
    pub author_name: String,
    pub author_handle: String,
    pub text: String,
    pub created_at: String,
    pub url: String,
    #[serde(
        default,
        alias = "link_preview",
        deserialize_with = "deserialize_link_previews",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub links: Vec<LinkPreview>,
    #[serde(default)]
    pub metrics: TweetMetrics,
    #[serde(default)]
    pub media: Vec<String>,
    #[serde(default)]
    pub local_media: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SerializedLinkPreviews {
    One(LinkPreview),
    Many(Vec<LinkPreview>),
}

fn deserialize_link_previews<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<LinkPreview>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(
        match Option::<SerializedLinkPreviews>::deserialize(deserializer)? {
            None => Vec::new(),
            Some(SerializedLinkPreviews::One(preview)) => vec![preview],
            Some(SerializedLinkPreviews::Many(previews)) => previews,
        },
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookmarkState {
    pub(super) version: u32,
    pub(super) last_successful_sync: String,
    pub(super) seen_tweet_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) article_titles: BTreeMap<String, String>,
}

impl BookmarkState {
    pub(super) fn new(
        seen_tweet_ids: HashSet<String>,
        article_titles: BTreeMap<String, String>,
    ) -> Self {
        let now: DateTime<Utc> = SystemTime::now().into();
        let mut seen_tweet_ids = seen_tweet_ids.into_iter().collect::<Vec<_>>();
        seen_tweet_ids.sort_unstable();
        Self {
            version: STATE_VERSION,
            last_successful_sync: now.to_rfc3339(),
            seen_tweet_ids,
            article_titles,
        }
    }
}
