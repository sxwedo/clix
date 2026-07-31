use clix_core::settings::{RssFeedSettings, RssSettings};

use crate::{Subscription, parse_feed, select_subscriptions};

const RSS_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <channel>
    <title>Example News</title>
    <link>https://example.com/</link>
    <description>Example feed</description>
    <item>
      <guid>newer</guid>
      <title>Newer entry</title>
      <link>https://example.com/newer</link>
      <description><![CDATA[<p>A <strong>new</strong> summary.</p>]]></description>
      <dc:creator>Ada</dc:creator>
      <pubDate>Wed, 30 Jul 2025 10:00:00 GMT</pubDate>
    </item>
    <item>
      <guid>older</guid>
      <title>Older entry</title>
      <link>https://example.com/older</link>
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
fn parses_and_limits_rss_through_the_shared_interface() {
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
}

#[test]
fn parses_atom_through_the_shared_interface() {
    let feed = parse_feed(
        &subscription("Atom", "https://example.net/feed"),
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
fn sanitizes_active_html_before_normalizing_summaries() {
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
fn subscription_selection_honors_enabled_names_and_rejects_duplicates() {
    let settings = RssSettings {
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
        ..RssSettings::default()
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
