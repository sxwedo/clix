use std::{sync::OnceLock, time::Duration};

use crate::content::{extract_author, extract_full_text, extract_media_urls};
use crate::{
    ContentSubtype, ContentType, TweetType, classify_post, error_excerpt, response_error_excerpt,
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const TWEET_DETAIL_QUERY_IDS: &[&str] = &[
    "Lq1caG5YPcdhpTdS2ZRx7Q",
    "_NvJCnIjOW__EP5-RF197A",
    "97JF30KziU00483E_8elBA",
    "aFvUsJm2c-oDkJV75blV6g",
];
static FEATURES_JSON: OnceLock<String> = OnceLock::new();
static FIELD_TOGGLES_JSON: OnceLock<String> = OnceLock::new();
static TITLE_FIELD_TOGGLES_JSON: OnceLock<String> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TweetDetail {
    pub id: String,
    pub content_type: ContentType,
    #[serde(default)]
    pub subtypes: Vec<ContentSubtype>,
    /// Historical single-value type retained for JSON/frontmatter compatibility.
    pub tweet_type: TweetType,
    pub author_name: String,
    pub author_handle: String,
    pub text: String,
    pub article_title: Option<String>,
    pub created_at: String,
    pub url: String,
    pub media_urls: Vec<String>,
    pub local_media: Vec<String>,
}

/// Fetch and parse one X post using an already-authenticated HTTP client.
///
/// This is also used by the bookmark exporter to enrich X Article titles that
/// are omitted from the bookmark timeline payload.
///
/// # Errors
///
/// Returns an error when all known GraphQL query variants fail, X rejects the
/// credentials or rate-limits the request, or the requested post is absent.
pub async fn fetch_tweet_detail(client: &reqwest::Client, tweet_id: &str) -> Result<TweetDetail> {
    fetch_tweet_with(
        client,
        tweet_id,
        tweet_field_toggles_json(),
        parse_tweet_detail_response,
    )
    .await
}

/// Fetch only the title attached to an X Article seed post.
///
/// Unlike [`fetch_tweet_detail`], this skips full rich-text and media parsing,
/// which keeps bulk bookmark title enrichment inexpensive.
///
/// # Errors
///
/// Returns an error when all known GraphQL query variants fail, X rejects the
/// credentials or rate-limits the request, or the requested post is absent.
pub async fn fetch_article_title(
    client: &reqwest::Client,
    tweet_id: &str,
) -> Result<Option<String>> {
    fetch_tweet_with(
        client,
        tweet_id,
        article_title_field_toggles_json(),
        |response, target_id| find_tweet_result(response, target_id).map(extract_article_title),
    )
    .await
}

async fn fetch_tweet_with<T>(
    client: &reqwest::Client,
    tweet_id: &str,
    field_toggles_json: &str,
    parse: impl Fn(&Value, &str) -> Option<T>,
) -> Result<T> {
    const OPERATION_TIMEOUT: Duration = Duration::from_mins(1);

    tokio::time::timeout(
        OPERATION_TIMEOUT,
        fetch_tweet_with_inner(client, tweet_id, field_toggles_json, parse),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "X status {tweet_id} request exceeded the {} second operation timeout",
            OPERATION_TIMEOUT.as_secs()
        )
    })?
}

async fn fetch_tweet_with_inner<T>(
    client: &reqwest::Client,
    tweet_id: &str,
    field_toggles_json: &str,
    parse: impl Fn(&Value, &str) -> Option<T>,
) -> Result<T> {
    let variables_json = tweet_variables_json(tweet_id);
    let features_json = tweet_features_json();
    let mut attempts = Vec::with_capacity(TWEET_DETAIL_QUERY_IDS.len());

    for query_id in TWEET_DETAIL_QUERY_IDS {
        let url = format!("https://x.com/i/api/graphql/{query_id}/TweetDetail");
        let response = match client
            .get(&url)
            .query(&[
                ("variables", variables_json.as_str()),
                ("features", features_json),
                ("fieldToggles", field_toggles_json),
            ])
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

        if let Some(value) = parse(&body, tweet_id) {
            return Ok(value);
        }
        if let Some(errors) = body.get("errors") {
            attempts.push(format!(
                "{query_id}: GraphQL errors: {}",
                error_excerpt(&errors.to_string(), 500)
            ));
        } else {
            attempts.push(format!(
                "{query_id}: target post was absent from the response"
            ));
        }
    }

    bail!(
        "failed to fetch X status {tweet_id}: {}",
        attempts.join("; ")
    )
}

fn tweet_variables_json(tweet_id: &str) -> String {
    serde_json::json!({
        "focalTweetId": tweet_id,
        "with_rux_injections": false,
        "rankingMode": "Relevance",
        "includePromotedContent": true,
        "withCommunity": true,
        "withQuickPromoteEligibilityTweetFields": true,
        "withBirdwatchNotes": true,
        "withVoice": true
    })
    .to_string()
}

fn tweet_features_json() -> &'static str {
    FEATURES_JSON.get_or_init(|| {
        serde_json::json!({
            "rweb_video_screen_enabled": true,
            "profile_label_improvements_pcf_label_in_post_enabled": true,
            "responsive_web_profile_redirect_enabled": true,
            "rweb_tipjar_consumption_enabled": true,
            "verified_phone_label_enabled": false,
            "creator_subscriptions_tweet_preview_api_enabled": true,
            "responsive_web_graphql_timeline_navigation_enabled": true,
            "responsive_web_graphql_exclude_directive_enabled": true,
            "responsive_web_graphql_skip_user_profile_image_extensions_enabled": false,
            "premium_content_api_read_enabled": false,
            "communities_web_enable_tweet_community_results_fetch": true,
            "c9s_tweet_anatomy_moderator_badge_enabled": true,
            "responsive_web_grok_analyze_button_fetch_trends_enabled": false,
            "responsive_web_grok_analyze_post_followups_enabled": false,
            "responsive_web_grok_annotations_enabled": false,
            "responsive_web_jetfuel_frame": true,
            "post_ctas_fetch_enabled": true,
            "responsive_web_grok_share_attachment_enabled": true,
            "articles_preview_enabled": true,
            "responsive_web_edit_tweet_api_enabled": true,
            "graphql_is_translatable_rweb_tweet_is_translatable_enabled": true,
            "view_counts_everywhere_api_enabled": true,
            "longform_notetweets_consumption_enabled": true,
            "responsive_web_twitter_article_tweet_consumption_enabled": true,
            "tweet_awards_web_tipping_enabled": false,
            "responsive_web_grok_show_grok_translated_post": false,
            "responsive_web_grok_analysis_button_from_backend": true,
            "creator_subscriptions_quote_tweet_preview_enabled": false,
            "freedom_of_speech_not_reach_fetch_enabled": true,
            "standardized_nudges_misinfo": true,
            "tweet_with_visibility_results_prefer_gql_limited_actions_policy_enabled": true,
            "longform_notetweets_rich_text_read_enabled": true,
            "longform_notetweets_inline_media_enabled": true,
            "responsive_web_grok_image_annotation_enabled": true,
            "responsive_web_grok_imagine_annotation_enabled": true,
            "responsive_web_grok_community_note_auto_translation_is_enabled": false,
            "responsive_web_enhance_cards_enabled": false,
            "responsive_web_twitter_article_plain_text_enabled": true,
            "responsive_web_twitter_article_seed_tweet_detail_enabled": true,
            "responsive_web_twitter_article_seed_tweet_summary_enabled": true
        })
        .to_string()
    })
}

fn tweet_field_toggles_json() -> &'static str {
    FIELD_TOGGLES_JSON.get_or_init(|| {
        serde_json::json!({
            "withPayments": false,
            "withAuxiliaryUserLabels": false,
            "withArticleRichContentState": true,
            "withArticlePlainText": true,
            "withGrokAnalyze": false,
            "withDisallowedReplyControls": false
        })
        .to_string()
    })
}

fn article_title_field_toggles_json() -> &'static str {
    TITLE_FIELD_TOGGLES_JSON.get_or_init(|| {
        serde_json::json!({
            "withPayments": false,
            "withAuxiliaryUserLabels": false,
            "withArticleRichContentState": false,
            "withArticlePlainText": false,
            "withGrokAnalyze": false,
            "withDisallowedReplyControls": false
        })
        .to_string()
    })
}

fn parse_tweet_detail_response(val: &Value, tweet_id: &str) -> Option<TweetDetail> {
    let detail =
        find_tweet_result(val, tweet_id).map(|target| extract_tweet_detail(target, tweet_id))?;
    (detail.author_handle != "unknown"
        && !detail.created_at.is_empty()
        && !detail.text.trim().is_empty())
    .then_some(detail)
}

fn find_tweet_result<'a>(value: &'a Value, tweet_id: &str) -> Option<&'a Value> {
    let mut pending = vec![value];
    while let Some(candidate) = pending.pop() {
        if let Some(target) = unwrap_tweet_result(candidate)
            && is_target_tweet(target, tweet_id)
        {
            return Some(target);
        }
        match candidate {
            Value::Array(items) => pending.extend(items),
            Value::Object(fields) => {
                pending.extend(fields.values());
            }
            _ => {}
        }
    }

    None
}

fn is_target_tweet(value: &Value, tweet_id: &str) -> bool {
    if value.get("rest_id").and_then(Value::as_str) != Some(tweet_id) {
        return false;
    }

    value
        .get("__typename")
        .and_then(Value::as_str)
        .is_some_and(|name| name.starts_with("Tweet"))
        || value.pointer("/legacy/full_text").is_some()
        || value.get("article").is_some()
        || value.get("note_tweet").is_some()
}

fn unwrap_tweet_result(result: &Value) -> Option<&Value> {
    if result.get("__typename").and_then(Value::as_str) == Some("TweetWithVisibilityResults") {
        result.get("tweet")
    } else {
        Some(result)
    }
}

fn extract_tweet_detail(target: &Value, target_id: &str) -> TweetDetail {
    let (author_name, author_handle) = extract_author(target);
    let article_title = extract_article_title(target);
    let media_urls = extract_media_urls(target);
    let full_text = extract_full_text(target, &media_urls);
    let created_at = target
        .pointer("/legacy/created_at")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let classification = classify_post(target);
    let tweet_type = classification.legacy_type();
    let url = format!("https://x.com/{author_handle}/status/{target_id}");

    TweetDetail {
        id: target_id.to_string(),
        content_type: classification.content_type,
        subtypes: classification.subtypes,
        tweet_type,
        author_name,
        author_handle,
        text: full_text,
        article_title,
        created_at,
        url,
        media_urls,
        local_media: Vec::new(),
    }
}

fn extract_article_title(target: &Value) -> Option<String> {
    target
        .pointer("/article/article_results/result/title")
        .or_else(|| target.pointer("/article/title"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        extract_article_title, extract_tweet_detail, find_tweet_result, parse_tweet_detail_response,
    };
    use crate::{ContentSubtype, ContentType, TweetType, error_excerpt};

    #[test]
    fn extraction_uses_shared_types_and_preserves_original_time() {
        let result = json!({
            "__typename": "Tweet",
            "rest_id": "123456789",
            "core": {
                "user_results": {
                    "result": {
                        "legacy": {
                            "screen_name": "alice",
                            "name": "Alice"
                        }
                    }
                }
            },
            "legacy": {
                "full_text": "Post with media",
                "created_at": "Wed Oct 10 20:19:24 +0000 2018",
                "extended_entities": {
                    "media": [{
                        "type": "photo",
                        "media_url_https": "https://pbs.twimg.com/media/example.jpg"
                    }]
                }
            }
        });

        let response = json!({"data": {"tweetResult": {"result": result}}});
        let detail = parse_tweet_detail_response(&response, "123456789")
            .expect("tweet detail should be extracted");

        assert_eq!(detail.tweet_type, TweetType::Media);
        assert_eq!(detail.content_type, ContentType::Post);
        assert_eq!(detail.subtypes, vec![ContentSubtype::Photo]);
        assert_eq!(detail.created_at, "Wed Oct 10 20:19:24 +0000 2018");
    }

    #[test]
    fn article_presence_is_enough_to_classify_an_article() {
        let result = json!({
            "__typename": "Tweet",
            "rest_id": "123456789",
            "core": {
                "user_results": {
                    "result": {
                        "legacy": {
                            "screen_name": "alice",
                            "name": "Alice"
                        }
                    }
                }
            },
            "article": {"article_results": {"result": {}}},
            "legacy": {
                "full_text": "Article preview",
                "created_at": "Wed Oct 10 20:19:24 +0000 2018"
            }
        });

        let response = json!({"data": {"tweetResult": {"result": result}}});
        let detail = parse_tweet_detail_response(&response, "123456789")
            .expect("article detail should be extracted");

        assert_eq!(detail.tweet_type, TweetType::Article);
        assert_eq!(detail.content_type, ContentType::Article);
    }

    #[test]
    fn article_url_uses_the_same_primary_type_as_bookmarks() {
        let target = json!({
            "__typename": "Tweet",
            "rest_id": "123456789",
            "core": {
                "user_results": {
                    "result": {
                        "legacy": {
                            "screen_name": "alice",
                            "name": "Alice"
                        }
                    }
                }
            },
            "legacy": {
                "full_text": "https://t.co/article",
                "created_at": "Wed Oct 10 20:19:24 +0000 2018",
                "entities": {
                    "urls": [{
                        "url": "https://t.co/article",
                        "expanded_url": "https://x.com/i/article/2076861011957800960"
                    }]
                }
            }
        });

        let detail = extract_tweet_detail(&target, "123456789");
        assert_eq!(detail.content_type, ContentType::Article);
        assert_eq!(detail.tweet_type, TweetType::Article);
    }

    #[test]
    fn article_title_parser_distinguishes_a_titleless_post_from_an_absent_post() {
        let response = json!({
            "data": {
                "tweetResult": {
                    "result": {
                        "__typename": "TweetWithVisibilityResults",
                        "tweet": {
                            "rest_id": "123",
                            "article": {
                                "article_results": {
                                    "result": {"title": "A precise title"}
                                }
                            }
                        }
                    }
                }
            }
        });

        let target = find_tweet_result(&response, "123").expect("target post should be found");
        assert_eq!(
            extract_article_title(target).as_deref(),
            Some("A precise title")
        );
        assert!(find_tweet_result(&response, "999").is_none());
    }

    #[test]
    fn timeline_modules_are_searched_and_incomplete_posts_are_rejected() {
        let target = json!({
            "__typename": "Tweet",
            "rest_id": "123",
            "core": {
                "user_results": {
                    "result": {
                        "legacy": {"screen_name": "alice", "name": "Alice"}
                    }
                }
            },
            "legacy": {
                "full_text": "Nested post",
                "created_at": "Wed Oct 10 20:19:24 +0000 2018"
            }
        });
        let response = json!({
            "data": {
                "threaded_conversation_with_injections_v2": {
                    "instructions": [{
                        "entries": [{
                            "content": {
                                "items": [{
                                    "item": {
                                        "itemContent": {
                                            "tweet_results": {"result": target}
                                        }
                                    }
                                }]
                            }
                        }]
                    }]
                }
            }
        });

        assert!(find_tweet_result(&response, "123").is_some());
        assert!(parse_tweet_detail_response(&response, "123").is_some());

        let incomplete = json!({
            "data": {
                "tweetResult": {
                    "result": {
                        "__typename": "Tweet",
                        "rest_id": "123",
                        "legacy": {"full_text": "", "created_at": ""}
                    }
                }
            }
        });
        assert!(parse_tweet_detail_response(&incomplete, "123").is_none());
    }

    #[test]
    fn error_summaries_are_bounded() {
        assert_eq!(error_excerpt("abcdef", 3), "abc…");
        assert_eq!(error_excerpt("abc", 3), "abc");
    }
}
