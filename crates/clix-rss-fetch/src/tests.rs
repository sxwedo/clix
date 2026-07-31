use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    thread,
};

use clix_core::settings::{RssFeedSettings, RssSettings};
use clix_rss_api::{Subscription, parse_feed, select_subscriptions};

use crate::{
    FetchArgs, OutputFormat,
    model::RssExport,
    output::{render_markdown, resolve_format},
    run,
};

const RSS_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <channel>
    <title>Example News</title>
    <link>https://example.com/</link>
    <description>Example feed</description>
    <item>
      <guid>newer</guid>
      <title>Newer [entry]</title>
      <link>https://example.com/newer</link>
      <description><![CDATA[<p>A <strong>new</strong> summary.</p>]]></description>
      <dc:creator>Ada</dc:creator>
      <category>Rust</category>
      <pubDate>Wed, 30 Jul 2025 10:00:00 GMT</pubDate>
    </item>
    <item>
      <guid>older</guid>
      <title>Older entry</title>
      <link>https://example.com/older</link>
      <description>Older summary.</description>
      <pubDate>Tue, 29 Jul 2025 10:00:00 GMT</pubDate>
    </item>
  </channel>
</rss>"#;

const ATOM_FIXTURE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Atom News</title>
  <id>https://example.net/feed</id>
  <updated>2025-07-30T12:00:00Z</updated>
  <link href="https://example.net/" rel="alternate"/>
  <entry>
    <title>Atom entry</title>
    <id>https://example.net/posts/1</id>
    <link href="https://example.net/posts/1"/>
    <updated>2025-07-30T11:00:00Z</updated>
    <summary type="html">&lt;p&gt;Atom summary.&lt;/p&gt;</summary>
  </entry>
</feed>"#;

const UNSAFE_HTML_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Unsafe Example</title>
    <link>https://example.com/</link>
    <description>Security regression fixture</description>
    <item>
      <guid>unsafe</guid>
      <title>Unsafe entry</title>
      <description><![CDATA[
        <p>Safe summary.</p>
        <script>alert("unsafe")</script>
        <a href="javascript:alert('unsafe')">unsafe link</a>
      ]]></description>
    </item>
  </channel>
</rss>"#;

fn subscription(name: &str, url: &str) -> Subscription {
    Subscription {
        name: name.to_string(),
        url: url.to_string(),
    }
}

#[test]
fn parses_rss_orders_entries_and_renders_safe_markdown() {
    let feed = parse_feed(
        &subscription("Example", "https://example.com/feed.xml"),
        RSS_FIXTURE.as_bytes(),
        1,
    )
    .expect("RSS should parse");
    assert_eq!(feed.title, "Example News");
    assert_eq!(feed.entries.len(), 1);
    assert_eq!(feed.entries[0].id, "newer");
    assert_eq!(feed.entries[0].authors, ["Ada"]);
    assert_eq!(
        feed.entries[0].summary.as_deref(),
        Some("A **new** summary.")
    );

    let export = RssExport {
        fetched_at: "2025-07-30T12:00:00Z".to_string(),
        feed_count: 1,
        entry_count: 1,
        feeds: vec![feed],
    };
    let markdown = render_markdown(&export).expect("Markdown should render");
    assert!(markdown.contains("### [Newer \\[entry\\]](<https://example.com/newer>)"));
    assert!(markdown.contains("> A **new** summary."));
}

#[test]
fn parses_atom_through_the_same_normalized_interface() {
    let feed = parse_feed(
        &subscription("Atom", "https://example.net/feed.xml"),
        ATOM_FIXTURE.as_bytes(),
        10,
    )
    .expect("Atom should parse");
    assert_eq!(feed.title, "Atom News");
    assert_eq!(feed.site_url.as_deref(), Some("https://example.net/"));
    assert_eq!(feed.entries[0].title, "Atom entry");
    assert_eq!(
        feed.entries[0].published_at.as_deref(),
        Some("2025-07-30T11:00:00Z")
    );
}

#[test]
fn sanitizes_active_html_before_rendering_feed_summaries() {
    let feed = parse_feed(
        &subscription("Unsafe", "https://example.com/feed.xml"),
        UNSAFE_HTML_FIXTURE.as_bytes(),
        10,
    )
    .expect("RSS should parse");
    let summary = feed.entries[0]
        .summary
        .as_deref()
        .expect("safe summary should remain");
    assert!(summary.contains("Safe summary."));
    assert!(!summary.contains("alert("));
    assert!(!summary.contains("javascript:"));
}

#[test]
fn subscription_selection_honors_enabled_names_and_validates_duplicates() {
    let settings = RssSettings {
        output: None,
        limit: None,
        feeds: vec![
            RssFeedSettings {
                name: "One".to_string(),
                url: "https://example.com/one.xml".to_string(),
                enabled: true,
            },
            RssFeedSettings {
                name: "Two".to_string(),
                url: "https://example.com/two.xml".to_string(),
                enabled: false,
            },
        ],
    };
    let selected = select_subscriptions(&settings, &["one".to_string()])
        .expect("case-insensitive selection should work");
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, "One");
    assert!(select_subscriptions(&settings, &["Two".to_string()]).is_err());

    let duplicate = RssSettings {
        feeds: vec![
            RssFeedSettings {
                name: "News".to_string(),
                url: "https://example.com/one.xml".to_string(),
                enabled: true,
            },
            RssFeedSettings {
                name: "news".to_string(),
                url: "https://example.com/two.xml".to_string(),
                enabled: true,
            },
        ],
        ..RssSettings::default()
    };
    assert!(select_subscriptions(&duplicate, &[]).is_err());
}

#[test]
fn output_format_is_inferred_from_the_path_but_explicit_format_wins() {
    assert_eq!(
        resolve_format(None, Some(Path::new("feeds.JSON"))),
        OutputFormat::Json
    );
    assert_eq!(
        resolve_format(Some(OutputFormat::Markdown), Some(Path::new("feeds.json"))),
        OutputFormat::Markdown
    );
    assert_eq!(resolve_format(None, None), OutputFormat::Markdown);
}

#[tokio::test(flavor = "current_thread")]
async fn configured_subscription_fetches_and_writes_json_end_to_end() {
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
    let output = directory.path().join("nested/rss.json");
    let settings = clix_core::settings::Settings {
        rss: RssSettings {
            output: Some(output.clone()),
            limit: Some(2),
            feeds: vec![RssFeedSettings {
                name: "Local".to_string(),
                url: format!("http://{address}/feed.xml"),
                enabled: true,
            }],
        },
        ..clix_core::settings::Settings::default()
    };
    run(
        FetchArgs {
            feeds: Vec::new(),
            output: None,
            format: None,
            limit: None,
        },
        &settings,
    )
    .await
    .expect("configured fetch should succeed");
    server.join().expect("server should finish");

    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(output).expect("JSON output should have been written"))
            .expect("valid JSON output");
    assert_eq!(value["feed_count"], 1);
    assert_eq!(value["entry_count"], 2);
    assert_eq!(value["feeds"][0]["subscription"], "Local");
}
