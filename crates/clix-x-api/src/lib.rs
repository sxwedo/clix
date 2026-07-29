use std::{env, fmt, path::Path, time::Duration};

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod content;
mod tweet;

pub use content::{extract_author, extract_media_urls};
pub use tweet::{
    TweetDetail, fetch_article_title, fetch_tweet_detail, fetch_tweet_detail_with_options,
};

const X_BEARER_TOKEN: &str = "Bearer AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs%3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA";
const X_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// Authentication cookies required by X's web GraphQL API.
#[derive(Clone)]
pub struct XCredentials {
    auth_token: String,
    ct0: String,
}

impl fmt::Debug for XCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("XCredentials")
            .field("auth_token", &"[REDACTED]")
            .field("ct0", &"[REDACTED]")
            .finish()
    }
}

impl XCredentials {
    /// Resolve credentials from explicit values and supported environment variables.
    #[must_use]
    pub fn resolve(auth_token: Option<String>, ct0: Option<String>) -> Option<Self> {
        let auth_token = first_nonblank(
            std::iter::once(auth_token)
                .chain(["X_AUTH_TOKEN", "TWITTER_AUTH_TOKEN", "AUTH_TOKEN"].map(env_value)),
        )?;
        let ct0 = first_nonblank(
            std::iter::once(ct0).chain(["X_CT0", "TWITTER_CT0", "CT0"].map(env_value)),
        )?;
        Some(Self { auth_token, ct0 })
    }

    /// Build an authenticated HTTP client with bounded connection and request timeouts.
    ///
    /// # Errors
    ///
    /// Returns an error when a credential cannot be represented as an HTTP
    /// header or the underlying client cannot be constructed.
    pub fn build_client(&self) -> Result<reqwest::Client> {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static(X_BEARER_TOKEN));
        headers.insert(
            "cookie",
            HeaderValue::from_str(&format!("auth_token={}; ct0={}", self.auth_token, self.ct0))
                .context("invalid X cookie header")?,
        );
        headers.insert(
            "x-csrf-token",
            HeaderValue::from_str(&self.ct0).context("invalid X CSRF token header")?,
        );
        headers.insert("x-twitter-active-user", HeaderValue::from_static("yes"));
        headers.insert("x-twitter-client-language", HeaderValue::from_static("en"));

        client_builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to build HTTP client for X")
    }
}

/// Build an unauthenticated client for media URLs returned by X.
///
/// Keeping this client separate prevents X cookies and CSRF credentials from
/// being forwarded to CDN or externally hosted media.
///
/// # Errors
///
/// Returns an error when the underlying HTTP client cannot be constructed.
pub fn build_media_client() -> Result<reqwest::Client> {
    client_builder()
        .build()
        .context("failed to build HTTP client for X media")
}

/// Read and summarize an HTTP error response without buffering an unbounded body.
pub async fn response_error_excerpt(mut response: reqwest::Response, max_chars: usize) -> String {
    let byte_limit = max_chars.saturating_mul(4).saturating_add(1);
    let mut bytes = Vec::with_capacity(byte_limit.min(4_096));
    let mut was_truncated = false;

    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let remaining = byte_limit.saturating_sub(bytes.len());
                if chunk.len() > remaining {
                    bytes.extend_from_slice(&chunk[..remaining]);
                    was_truncated = true;
                    break;
                }
                bytes.extend_from_slice(&chunk);
                if bytes.len() == byte_limit {
                    was_truncated = true;
                    break;
                }
            }
            Ok(None) => break,
            Err(error) if bytes.is_empty() => {
                return format!("<failed to read X error response: {error}>");
            }
            Err(_) => {
                was_truncated = true;
                break;
            }
        }
    }

    let body = String::from_utf8_lossy(&bytes);
    let mut excerpt = error_excerpt(&body, max_chars);
    if was_truncated && !excerpt.ends_with('…') {
        excerpt.push('…');
    }
    excerpt
}

/// Truncate a diagnostic string at a Unicode character boundary.
#[must_use]
pub fn error_excerpt(value: &str, max_chars: usize) -> String {
    let mut characters = value.chars();
    let summary = characters.by_ref().take(max_chars).collect::<String>();
    if characters.next().is_some() {
        format!("{summary}…")
    } else {
        summary
    }
}

fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(X_USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
}

fn env_value(name: &str) -> Option<String> {
    env::var(name).ok()
}

fn first_nonblank(values: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    values
        .into_iter()
        .flatten()
        .find_map(|value| nonblank(&value).map(ToOwned::to_owned))
}

fn nonblank(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

/// Derive a safe local extension from an X media URL.
#[must_use]
pub fn media_extension(media_url: &str) -> &'static str {
    let Ok(url) = reqwest::Url::parse(media_url) else {
        return "jpg";
    };

    if let Some(extension) = url
        .query_pairs()
        .find_map(|(key, value)| (key == "format").then_some(value))
        .and_then(|value| supported_media_extension(&value))
    {
        return extension;
    }

    Path::new(url.path())
        .extension()
        .and_then(|extension| extension.to_str())
        .and_then(supported_media_extension)
        .unwrap_or("jpg")
}

/// Percent-encode bytes that are unsafe inside a Markdown link destination.
#[must_use]
pub fn markdown_destination(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b':'
                    | b'/'
                    | b'?'
                    | b'#'
                    | b'['
                    | b']'
                    | b'@'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b'%'
            )
        {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

const fn supported_media_extension(value: &str) -> Option<&'static str> {
    if value.eq_ignore_ascii_case("jpg") || value.eq_ignore_ascii_case("jpeg") {
        Some("jpg")
    } else if value.eq_ignore_ascii_case("png") {
        Some("png")
    } else if value.eq_ignore_ascii_case("webp") {
        Some("webp")
    } else if value.eq_ignore_ascii_case("gif") {
        Some("gif")
    } else if value.eq_ignore_ascii_case("mp4") {
        Some("mp4")
    } else {
        None
    }
}

/// The primary shape normalized by clix from X post fields.
///
/// This is a clix taxonomy, not an enum published by X.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    Article,
    NoteTweet,
    Post,
}

impl fmt::Display for ContentType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Article => "article",
            Self::NoteTweet => "note_tweet",
            Self::Post => "post",
        })
    }
}

/// Non-exclusive relationship, seed-post attachment, and poll traits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentSubtype {
    Retweeted,
    Quoted,
    RepliedTo,
    Photo,
    Video,
    AnimatedGif,
    Poll,
}

impl fmt::Display for ContentSubtype {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Retweeted => "retweeted",
            Self::Quoted => "quoted",
            Self::RepliedTo => "replied_to",
            Self::Photo => "photo",
            Self::Video => "video",
            Self::AnimatedGif => "animated_gif",
            Self::Poll => "poll",
        })
    }
}

/// Canonical clix classification shared by all X commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostClassification {
    pub content_type: ContentType,
    #[serde(default)]
    pub subtypes: Vec<ContentSubtype>,
}

impl PostClassification {
    /// Map the canonical classification to the historical single-value reader type.
    #[must_use]
    pub fn legacy_type(&self) -> LegacyTweetType {
        match self.content_type {
            ContentType::Article => LegacyTweetType::Article,
            ContentType::NoteTweet => LegacyTweetType::NoteTweet,
            ContentType::Post
                if self.subtypes.iter().any(|subtype| {
                    matches!(
                        subtype,
                        ContentSubtype::Photo | ContentSubtype::Video | ContentSubtype::AnimatedGif
                    )
                }) =>
            {
                LegacyTweetType::Media
            }
            ContentType::Post => LegacyTweetType::Tweet,
        }
    }
}

/// Return whether a URL points to an X Article.
#[must_use]
pub fn is_x_article_url(value: &str) -> bool {
    reqwest::Url::parse(value).ok().is_some_and(|url| {
        matches!(
            url.host_str(),
            Some("x.com" | "www.x.com" | "twitter.com" | "www.twitter.com")
        ) && url.path().starts_with("/i/article/")
    })
}

/// Classify an unwrapped GraphQL Tweet result using the canonical type system.
#[must_use]
pub fn classify_post(target: &Value) -> PostClassification {
    let has_article_object = target
        .get("article")
        .is_some_and(|article| !article.is_null());
    let has_note_tweet = target
        .get("note_tweet")
        .is_some_and(|note_tweet| !note_tweet.is_null());
    let content_type = if has_article_object {
        ContentType::Article
    } else if has_note_tweet {
        ContentType::NoteTweet
    } else if has_x_article_seed_link(target) {
        ContentType::Article
    } else {
        ContentType::Post
    };

    let legacy = target.get("legacy");
    let mut subtypes = Vec::new();
    let is_retweet = target
        .get("retweeted_status_result")
        .or_else(|| legacy.and_then(|value| value.get("retweeted_status_result")))
        .is_some_and(|value| !value.is_null())
        || legacy
            .and_then(|value| value.get("full_text"))
            .and_then(Value::as_str)
            .is_some_and(|text| text.starts_with("RT @"));
    push_if(&mut subtypes, is_retweet, ContentSubtype::Retweeted);

    let is_quote = target
        .get("quoted_status_result")
        .is_some_and(|value| !value.is_null())
        || legacy
            .and_then(|value| value.get("is_quote_status"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    push_if(&mut subtypes, is_quote, ContentSubtype::Quoted);

    let is_reply = legacy
        .and_then(|value| value.get("in_reply_to_status_id_str"))
        .is_some_and(|value| !value.is_null());
    push_if(&mut subtypes, is_reply, ContentSubtype::RepliedTo);

    collect_media_subtypes(target, &mut subtypes);

    let has_poll = target
        .pointer("/card/legacy/name")
        .and_then(Value::as_str)
        .is_some_and(|name| name.starts_with("poll"));
    push_if(&mut subtypes, has_poll, ContentSubtype::Poll);

    PostClassification {
        content_type,
        subtypes,
    }
}

fn has_x_article_seed_link(target: &Value) -> bool {
    let text = target
        .pointer("/legacy/full_text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let entity_is_seed_link = [
        target.pointer("/legacy/entities/urls"),
        target.pointer("/note_tweet/note_tweet_results/result/entity_set/urls"),
        target.pointer("/note_tweet/entity_set/urls"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_array)
    .flatten()
    .any(|entity| {
        let is_article = ["expanded_url", "unwound_url", "url"]
            .into_iter()
            .filter_map(|field| entity.get(field).and_then(Value::as_str))
            .any(is_x_article_url);
        is_article
            && (text.is_empty()
                || ["url", "expanded_url", "unwound_url"]
                    .into_iter()
                    .filter_map(|field| entity.get(field).and_then(Value::as_str))
                    .any(|url| text == url.trim()))
    });
    entity_is_seed_link
        || target
            .pointer("/card/legacy/url")
            .and_then(Value::as_str)
            .is_some_and(|url| is_x_article_url(url) && (text.is_empty() || text == url.trim()))
}

fn push_if(subtypes: &mut Vec<ContentSubtype>, condition: bool, subtype: ContentSubtype) {
    if condition {
        subtypes.push(subtype);
    }
}

fn collect_media_subtypes(target: &Value, subtypes: &mut Vec<ContentSubtype>) {
    let media = target
        .pointer("/legacy/extended_entities/media")
        .or_else(|| target.pointer("/legacy/entities/media"))
        .and_then(Value::as_array);
    for media in media.into_iter().flatten() {
        let subtype = match media.get("type").and_then(Value::as_str) {
            Some("photo") => Some(ContentSubtype::Photo),
            Some("video") => Some(ContentSubtype::Video),
            Some("animated_gif") => Some(ContentSubtype::AnimatedGif),
            _ => None,
        };
        if let Some(subtype) = subtype
            && !subtypes.contains(&subtype)
        {
            subtypes.push(subtype);
        }
    }
}

/// Backwards-compatible single-value type emitted by `clix x read`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegacyTweetType {
    Article,
    NoteTweet,
    Media,
    Tweet,
}

impl fmt::Display for LegacyTweetType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Article => "Article",
            Self::NoteTweet => "NoteTweet",
            Self::Media => "Media",
            Self::Tweet => "Tweet",
        })
    }
}

/// Compatibility alias for callers of the original X reader API.
pub type TweetType = LegacyTweetType;

/// Classify the legacy reader output while preserving its historical precedence.
#[must_use]
pub const fn classify_tweet(
    has_article: bool,
    has_note_tweet: bool,
    has_media: bool,
) -> LegacyTweetType {
    if has_article {
        LegacyTweetType::Article
    } else if has_note_tweet {
        LegacyTweetType::NoteTweet
    } else if has_media {
        LegacyTweetType::Media
    } else {
        LegacyTweetType::Tweet
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        ContentSubtype, ContentType, LegacyTweetType, classify_post, classify_tweet,
        first_nonblank, media_extension,
    };

    #[test]
    fn blank_values_do_not_block_later_credentials() {
        assert_eq!(
            first_nonblank([Some("  ".into()), Some(" token ".into())]),
            Some("token".into())
        );
    }

    #[test]
    fn credential_debug_output_is_redacted() {
        let credentials = super::XCredentials {
            auth_token: "secret-auth".into(),
            ct0: "secret-csrf".into(),
        };
        let debug = format!("{credentials:?}");

        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret-auth"));
        assert!(!debug.contains("secret-csrf"));
    }

    #[test]
    fn classifies_legacy_output_using_the_existing_precedence() {
        assert_eq!(classify_tweet(true, true, true), LegacyTweetType::Article);
        assert_eq!(
            classify_tweet(false, true, true),
            LegacyTweetType::NoteTweet
        );
        assert_eq!(classify_tweet(false, false, true), LegacyTweetType::Media);
        assert_eq!(classify_tweet(false, false, false), LegacyTweetType::Tweet);
    }

    #[test]
    fn canonical_classification_keeps_primary_and_nonexclusive_subtypes() {
        let target = json!({
            "article": {},
            "quoted_status_result": {},
            "legacy": {
                "in_reply_to_status_id_str": "1",
                "extended_entities": {
                    "media": [
                        {"type": "photo"},
                        {"type": "video"},
                        {"type": "photo"}
                    ]
                }
            },
            "card": {"legacy": {"name": "poll2choice_text_only"}}
        });

        let classification = classify_post(&target);
        assert_eq!(classification.content_type, ContentType::Article);
        assert_eq!(
            classification.subtypes,
            vec![
                ContentSubtype::Quoted,
                ContentSubtype::RepliedTo,
                ContentSubtype::Photo,
                ContentSubtype::Video,
                ContentSubtype::Poll,
            ]
        );
        assert_eq!(classification.legacy_type(), LegacyTweetType::Article);

        let article_body_media = json!({
            "article": {
                "article_results": {
                    "result": {
                        "media_entities": [{
                            "media_info": {"__typename": "ApiImage"}
                        }]
                    }
                }
            }
        });
        assert!(
            classify_post(&article_body_media).subtypes.is_empty(),
            "Article-body media is content, not a seed-post attachment"
        );
    }

    #[test]
    fn article_url_is_part_of_the_shared_primary_classification() {
        let target = json!({
            "legacy": {
                "full_text": "https://t.co/article",
                "entities": {
                    "urls": [{
                        "url": "https://t.co/article",
                        "expanded_url": "https://x.com/i/article/2076861011957800960"
                    }]
                }
            }
        });

        assert_eq!(classify_post(&target).content_type, ContentType::Article);
    }

    #[test]
    fn shared_article_links_do_not_override_the_actual_post_shape() {
        let shared_link = json!({
            "legacy": {
                "full_text": "Worth reading https://t.co/article",
                "entities": {
                    "urls": [{
                        "url": "https://t.co/article",
                        "expanded_url": "https://x.com/i/article/2076861011957800960"
                    }]
                }
            }
        });
        let note_tweet = json!({
            "note_tweet": {
                "note_tweet_results": {
                    "result": {
                        "text": "Long commentary",
                        "entity_set": {
                            "urls": [{
                                "url": "https://t.co/article",
                                "expanded_url": "https://x.com/i/article/2076861011957800960"
                            }]
                        }
                    }
                }
            },
            "legacy": {"full_text": "https://t.co/article"}
        });

        assert_eq!(classify_post(&shared_link).content_type, ContentType::Post);
        assert_eq!(
            classify_post(&note_tweet).content_type,
            ContentType::NoteTweet
        );
    }

    #[test]
    fn media_extensions_are_safe_and_normalized() {
        assert_eq!(
            media_extension("https://pbs.twimg.com/media/id?format=png&name=large"),
            "png"
        );
        assert_eq!(
            media_extension("https://pbs.twimg.com/media/id.JPEG?name=large"),
            "jpg"
        );
        assert_eq!(
            media_extension("https://pbs.twimg.com/media/id?format=../../bad"),
            "jpg"
        );
    }
}
