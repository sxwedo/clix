use std::{
    collections::{BTreeMap, HashSet},
    sync::OnceLock,
};

use anyhow::{Result, anyhow, bail};
use clix_core::ui;
use clix_x_api::{
    ContentType, classify_post, error_excerpt, extract_author, extract_media_urls,
    is_x_article_url, response_error_excerpt,
};
use serde_json::Value;

use crate::model::{LinkPreview, TweetBookmark, TweetMetrics};

const QUERY_IDS: &[&str] = &["RV1g3b8n_SGOHwkqKYSCFw", "tmd4ifV8RHltzn8ymGg1aw"];
static BOOKMARK_FEATURES_JSON: OnceLock<String> = OnceLock::new();

pub fn bookmark_features_json() -> &'static str {
    BOOKMARK_FEATURES_JSON.get_or_init(|| {
        serde_json::json!({
            "graphql_timeline_v2_bookmark_timeline": true,
            "responsive_web_graphql_exclude_directive_enabled": true,
            "verified_phone_label_enabled": false,
            "creator_subscriptions_tweet_preview_api_enabled": true,
            "responsive_web_graphql_timeline_navigation_enabled": true,
            "responsive_web_graphql_skip_user_profile_image_extensions_enabled": false,
            "tweet_with_visibility_results_prefer_gql_limited_actions_policy_enabled": true,
            "tweet_awards_web_tipping_enabled": false,
            "freedom_of_speech_not_reach_fetch_enabled": true,
            "standardized_nudges_misinfo": true,
            "tweet_with_visibility_results_prefer_gql_media_interstitial_enabled": true,
            "rweb_video_timestamps_enabled": true,
            "view_counts_everywhere_api_enabled": true,
            "articles_preview_enabled": true,
            "longform_notetweets_consumption_enabled": true,
            "longform_notetweets_rich_text_read_enabled": true,
            "longform_notetweets_inline_media_enabled": true,
            "responsive_web_twitter_article_tweet_consumption_enabled": true,
            "responsive_web_twitter_article_plain_text_enabled": true,
            "responsive_web_twitter_article_seed_tweet_detail_enabled": true,
            "responsive_web_twitter_article_seed_tweet_summary_enabled": true,
            "responsive_web_enhance_cards_enabled": false
        })
        .to_string()
    })
}

pub async fn fetch_bookmarks(
    client: &reqwest::Client,
    features_json: &str,
    count: Option<usize>,
    incremental: bool,
    known_ids: &HashSet<String>,
) -> Result<Vec<TweetBookmark>> {
    let spinner = ui::create_spinner("fetching X bookmarks...");
    let mut bookmarks = Vec::new();
    let mut fetched_ids = HashSet::new();
    let mut cursor: Option<String> = None;
    let limit = count.unwrap_or(usize::MAX);

    loop {
        if bookmarks.len() >= limit {
            break;
        }

        spinner.set_message(format!(
            "fetching X bookmarks ({})",
            ui::style_bold(&format!("{} new", bookmarks.len()))
        ));

        let page_count = std::cmp::min(20, limit - bookmarks.len());
        let mut variables_json = serde_json::json!({
            "count": page_count,
            "includePromotedContent": false
        });
        if let Some(ref value) = cursor {
            variables_json["cursor"] = Value::String(value.clone());
        }

        let variables = variables_json.to_string();
        let page = match fetch_bookmark_page(client, features_json, &variables).await {
            Ok(page) => page,
            Err(error) => {
                spinner.finish_and_clear();
                return Err(error);
            }
        };
        let reached_known_boundary = page_is_known_boundary(&page.tweets, incremental, known_ids);
        for tweet in page.tweets {
            if bookmarks.len() >= limit {
                break;
            }
            if (!incremental || !known_ids.contains(&tweet.id))
                && fetched_ids.insert(tweet.id.clone())
            {
                bookmarks.push(tweet);
            }
        }
        let next_cursor = page.next_cursor;
        if reached_known_boundary || next_cursor.is_none() || next_cursor == cursor {
            break;
        }
        cursor = next_cursor;
    }

    spinner.finish_and_clear();
    Ok(bookmarks)
}

struct BookmarkPage {
    tweets: Vec<TweetBookmark>,
    next_cursor: Option<String>,
}

async fn fetch_bookmark_page(
    client: &reqwest::Client,
    features_json: &str,
    variables_json: &str,
) -> Result<BookmarkPage> {
    let mut attempts = Vec::with_capacity(QUERY_IDS.len());

    for query_id in QUERY_IDS {
        let url = format!("https://x.com/i/api/graphql/{query_id}/Bookmarks");
        let response = match client
            .get(&url)
            .query(&[("variables", variables_json), ("features", features_json)])
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                attempts.push(format!("{query_id}: request failed: {error}"));
                continue;
            }
        };
        let status = response.status();
        if !status.is_success() {
            let summary = response_error_excerpt(response, 500).await;
            if matches!(status.as_u16(), 401 | 403) {
                bail!("X authentication failed (HTTP {status}): {summary}");
            }
            if status.as_u16() == 429 {
                bail!("X rate limit exceeded (HTTP 429): {summary}");
            }
            attempts.push(format!("{query_id}: HTTP {status}: {summary}"));
            continue;
        }
        let body: Value = match response.json().await {
            Ok(body) => body,
            Err(error) => {
                attempts.push(format!("{query_id}: invalid JSON response: {error}"));
                continue;
            }
        };
        if has_bookmark_timeline(&body) {
            let (tweets, next_cursor) = parse_bookmarks_response(&body);
            return Ok(BookmarkPage {
                tweets,
                next_cursor,
            });
        }
        if let Some(errors) = body.get("errors") {
            attempts.push(format!(
                "{query_id}: GraphQL errors: {}",
                error_excerpt(&errors.to_string(), 500)
            ));
        } else {
            attempts.push(format!("{query_id}: bookmark timeline was absent"));
        }
    }

    Err(anyhow!(
        "failed to fetch X bookmark page: {}",
        attempts.join("; ")
    ))
}

pub fn page_is_known_boundary(
    page: &[TweetBookmark],
    incremental: bool,
    known_ids: &HashSet<String>,
) -> bool {
    incremental && !page.is_empty() && page.iter().all(|bookmark| known_ids.contains(&bookmark.id))
}

pub fn has_bookmark_timeline(value: &Value) -> bool {
    value
        .pointer("/data/bookmark_timeline_v2/timeline/instructions")
        .or_else(|| value.pointer("/data/bookmark_timeline/timeline/instructions"))
        .and_then(Value::as_array)
        .is_some()
}

pub fn parse_bookmarks_response(val: &Value) -> (Vec<TweetBookmark>, Option<String>) {
    let mut tweets = Vec::new();
    let mut next_cursor = None;

    let instructions = val
        .pointer("/data/bookmark_timeline_v2/timeline/instructions")
        .or_else(|| val.pointer("/data/bookmark_timeline/timeline/instructions"))
        .and_then(|v| v.as_array());

    if let Some(instructions) = instructions {
        for inst in instructions {
            let entries = inst.get("entries").and_then(|v| v.as_array());
            if let Some(entries) = entries {
                for entry in entries {
                    // Check for bottom cursor
                    let entry_id = entry.get("entryId").and_then(|v| v.as_str()).unwrap_or("");
                    let cursor_type = entry
                        .pointer("/content/cursorType")
                        .and_then(|v| v.as_str());

                    if (entry_id.starts_with("cursor-bottom-") || cursor_type == Some("Bottom"))
                        && let Some(val) = entry.pointer("/content/value").and_then(|v| v.as_str())
                    {
                        next_cursor = Some(val.to_string());
                    }

                    for tweet_result in bookmark_entry_tweet_results(entry) {
                        if let Some(tweet) = extract_tweet(tweet_result) {
                            tweets.push(tweet);
                        }
                    }
                }
            }
        }
    }

    (tweets, next_cursor)
}

fn bookmark_entry_tweet_results(entry: &Value) -> Vec<&Value> {
    let mut results = Vec::new();
    if let Some(result) = entry
        .pointer("/content/itemContent/tweet_results/result")
        .or_else(|| entry.pointer("/item/itemContent/tweet_results/result"))
    {
        results.push(result);
    }

    let module_items = entry
        .pointer("/content/items")
        .or_else(|| entry.pointer("/item/content/items"))
        .and_then(Value::as_array);
    for item in module_items.into_iter().flatten() {
        if let Some(result) = item
            .pointer("/item/itemContent/tweet_results/result")
            .or_else(|| item.pointer("/itemContent/tweet_results/result"))
        {
            results.push(result);
        }
    }
    results
}

pub fn extract_tweet(res: &Value) -> Option<TweetBookmark> {
    // Handle TweetWithVisibilityResults wrapper
    let target =
        if res.get("__typename").and_then(|v| v.as_str()) == Some("TweetWithVisibilityResults") {
            res.get("tweet")?
        } else {
            res
        };

    let id = target.get("rest_id")?.as_str()?.to_string();

    let (author_name, author_handle) = extract_author(target);

    let article_title = target
        .pointer("/article/article_results/result/title")
        .or_else(|| target.pointer("/article/title"))
        .and_then(|v| v.as_str());

    let raw_text = target
        .pointer("/note_tweet/note_tweet_results/result/text")
        .and_then(|v| v.as_str())
        .or_else(|| target.pointer("/legacy/full_text").and_then(|v| v.as_str()))
        .unwrap_or("");

    let text = raw_text.to_string();

    let created_at = target
        .pointer("/legacy/created_at")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let media = extract_media_urls(target);
    let links = extract_link_previews(target, article_title);
    let classification = classify_post(target);
    let metrics = extract_metrics(target);
    let url = format!("https://x.com/{author_handle}/status/{id}");

    Some(TweetBookmark {
        id,
        content_type: classification.content_type,
        subtypes: classification.subtypes,
        author_name,
        author_handle,
        text,
        created_at,
        url,
        links,
        metrics,
        media,
        local_media: Vec::new(),
    })
}

fn extract_metrics(target: &Value) -> TweetMetrics {
    TweetMetrics {
        bookmarks: value_as_u64(target.pointer("/legacy/bookmark_count")),
        likes: value_as_u64(target.pointer("/legacy/favorite_count")),
        replies: value_as_u64(target.pointer("/legacy/reply_count")),
        views: value_as_u64(target.pointer("/views/count")),
        reposts: value_as_u64(target.pointer("/legacy/retweet_count")),
        quotes: value_as_u64(target.pointer("/legacy/quote_count")),
    }
}

fn extract_link_previews(target: &Value, article_title: Option<&str>) -> Vec<LinkPreview> {
    let legacy_urls = target
        .pointer("/legacy/entities/urls")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let note_urls = target
        .pointer("/note_tweet/note_tweet_results/result/entity_set/urls")
        .or_else(|| target.pointer("/note_tweet/entity_set/urls"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let card_title =
        extract_card_binding_string(target, "title").filter(|title| !title.trim().is_empty());
    let mut previews = Vec::with_capacity((legacy_urls.len() + note_urls.len()).max(1));
    let mut seen_urls = HashSet::new();

    for entity in legacy_urls.iter().chain(note_urls) {
        let Some(url) = entity.get("url").and_then(Value::as_str) else {
            continue;
        };
        if url.trim().is_empty() || !seen_urls.insert(url) {
            continue;
        }
        let expanded_url = entity
            .get("expanded_url")
            .and_then(Value::as_str)
            .map(normalize_expanded_url);
        let title = entity
            .pointer("/unwound/title")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .filter(|title| !title.trim().is_empty());
        previews.push(LinkPreview {
            title,
            url: url.to_string(),
            expanded_url,
        });
    }

    if previews.is_empty()
        && let Some(url) = target.pointer("/card/legacy/url").and_then(Value::as_str)
        && !url.trim().is_empty()
    {
        previews.push(LinkPreview {
            title: None,
            url: url.to_string(),
            expanded_url: None,
        });
    }

    if let Some(title) = article_title.map(ToOwned::to_owned).or(card_title) {
        let title_index = previews
            .iter()
            .position(|preview| {
                preview
                    .expanded_url
                    .as_deref()
                    .is_some_and(is_x_article_url)
            })
            .or_else(|| (!previews.is_empty()).then_some(0));
        if let Some(index) = title_index {
            previews[index].title = Some(title);
        }
    }

    previews
}

fn normalize_expanded_url(value: &str) -> String {
    if let Ok(mut url) = reqwest::Url::parse(value)
        && matches!(
            url.host_str(),
            Some("x.com" | "www.x.com" | "twitter.com" | "www.twitter.com")
        )
        && url.set_scheme("https").is_ok()
    {
        return url.to_string();
    }

    value.to_string()
}

pub async fn enrich_article_titles(client: &reqwest::Client, bookmarks: &mut [TweetBookmark]) {
    const MAX_CONCURRENT_TITLE_REQUESTS: usize = 4;

    let candidates = bookmarks
        .iter()
        .enumerate()
        .filter(|(_, bookmark)| needs_article_title(bookmark))
        .map(|(index, bookmark)| (index, bookmark.id.clone()))
        .collect::<Vec<_>>();
    let candidate_count = candidates.len();
    if candidate_count == 0 {
        return;
    }

    let spinner = ui::create_spinner(&format!(
        "fetching titles for {candidate_count} X articles..."
    ));
    let mut enriched = 0;
    let mut completed = 0;
    let mut omitted = 0;
    let mut failed = 0;
    let mut first_failure = None;
    let mut rate_limited = false;
    let mut candidates = candidates.into_iter();
    let mut tasks = tokio::task::JoinSet::new();

    for candidate in candidates.by_ref().take(MAX_CONCURRENT_TITLE_REQUESTS) {
        spawn_title_request(&mut tasks, client.clone(), candidate);
    }

    while let Some(result) = tasks.join_next().await {
        completed += 1;
        spinner.set_message(format!(
            "fetching X article titles ({completed}/{candidate_count})"
        ));
        match result {
            Ok((bookmark_index, Ok(Some(title)))) => {
                if let Some(link_index) = article_link_index(&bookmarks[bookmark_index]) {
                    bookmarks[bookmark_index].links[link_index].title = Some(title);
                    enriched += 1;
                }
            }
            Ok((_, Ok(None))) => omitted += 1,
            Ok((_, Err(error))) => {
                failed += 1;
                let error = error.to_string();
                rate_limited |= error.to_ascii_lowercase().contains("rate limit");
                first_failure.get_or_insert(error);
            }
            Err(error) => {
                failed += 1;
                first_failure.get_or_insert_with(|| error.to_string());
            }
        }
        if !rate_limited && let Some(candidate) = candidates.next() {
            spawn_title_request(&mut tasks, client.clone(), candidate);
        }
    }
    if rate_limited {
        failed += candidates.count();
    }

    spinner.finish_and_clear();
    if enriched > 0 {
        ui::success(format!("added titles for {enriched} X articles"));
    }
    if omitted > 0 {
        ui::warn(format!(
            "X omitted titles for {omitted} articles; their original links were preserved"
        ));
    }
    if failed > 0 {
        let detail = first_failure
            .as_deref()
            .map(|error| format!("; first error: {}", error_excerpt(error, 300)))
            .unwrap_or_default();
        ui::warn(format!(
            "could not fetch titles for {failed} articles; their original links were preserved{detail}"
        ));
    }
}

pub fn apply_cached_article_titles(
    bookmarks: &mut [TweetBookmark],
    titles: &BTreeMap<String, String>,
) {
    for bookmark in bookmarks {
        if let Some(title) = titles.get(&bookmark.id)
            && let Some(index) = article_link_index(bookmark)
            && bookmark.links[index].title.is_none()
        {
            bookmark.links[index].title = Some(title.clone());
        }
    }
}

fn spawn_title_request(
    tasks: &mut tokio::task::JoinSet<(usize, Result<Option<String>>)>,
    client: reqwest::Client,
    (bookmark_index, tweet_id): (usize, String),
) {
    tasks.spawn(async move {
        let title = clix_x_api::fetch_article_title(&client, &tweet_id).await;
        (bookmark_index, title)
    });
}

fn needs_article_title(bookmark: &TweetBookmark) -> bool {
    bookmark.content_type == ContentType::Article
        && article_link_index(bookmark).is_some_and(|index| bookmark.links[index].title.is_none())
}

pub fn article_link_index(bookmark: &TweetBookmark) -> Option<usize> {
    bookmark
        .links
        .iter()
        .position(|preview| {
            preview
                .expanded_url
                .as_deref()
                .is_some_and(is_x_article_url)
        })
        .or_else(|| {
            (bookmark.content_type == ContentType::Article && !bookmark.links.is_empty())
                .then_some(0)
        })
}

fn extract_card_binding_string(target: &Value, key: &str) -> Option<String> {
    let bindings = target.pointer("/card/legacy/binding_values")?;

    if let Some(binding) = bindings.get(key) {
        return binding
            .pointer("/string_value")
            .or_else(|| binding.pointer("/value/string_value"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
    }

    bindings.as_array().and_then(|bindings| {
        bindings.iter().find_map(|binding| {
            (binding.get("key").and_then(Value::as_str) == Some(key))
                .then(|| {
                    binding
                        .pointer("/value/string_value")
                        .or_else(|| binding.pointer("/string_value"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .flatten()
        })
    })
}

fn value_as_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
    })
}
