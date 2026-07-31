mod api;
mod media;
mod model;
mod output;
mod state;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

type KnownState = (
    HashSet<String>,
    BTreeMap<String, String>,
    Option<ExistingOutput>,
);

use anyhow::{Context, Result, bail};
use clix_core::ui;
pub use clix_x_api::{ContentSubtype, ContentType};
use clix_x_api::{XCredentials, build_media_client};

use api::{
    apply_cached_article_titles, article_link_index, bookmark_features_json, enrich_article_titles,
    fetch_bookmarks,
};
#[cfg(test)]
use api::{extract_tweet, has_bookmark_timeline, page_is_known_boundary, parse_bookmarks_response};
use media::download_all_media;
pub use model::{BookmarksArgs, LinkPreview, OutputFormat, TweetBookmark, TweetMetrics};
use output::{
    ExistingOutput, acquire_export_lock, append_output, ensure_distinct_paths, legacy_state_path,
    write_output,
};
#[cfg(test)]
use output::{format_tweet_cell, markdown_status_ids_and_count, render_output};
use state::{BookmarkSnapshot, BookmarksDb};

/// Default bookmark state database path: `~/.config/clix/bookmarks.redb`.
fn default_state_db_path() -> PathBuf {
    clix_core::settings::config_dir().join("bookmarks.redb")
}

/// Migrate a legacy `<output>.state.json` sidecar into the redb store.
///
/// Only runs when the database is empty. On success the JSON file is renamed
/// to `<output>.state.json.bak`. Returns `true` when a migration happened.
fn migrate_legacy_state(db: &BookmarksDb, output_path: &Path) -> Result<bool> {
    let legacy = legacy_state_path(output_path);
    if !legacy.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(&legacy)
        .with_context(|| format!("failed to read legacy state {}", legacy.display()))?;
    let state: model::BookmarkState = serde_json::from_str(&content)
        .with_context(|| format!("invalid legacy state {}", legacy.display()))?;
    if state.version != model::STATE_VERSION {
        ui::warn(format!(
            "skipping legacy state migration: unsupported version {} in {}",
            state.version,
            legacy.display()
        ));
        return Ok(false);
    }
    let migrated = state.seen_tweet_ids.len();
    let entries: Vec<(&str, &str)> = state
        .seen_tweet_ids
        .iter()
        .map(|id| {
            (
                id.as_str(),
                state.article_titles.get(id).map_or("", String::as_str),
            )
        })
        .collect();
    db.upsert(entries)?;
    let backup: PathBuf = format!("{}.bak", legacy.display()).into();
    std::fs::rename(&legacy, &backup)?;
    ui::success(format!(
        "migrated {migrated} bookmarks from {} to redb",
        legacy.display()
    ));
    Ok(true)
}

/// Export the authenticated X account's bookmarks.
///
/// Resolve prior dedup state: known tweet IDs and cached article titles.
///
/// Existing output is folded in only for incremental runs.
fn prepare_known_state(
    snapshot: BookmarkSnapshot,
    output_path: &Path,
    format: OutputFormat,
    incremental: bool,
) -> Result<KnownState> {
    let mut known_ids = if incremental {
        snapshot.known_ids
    } else {
        HashSet::new()
    };
    let mut article_titles = snapshot.article_titles;

    let existing_output = if output_path.exists() {
        match ExistingOutput::load(output_path, format) {
            Ok(existing) => {
                if incremental {
                    known_ids.extend(existing.ids().iter().cloned());
                }
                Some(existing)
            }
            Err(error) if incremental => return Err(error),
            Err(error) => {
                ui::warn(format!(
                    "could not reuse existing Article titles from {}: {error:#}",
                    output_path.display()
                ));
                None
            }
        }
    } else {
        None
    };
    article_titles.extend(
        existing_output
            .as_ref()
            .map(ExistingOutput::article_titles)
            .unwrap_or_default(),
    );

    Ok((known_ids, article_titles, existing_output))
}

/// Build the `(tweet id, article title)` entries to persist from fetched bookmarks.
fn state_entries(bookmarks: &[TweetBookmark]) -> Vec<(&str, &str)> {
    bookmarks
        .iter()
        .map(|bookmark| {
            let title = article_link_index(bookmark)
                .and_then(|index| bookmark.links[index].title.as_deref())
                .unwrap_or("");
            (bookmark.id.as_str(), title)
        })
        .collect()
}

/// Export the authenticated X account's bookmarks.
///
/// # Errors
///
/// Returns an error for missing credentials, invalid output/state paths,
/// unsuccessful X requests, incompatible incremental output, media-directory
/// failures, or output/state persistence failures.
pub async fn run(args: BookmarksArgs, settings: &clix_core::settings::Settings) -> Result<()> {
    let Some(credentials) = XCredentials::resolve(args.auth_token, args.ct0, &settings.x) else {
        bail!(
            "Missing X authentication credentials!\n\n\
             Provide them via (highest priority first):\n  \
             • CLI flags:  clix x bookmarks --auth-token \"<auth_token>\" --ct0 \"<ct0>\"\n  \
             • config.toml [x] section (~/.config/clix/config.toml)\n  \
             • env vars:   export X_AUTH_TOKEN=\"...\"; export X_CT0=\"...\""
        );
    };

    let output_path = args.output.unwrap_or_else(|| match args.format {
        OutputFormat::Markdown => PathBuf::from("x_bookmarks.md"),
        OutputFormat::Urls => PathBuf::from("x_bookmarks_urls.txt"),
        OutputFormat::Json => PathBuf::from("x_bookmarks.json"),
    });
    let state_path = args.state.unwrap_or_else(default_state_db_path);
    ensure_distinct_paths(&output_path, &state_path)?;

    let _output_lock = acquire_export_lock(&output_path)?;

    let db = BookmarksDb::open(&state_path)?;
    let mut snapshot = db.snapshot()?;
    if snapshot.is_empty() && migrate_legacy_state(&db, &output_path)? {
        snapshot = db.snapshot()?;
    }

    let (known_ids, article_titles, existing_output) =
        prepare_known_state(snapshot, &output_path, args.format, args.incremental)?;

    let client = credentials.build_client()?;

    let mut bookmarks = fetch_bookmarks(
        &client,
        bookmark_features_json(),
        args.count,
        args.incremental,
        &known_ids,
    )
    .await?;

    if bookmarks.is_empty() && args.incremental {
        ui::success(format!(
            "X bookmarks are already up to date; state saved to {}",
            ui::style_bold(&state_path.display().to_string())
        ));
        return Ok(());
    }
    if bookmarks.is_empty() {
        bail!(
            "No bookmarks found or failed to authenticate with X. \
             Please verify your auth_token and ct0 values."
        );
    }

    apply_cached_article_titles(&mut bookmarks, &article_titles);
    if args.link_only {
        // link_only keeps the article link only; skip title enrichment entirely
    } else {
        enrich_article_titles(&client, &mut bookmarks).await;
    }

    if args.download_media {
        let media_client = build_media_client()?;
        download_all_media(&media_client, &mut bookmarks, &output_path).await?;
    }

    if args.incremental {
        append_output(
            &bookmarks,
            &output_path,
            args.format,
            args.link_only,
            &known_ids,
            existing_output.as_ref(),
        )?;
    } else {
        write_output(&bookmarks, &output_path, args.format, args.link_only)?;
        db.clear()?;
    }
    db.upsert(state_entries(&bookmarks))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashSet},
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use serde_json::json;

    use super::{
        BookmarksDb, ContentSubtype, ContentType, ExistingOutput, LinkPreview, OutputFormat,
        TweetBookmark, TweetMetrics, acquire_export_lock, append_output, article_link_index,
        ensure_distinct_paths, extract_tweet, format_tweet_cell, has_bookmark_timeline,
        markdown_status_ids_and_count, migrate_legacy_state, page_is_known_boundary,
        parse_bookmarks_response, render_output, write_output,
    };
    use crate::model::STATE_VERSION;

    fn tweet_fixture(extra: serde_json::Value) -> serde_json::Value {
        let mut tweet = json!({
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
                "full_text": "Original post",
                "created_at": "Wed Oct 10 20:19:24 +0000 2018"
            }
        });

        let tweet_object = tweet.as_object_mut().expect("tweet fixture is an object");
        let serde_json::Value::Object(extra_object) = extra else {
            panic!("extra fixture is an object");
        };
        tweet_object.extend(extra_object);
        tweet
    }

    fn bookmark_fixture(id: &str) -> TweetBookmark {
        TweetBookmark {
            id: id.to_string(),
            content_type: ContentType::Article,
            subtypes: vec![ContentSubtype::Quoted, ContentSubtype::Video],
            author_name: "RainbowBird | 洛灵".to_string(),
            author_handle: "alice".to_string(),
            text: "https://t.co/ac3XPxNKAb".to_string(),
            created_at: "Wed Oct 10 20:19:24 +0000 2018".to_string(),
            url: format!("https://x.com/alice/status/{id}"),
            links: vec![LinkPreview {
                title: Some(format!("Article {id}")),
                url: "https://t.co/ac3XPxNKAb".to_string(),
                expanded_url: Some(format!("https://x.com/i/article/{id}")),
            }],
            metrics: TweetMetrics {
                bookmarks: Some(10),
                likes: Some(20),
                replies: Some(30),
                views: Some(40),
                reposts: Some(50),
                quotes: Some(60),
            },
            media: Vec::new(),
            local_media: Vec::new(),
        }
    }

    fn temporary_path(extension: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "clix-x-bookmarks-{}-{unique}-{seq}.{extension}",
            std::process::id()
        ))
    }

    #[test]
    fn extraction_uses_content_type_and_non_exclusive_subtypes() {
        let article = extract_tweet(&tweet_fixture(json!({
            "article": {"article_results": {"result": {}}},
            "quoted_status_result": {"result": {"rest_id": "987654321"}},
            "card": {"legacy": {"name": "poll2choice_text_only"}},
            "legacy": {
                "full_text": "Article with several subtypes",
                "created_at": "Wed Oct 10 20:19:24 +0000 2018",
                "is_quote_status": true,
                "in_reply_to_status_id_str": "987654321",
                "extended_entities": {
                    "media": [
                        {
                            "type": "photo",
                            "media_url_https": "https://pbs.twimg.com/media/photo.jpg"
                        },
                        {
                            "type": "video",
                            "media_url_https": "https://pbs.twimg.com/media/video.jpg"
                        },
                        {
                            "type": "animated_gif",
                            "media_url_https": "https://pbs.twimg.com/media/gif.jpg"
                        }
                    ]
                }
            }
        })))
        .expect("article should be extracted");
        assert_eq!(article.content_type, ContentType::Article);
        assert_eq!(
            article.subtypes,
            vec![
                ContentSubtype::Quoted,
                ContentSubtype::RepliedTo,
                ContentSubtype::Photo,
                ContentSubtype::Video,
                ContentSubtype::AnimatedGif,
                ContentSubtype::Poll,
            ]
        );

        let note_tweet = extract_tweet(&tweet_fixture(json!({
            "note_tweet": {"note_tweet_results": {"result": {"text": "Long post"}}}
        })))
        .expect("note tweet should be extracted");
        assert_eq!(note_tweet.content_type, ContentType::NoteTweet);

        let post =
            extract_tweet(&tweet_fixture(json!({}))).expect("plain tweet should be extracted");
        assert_eq!(post.content_type, ContentType::Post);
        assert!(post.subtypes.is_empty());
    }

    #[test]
    fn extraction_collects_available_engagement_metrics() {
        let bookmark = extract_tweet(&tweet_fixture(json!({
            "views": {"count": "600"},
            "legacy": {
                "full_text": "Measured post",
                "created_at": "Wed Oct 10 20:19:24 +0000 2018",
                "bookmark_count": 100,
                "favorite_count": 200,
                "reply_count": 300,
                "retweet_count": 400,
                "quote_count": 500
            }
        })))
        .expect("measured post should be extracted");

        assert_eq!(
            bookmark.metrics,
            TweetMetrics {
                bookmarks: Some(100),
                likes: Some(200),
                replies: Some(300),
                views: Some(600),
                reposts: Some(400),
                quotes: Some(500),
            }
        );
    }

    #[test]
    fn extraction_builds_an_article_title_link_from_the_short_url() {
        let bookmark = extract_tweet(&tweet_fixture(json!({
            "article": {
                "article_results": {
                    "result": {
                        "title": "A useful long post"
                    }
                }
            },
            "legacy": {
                "full_text": "https://t.co/ac3XPxNKAb",
                "created_at": "Wed Oct 10 20:19:24 +0000 2018",
                "entities": {
                    "urls": [{
                        "url": "https://t.co/ac3XPxNKAb",
                        "expanded_url": "https://x.com/i/article/2078674233731985408"
                    }]
                }
            }
        })))
        .expect("article should be extracted");

        assert_eq!(bookmark.content_type, ContentType::Article);
        assert_eq!(
            bookmark.links,
            vec![LinkPreview {
                title: Some("A useful long post".to_string()),
                url: "https://t.co/ac3XPxNKAb".to_string(),
                expanded_url: Some("https://x.com/i/article/2078674233731985408".to_string()),
            }]
        );
    }

    #[test]
    fn expanded_x_article_url_classifies_a_post_even_without_a_title() {
        let bookmark = extract_tweet(&tweet_fixture(json!({
            "legacy": {
                "full_text": "https://t.co/inS2sFFa5b",
                "created_at": "Tue Jul 14 03:03:13 +0000 2026",
                "entities": {
                    "urls": [{
                        "url": "https://t.co/inS2sFFa5b",
                        "expanded_url": "http://x.com/i/article/2076861011957800960"
                    }]
                }
            }
        })))
        .expect("article seed post should be extracted");

        assert_eq!(bookmark.content_type, ContentType::Article);
        assert_eq!(
            bookmark.links,
            vec![LinkPreview {
                title: None,
                url: "https://t.co/inS2sFFa5b".to_string(),
                expanded_url: Some("https://x.com/i/article/2076861011957800960".to_string()),
            }]
        );
    }

    #[test]
    fn note_tweet_entities_are_preserved_in_media_links() {
        let bookmark = extract_tweet(&tweet_fixture(json!({
            "note_tweet": {
                "note_tweet_results": {
                    "result": {
                        "text": "Read https://t.co/note后续",
                        "entity_set": {
                            "urls": [{
                                "url": "https://t.co/note",
                                "expanded_url": "https://example.com/note"
                            }]
                        }
                    }
                }
            }
        })))
        .expect("NoteTweet should be extracted");

        assert_eq!(bookmark.content_type, ContentType::NoteTweet);
        assert_eq!(bookmark.links.len(), 1);
        assert_eq!(
            bookmark.links[0].expanded_url.as_deref(),
            Some("https://example.com/note")
        );
        assert_eq!(format_tweet_cell(&bookmark, false), "Read 后续");

        let output = render_output(&[bookmark], OutputFormat::Markdown, false)
            .expect("Markdown should render");
        assert!(output.contains("[Open Link](https://example.com/note)"));
    }

    #[test]
    fn null_timeline_is_not_accepted_as_a_successful_response() {
        assert!(!has_bookmark_timeline(&json!({
            "data": {"bookmark_timeline_v2": {"timeline": {"instructions": null}}}
        })));
        assert!(has_bookmark_timeline(&json!({
            "data": {"bookmark_timeline_v2": {"timeline": {"instructions": []}}}
        })));
    }

    #[test]
    fn bookmark_timeline_modules_and_bottom_cursors_are_parsed() {
        let response = json!({
            "data": {
                "bookmark_timeline_v2": {
                    "timeline": {
                        "instructions": [{
                            "entries": [
                                {
                                    "entryId": "module-1",
                                    "content": {
                                        "items": [{
                                            "item": {
                                                "itemContent": {
                                                    "tweet_results": {
                                                        "result": tweet_fixture(json!({}))
                                                    }
                                                }
                                            }
                                        }]
                                    }
                                },
                                {
                                    "entryId": "cursor-bottom-1",
                                    "content": {
                                        "cursorType": "Bottom",
                                        "value": "next-page"
                                    }
                                }
                            ]
                        }]
                    }
                }
            }
        });

        let (tweets, cursor) = parse_bookmarks_response(&response);
        assert_eq!(tweets.len(), 1);
        assert_eq!(tweets[0].id, "123456789");
        assert_eq!(cursor.as_deref(), Some("next-page"));
    }

    #[test]
    fn article_tweet_cell_has_plain_title_or_placeholder_without_links() {
        let mut bookmark = bookmark_fixture("2078674233731985408");
        bookmark.links[0].title =
            Some("Article 2078674233731985408 https://example.com/source".to_string());
        let titled = format_tweet_cell(&bookmark, false);
        let link_only = format_tweet_cell(&bookmark, true);

        assert_eq!(titled, "Article 2078674233731985408");
        assert!(!titled.contains("https://"));
        assert!(!titled.contains("t.co"));
        assert!(!titled.contains("]("));
        assert_eq!(link_only, "-");
    }

    #[test]
    fn tweet_cell_removes_all_urls_but_preserves_article_commentary() {
        let mut bookmark = bookmark_fixture("2078674233731985408");
        bookmark.text =
            "My commentary https://t.co/ac3XPxNKAb and https://t.co/media123".to_string();
        bookmark.links.push(LinkPreview {
            title: Some("Second link".to_string()),
            url: "https://t.co/second".to_string(),
            expanded_url: Some("https://example.com/second".to_string()),
        });

        assert_eq!(
            format_tweet_cell(&bookmark, false),
            "Article 2078674233731985408<br/>My commentary and"
        );
        assert_eq!(format_tweet_cell(&bookmark, true), "My commentary and");

        let output = render_output(&[bookmark], OutputFormat::Markdown, false)
            .expect("Markdown should render");
        assert!(!output.contains("t.co"));
        assert!(output.contains(
            "[Article 2078674233731985408](https://x.com/i/article/2078674233731985408)"
        ));
        assert!(output.contains("[Open Link](https://example.com/second)"));
    }

    #[test]
    fn tweet_cell_deactivates_markdown_and_preserves_text_after_urls() {
        let mut bookmark = bookmark_fixture("2078674233731985408");
        bookmark.links[0].title = None;
        bookmark.text =
            "See [local docs](guide.md), HTTPS://t.co/path后续 and www.example.com.".to_string();

        assert_eq!(
            format_tweet_cell(&bookmark, false),
            "See local docs, 后续 and."
        );
    }

    #[test]
    fn partial_media_downloads_keep_remote_fallbacks() {
        let mut bookmark = bookmark_fixture("100");
        bookmark.media = vec![
            "https://pbs.twimg.com/media/first.jpg".into(),
            "https://pbs.twimg.com/media/second.png".into(),
        ];
        bookmark.local_media = vec!["./media/alice_100_2.png".into()];
        bookmark.links.push(LinkPreview {
            title: None,
            url: "https://t.co/special".into(),
            expanded_url: Some("https://example.com/a_(b)".into()),
        });

        let output = render_output(&[bookmark], OutputFormat::Markdown, false)
            .expect("Markdown should render");
        assert!(output.contains("https://pbs.twimg.com/media/first.jpg"));
        assert!(output.contains("./media/alice_100_2.png"));
        assert!(!output.contains("https://pbs.twimg.com/media/second.png"));
        assert!(output.contains("https://example.com/a_%28b%29"));
    }

    #[test]
    fn referenced_status_links_do_not_pollute_incremental_ids_or_total() {
        let mut bookmark = bookmark_fixture("100");
        bookmark.links.push(LinkPreview {
            title: None,
            url: "https://t.co/reference".to_string(),
            expanded_url: Some("https://x.com/other/status/999".to_string()),
        });
        let output = render_output(&[bookmark], OutputFormat::Markdown, false)
            .expect("Markdown should render");

        let (ids, row_count) = markdown_status_ids_and_count(&output);
        assert_eq!(ids, HashSet::from(["100".to_string()]));
        assert_eq!(row_count, 1);
    }

    #[test]
    fn output_and_state_paths_must_be_distinct() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let output = directory.path().join("bookmarks.json");

        assert!(ensure_distinct_paths(&output, &output).is_err());
        assert_ne!(
            clix_core::settings::config_dir().join("bookmarks.redb"),
            output,
            "the default redb state path must never collide with the output"
        );
    }

    #[test]
    fn markdown_contains_types_original_time_and_metrics() {
        let bookmark = bookmark_fixture("123456789");
        let path = temporary_path("md");
        let json = serde_json::to_value(&bookmark).expect("bookmark should serialize");

        write_output(&[bookmark], &path, OutputFormat::Markdown, false)
            .expect("Markdown output should be written");
        let output = std::fs::read_to_string(&path).expect("Markdown output should be readable");
        std::fs::remove_file(path).expect("temporary Markdown output should be removable");

        assert!(output.contains(
            "| Content Type | Subtypes | Author | Published At | Tweet | Media / Links | Bookmarks | Likes | Replies | Views | Reposts | Quotes |"
        ));
        assert!(output.contains("| `article` | `quoted`, `video` |"));
        assert!(output.contains("RainbowBird \\| 洛灵 (@alice)"));
        assert!(output.contains("`2018-10-10 20:19:24`"));
        assert!(output.contains("| Article 123456789 |"));
        assert!(output.contains("[Article 123456789](https://x.com/i/article/123456789)"));
        assert!(!output.contains("[https://t.co/ac3XPxNKAb]"));
        assert!(output.contains("| 10 | 20 | 30 | 40 | 50 | 60 |"));
        assert_eq!(json["content_type"], "article");
        assert_eq!(json["subtypes"], json!(["quoted", "video"]));
    }

    #[test]
    fn incremental_markdown_prepends_new_rows_updates_total_and_deduplicates() {
        let old = bookmark_fixture("100");
        let new = bookmark_fixture("200");
        let path = temporary_path("md");
        write_output(
            std::slice::from_ref(&old),
            &path,
            OutputFormat::Markdown,
            false,
        )
        .expect("baseline should be written");

        let existing =
            ExistingOutput::load(&path, OutputFormat::Markdown).expect("baseline should load");
        let known = HashSet::from([old.id]);
        append_output(
            std::slice::from_ref(&new),
            &path,
            OutputFormat::Markdown,
            false,
            &known,
            Some(&existing),
        )
        .expect("new row should be appended");
        let existing =
            ExistingOutput::load(&path, OutputFormat::Markdown).expect("merged output should load");
        append_output(
            std::slice::from_ref(&new),
            &path,
            OutputFormat::Markdown,
            false,
            &HashSet::new(),
            Some(&existing),
        )
        .expect("repeated row should be ignored");

        let output = std::fs::read_to_string(&path).expect("merged output should be readable");
        std::fs::remove_file(path).expect("temporary output should be removable");
        assert!(output.contains("Total: 2 bookmarks"));
        assert!(
            output.find("/status/200").expect("new row exists")
                < output.find("/status/100").expect("old row exists")
        );
        assert_eq!(output.matches("/status/200").count(), 1);
        assert_eq!(
            markdown_status_ids_and_count(&output).0,
            HashSet::from(["100".into(), "200".into()])
        );
    }

    #[test]
    fn incremental_markdown_migrates_legacy_tweet_links_without_new_rows() {
        let bookmark = bookmark_fixture("100");
        let path = temporary_path("md");
        write_output(
            std::slice::from_ref(&bookmark),
            &path,
            OutputFormat::Markdown,
            false,
        )
        .expect("baseline should be written");

        let legacy = std::fs::read_to_string(&path)
            .expect("baseline should be readable")
            .replace(
                "| Article 100 |",
                "| [Article 100](https://x.com/i/article/100) https://t.co/old后续 |",
            );
        std::fs::write(&path, legacy).expect("legacy fixture should be written");

        let existing =
            ExistingOutput::load(&path, OutputFormat::Markdown).expect("legacy output should load");
        append_output(
            &[],
            &path,
            OutputFormat::Markdown,
            false,
            &HashSet::from(["100".to_string()]),
            Some(&existing),
        )
        .expect("legacy rows should be migrated");

        let migrated = std::fs::read_to_string(&path).expect("migrated output should be readable");
        let row = migrated
            .lines()
            .find(|line| line.contains("[View Status]"))
            .expect("bookmark row should remain");
        let cells = row.split(" | ").collect::<Vec<_>>();
        assert_eq!(cells[4], "Article 100 后续");
        assert!(!cells[4].contains("]("));
        assert!(!cells[4].contains("http"));

        let loaded = ExistingOutput::load(&path, OutputFormat::Markdown)
            .expect("migrated output should load");
        append_output(
            &[],
            &path,
            OutputFormat::Markdown,
            false,
            &HashSet::from(["100".to_string()]),
            Some(&loaded),
        )
        .expect("migration should be idempotent");
        assert_eq!(
            std::fs::read_to_string(&path).expect("output should remain readable"),
            migrated
        );
        std::fs::remove_file(path).expect("temporary output should be removable");
    }

    #[test]
    fn existing_export_bootstraps_state_and_state_round_trips() {
        let bookmark = bookmark_fixture("2076865068613206046");
        let output_path = temporary_path("md");
        let state_path = temporary_path("redb");
        write_output(
            std::slice::from_ref(&bookmark),
            &output_path,
            OutputFormat::Markdown,
            false,
        )
        .expect("baseline should be written");

        let existing =
            ExistingOutput::load(&output_path, OutputFormat::Markdown).expect("output should load");
        let output_ids = existing.ids().clone();
        assert_eq!(output_ids, HashSet::from([bookmark.id]));
        assert_eq!(
            existing.article_titles(),
            BTreeMap::from([(
                "2076865068613206046".to_string(),
                "Article 2076865068613206046".to_string()
            )])
        );

        let titles = existing.article_titles();
        let db = BookmarksDb::open(&state_path).expect("db should open");
        let entries: Vec<(&str, &str)> = output_ids
            .iter()
            .map(|id| (id.as_str(), titles.get(id).map_or("", String::as_str)))
            .collect();
        db.upsert(entries).expect("state should be written");
        let snapshot = db.snapshot().expect("snapshot should load");
        assert_eq!(snapshot.known_ids, output_ids);
        assert_eq!(snapshot.article_titles, titles);
        std::fs::remove_file(output_path).expect("temporary output should be removable");
        std::fs::remove_file(state_path).expect("temporary state should be removable");
    }

    #[test]
    fn generic_article_link_does_not_cache_commentary_as_a_title() {
        let mut bookmark = bookmark_fixture("2076865068613206046");
        bookmark.text = "Author commentary".to_string();
        let output_path = temporary_path("md");
        write_output(
            std::slice::from_ref(&bookmark),
            &output_path,
            OutputFormat::Markdown,
            true,
        )
        .expect("link-only output should be written");

        let existing =
            ExistingOutput::load(&output_path, OutputFormat::Markdown).expect("output should load");
        assert!(existing.article_titles().is_empty());
        std::fs::remove_file(output_path).expect("temporary output should be removable");
    }

    #[test]
    fn ordinary_external_preview_is_not_an_article_cache_target() {
        let mut bookmark = bookmark_fixture("2076865068613206046");
        bookmark.content_type = ContentType::Post;
        bookmark.links[0].expanded_url = Some("https://example.com/story".to_string());

        assert_eq!(article_link_index(&bookmark), None);
    }

    #[test]
    fn state_title_takes_precedence_over_the_rendered_title() {
        let bookmark = bookmark_fixture("2076865068613206046");
        let output_path = temporary_path("md");
        let state_path = temporary_path("redb");
        write_output(
            std::slice::from_ref(&bookmark),
            &output_path,
            OutputFormat::Markdown,
            false,
        )
        .expect("output should be written");

        let db = BookmarksDb::open(&state_path).expect("db should open");
        db.upsert(std::iter::once((bookmark.id.as_str(), "Fresh API title")))
            .expect("state should be written");

        let loaded_titles = db.snapshot().expect("snapshot should load").article_titles;
        assert_eq!(
            loaded_titles.get(&bookmark.id).map(String::as_str),
            Some("Fresh API title")
        );
        std::fs::remove_file(output_path).expect("temporary output should be removable");
        std::fs::remove_file(state_path).expect("temporary state should be removable");
    }

    #[test]
    fn incremental_boundary_requires_a_full_known_page() {
        let known = HashSet::from(["100".to_string(), "200".to_string()]);
        let fully_known = vec![bookmark_fixture("100"), bookmark_fixture("200")];
        let mixed = vec![bookmark_fixture("200"), bookmark_fixture("300")];

        assert!(page_is_known_boundary(&fully_known, true, &known));
        assert!(!page_is_known_boundary(&mixed, true, &known));
        assert!(!page_is_known_boundary(&fully_known, false, &known));
        assert!(!page_is_known_boundary(&[], true, &known));
    }

    #[test]
    fn export_lock_rejects_concurrent_updates_and_releases_on_drop() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let output = directory.path().join("bookmarks.md");

        let first = acquire_export_lock(&output).expect("first export should acquire its lock");
        assert!(
            acquire_export_lock(&output).is_err(),
            "a concurrent exporter must not overwrite an in-progress output"
        );
        drop(first);
        acquire_export_lock(&output).expect("the lock should be released when the exporter exits");
    }
    #[test]
    fn legacy_json_state_migrates_into_redb() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let output_path = directory.path().join("x_bookmarks.md");
        let legacy_path = output_path.with_extension("state.json");
        let db_path = directory.path().join("test.redb");

        // Write a legacy v1 JSON sidecar next to the (absent) output file.
        let legacy_json = serde_json::json!({
            "version": STATE_VERSION,
            "last_successful_sync": "2024-01-01T00:00:00+00:00",
            "seen_tweet_ids": ["111", "222", "333"],
            "article_titles": {"222": "Title Two"}
        });
        std::fs::write(&legacy_path, legacy_json.to_string())
            .expect("legacy state should be written");

        let db = BookmarksDb::open(&db_path).expect("db should open");
        assert!(db.snapshot().expect("initial snapshot").is_empty());

        let migrated = migrate_legacy_state(&db, &output_path).expect("migration should succeed");
        assert!(migrated, "migration should report success");

        let snapshot = db.snapshot().expect("migrated snapshot");
        assert_eq!(
            snapshot.known_ids,
            HashSet::from(["111".to_string(), "222".to_string(), "333".to_string()])
        );
        assert_eq!(
            snapshot.article_titles.get("222").map(String::as_str),
            Some("Title Two")
        );

        // The legacy JSON must be renamed to .bak and no longer present.
        assert!(!legacy_path.exists(), "legacy file should be renamed");
        assert!(
            PathBuf::from(format!("{}.bak", legacy_path.display())).exists(),
            "legacy file should be backed up"
        );
    }
}
