use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

use clix_core::settings::{RssFeedSettings, RssSettings};
use clix_rss_store::{EntryQuery, RssStore};

use crate::{SyncArgs, run};

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
            output: None,
            state: Some(state_path.clone()),
            limit: Some(10),
            feeds: vec![RssFeedSettings {
                name: "Local".to_string(),
                url: source_url.clone(),
                enabled: true,
            }],
        },
        ..clix_core::settings::Settings::default()
    };

    for _ in 0..2 {
        run(
            SyncArgs {
                feeds: Vec::new(),
                state: None,
                limit: None,
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
