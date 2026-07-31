use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

use clix_core::settings::{RssFeedSettings, RssSettings};
use clix_rss_store::{EntryQuery, RssStore};

use crate::{SyncArgs, run, select_push_destinations};

const RSS_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Local News</title>
    <link>https://example.com/</link>
    <description>Local fixture</description>
    <item>
      <guid>entry-one</guid>
      <title>Entry One</title>
      <link>https://example.com/one</link>
      <description>Summary.</description>
      <pubDate>Thu, 31 Jul 2025 10:00:00 GMT</pubDate>
    </item>
  </channel>
</rss>"#;

#[tokio::test(flavor = "current_thread")]
async fn configured_subscription_syncs_into_redb_and_deduplicates_next_poll() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test listener");
    let address = listener.local_addr().expect("listener address");
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("request");
            let mut request = [0_u8; 2_048];
            let _ = stream.read(&mut request).expect("read request");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                RSS_FIXTURE.len(),
                RSS_FIXTURE
            )
            .expect("write response");
        }
    });

    let directory = tempfile::tempdir().expect("temp dir");
    let state_path = directory.path().join("nested/rss.redb");
    let source_url = format!("http://{address}/feed.xml");
    let settings = clix_core::settings::Settings {
        rss: RssSettings {
            state: Some(state_path.clone()),
            limit: Some(10),
            feeds: vec![RssFeedSettings {
                name: "Local".to_string(),
                url: source_url.clone(),
                enabled: true,
            }],
            ..RssSettings::default()
        },
        ..clix_core::settings::Settings::default()
    };

    for _ in 0..2 {
        run(
            SyncArgs {
                feeds: Vec::new(),
                state: None,
                limit: None,
                push_to: Vec::new(),
                no_push: false,
            },
            &settings,
        )
        .await
        .expect("configured sync should succeed");
    }
    server.join().expect("server should finish");

    let store = RssStore::open(&state_path).expect("reopen synced database");
    let result = store.query(&EntryQuery::default()).expect("query entries");
    assert_eq!(result.database_entries, 1);
    let stored = &result.entries[0];
    assert_eq!(stored.source_url, source_url);
    assert_eq!(stored.subscription, "Local");
    assert_eq!(stored.entry.title, "Entry One");
}

#[test]
fn explicit_push_destinations_override_config_and_no_push_wins() {
    let mut settings = clix_core::settings::Settings::default();
    settings.rss.push_to = vec!["configured".to_string()];
    let mut args = SyncArgs {
        feeds: Vec::new(),
        state: None,
        limit: None,
        push_to: vec!["manual".to_string(), "manual".to_string()],
        no_push: false,
    };

    assert_eq!(
        select_push_destinations(&args, &settings),
        ["manual".to_string()]
    );

    args.no_push = true;
    assert!(select_push_destinations(&args, &settings).is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn local_sync_stays_committed_when_configured_push_fails() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test listener");
    let address = listener.local_addr().expect("listener address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("request");
        let mut request = [0_u8; 2_048];
        let _ = stream.read(&mut request).expect("read request");
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            RSS_FIXTURE.len(),
            RSS_FIXTURE
        )
        .expect("write response");
    });

    let directory = tempfile::tempdir().expect("temp dir");
    let state_path = directory.path().join("rss.redb");
    let source_url = format!("http://{address}/feed.xml");
    let mut settings = clix_core::settings::Settings::default();
    settings.rss.state = Some(state_path.clone());
    settings.rss.push_to = vec!["missing".to_string()];
    settings.rss.feeds.push(RssFeedSettings {
        name: "Local".to_string(),
        url: source_url,
        enabled: true,
    });

    let error = run(
        SyncArgs {
            feeds: Vec::new(),
            state: None,
            limit: None,
            push_to: Vec::new(),
            no_push: false,
        },
        &settings,
    )
    .await
    .expect_err("unknown configured destination should fail");
    server.join().expect("server should finish");

    let message = format!("{error:#}");
    assert!(message.contains("missing `[rss.destinations.missing]`"));
    assert!(message.contains("remove `missing` from `[rss].push_to`"));
    let result = RssStore::open(&state_path)
        .expect("local sync should already be committed")
        .query(&EntryQuery::default())
        .expect("query entries");
    assert_eq!(result.database_entries, 1);
}
