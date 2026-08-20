use std::{fmt, path::PathBuf};

use anyhow::{Result, bail};
use clap::{Args, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReadSource {
    X,
    #[value(alias = "wx")]
    Wechat,
}

impl fmt::Display for ReadSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::X => "x",
            Self::Wechat => "wechat",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReadOutputFormat {
    Markdown,
    Mdx,
    Json,
}

#[derive(Debug, Args)]
pub struct ReadArgs {
    /// X status or `WeChat` article URL; use --source for a bare ID
    pub url_or_id: String,

    /// Content source override for a bare ID (x or wechat; wx is accepted as an alias)
    #[arg(long, value_enum)]
    pub source: Option<ReadSource>,

    /// Output path (default: `<author>:<title>.<format>`)
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Output format: markdown, mdx, or json
    #[arg(short, long, value_enum, default_value_t = ReadOutputFormat::Markdown)]
    pub format: ReadOutputFormat,

    /// Skip downloading media images locally into a `media/` folder
    #[arg(long)]
    pub no_media: bool,

    /// Include author and thread replies (X only)
    #[arg(long)]
    pub include_replies: bool,

    /// X `auth_token` cookie (X only; or use config/environment variables)
    #[arg(long)]
    pub auth_token: Option<String>,

    /// X `ct0` (CSRF) cookie (X only; or use config/environment variables)
    #[arg(long)]
    pub ct0: Option<String>,
}

/// Detect the content source, then download and render it through the matching reader.
///
/// # Errors
///
/// Returns an error for unsupported or ambiguous inputs, source-specific options used with
/// another source, configuration failures, network failures, or output write failures.
pub async fn run(args: ReadArgs) -> Result<()> {
    let source = resolve_source(&args.url_or_id, args.source)?;
    validate_source_options(source, &args)?;
    let url_or_id = normalize_input(source, args.url_or_id);

    match source {
        ReadSource::X => {
            let settings = clix_core::settings::Settings::load()?;
            clix_x_read::run(
                clix_x_read::ReadArgs {
                    url_or_id,
                    auth_token: args.auth_token,
                    ct0: args.ct0,
                    output: args.output,
                    format: x_output_format(args.format),
                    no_media: args.no_media,
                    include_replies: args.include_replies,
                },
                &settings,
            )
            .await
        }
        ReadSource::Wechat => {
            clix_wx_read::run(clix_wx_read::ReadArgs {
                url_or_id,
                output: args.output,
                format: wechat_output_format(args.format),
                no_media: args.no_media,
            })
            .await
        }
    }
}

fn normalize_input(source: ReadSource, input: String) -> String {
    if source == ReadSource::X
        && !input.contains("://")
        && ["x.com/", "www.x.com/", "twitter.com/", "www.twitter.com/"]
            .iter()
            .any(|prefix| input.starts_with(prefix))
    {
        format!("https://{input}")
    } else {
        input
    }
}

fn resolve_source(input: &str, explicit: Option<ReadSource>) -> Result<ReadSource> {
    let inferred = infer_source(input)?;
    match (explicit, inferred) {
        (Some(explicit), Some(inferred)) if explicit != inferred => bail!(
            "source `{explicit}` conflicts with the URL, which identifies `{inferred}` content"
        ),
        (Some(explicit), _) => Ok(explicit),
        (None, Some(inferred)) => Ok(inferred),
        (None, None) => bail!(
            "cannot infer a content source from `{}`; pass a supported URL or add `--source x|wechat` for a bare ID",
            input.trim()
        ),
    }
}

fn infer_source(input: &str) -> Result<Option<ReadSource>> {
    let input = input.trim();
    if input.is_empty() {
        bail!("content URL or ID cannot be empty");
    }

    if input.starts_with("s/") {
        return Ok(Some(ReadSource::Wechat));
    }

    let url = if input.contains("://") {
        Some(
            reqwest::Url::parse(input)
                .map_err(|error| anyhow::anyhow!("invalid content URL `{input}`: {error}"))?,
        )
    } else if starts_with_known_host(input) {
        Some(
            reqwest::Url::parse(&format!("https://{input}"))
                .map_err(|error| anyhow::anyhow!("invalid content URL `{input}`: {error}"))?,
        )
    } else {
        None
    };

    let Some(url) = url else {
        return Ok(None);
    };
    let Some(host) = url.host_str() else {
        bail!("content URL `{input}` has no host");
    };

    match host.to_ascii_lowercase().as_str() {
        "x.com" | "www.x.com" | "twitter.com" | "www.twitter.com" => Ok(Some(ReadSource::X)),
        "mp.weixin.qq.com" => Ok(Some(ReadSource::Wechat)),
        _ => bail!(
            "unsupported content URL host `{host}`; supported sources are x.com, twitter.com, and mp.weixin.qq.com"
        ),
    }
}

fn starts_with_known_host(input: &str) -> bool {
    [
        "x.com/",
        "www.x.com/",
        "twitter.com/",
        "www.twitter.com/",
        "mp.weixin.qq.com/",
    ]
    .iter()
    .any(|prefix| input.starts_with(prefix))
}

fn validate_source_options(source: ReadSource, args: &ReadArgs) -> Result<()> {
    if source == ReadSource::Wechat {
        let mut x_options = Vec::new();
        if args.include_replies {
            x_options.push("--include-replies");
        }
        if args.auth_token.is_some() {
            x_options.push("--auth-token");
        }
        if args.ct0.is_some() {
            x_options.push("--ct0");
        }
        if !x_options.is_empty() {
            bail!(
                "{} can only be used when reading X content",
                x_options.join(", ")
            );
        }
    }
    Ok(())
}

const fn x_output_format(format: ReadOutputFormat) -> clix_x_read::ReadOutputFormat {
    match format {
        ReadOutputFormat::Markdown => clix_x_read::ReadOutputFormat::Markdown,
        ReadOutputFormat::Mdx => clix_x_read::ReadOutputFormat::Mdx,
        ReadOutputFormat::Json => clix_x_read::ReadOutputFormat::Json,
    }
}

const fn wechat_output_format(format: ReadOutputFormat) -> clix_wx_read::ReadOutputFormat {
    match format {
        ReadOutputFormat::Markdown => clix_wx_read::ReadOutputFormat::Markdown,
        ReadOutputFormat::Mdx => clix_wx_read::ReadOutputFormat::Mdx,
        ReadOutputFormat::Json => clix_wx_read::ReadOutputFormat::Json,
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{
        ReadArgs, ReadOutputFormat, ReadSource, normalize_input, resolve_source,
        validate_source_options,
    };

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(flatten)]
        args: ReadArgs,
    }

    #[test]
    fn detects_supported_url_sources() {
        for input in [
            "https://x.com/alice/status/123",
            "https://www.twitter.com/alice/status/123",
            "x.com/alice/status/123",
        ] {
            assert_eq!(resolve_source(input, None).unwrap(), ReadSource::X);
        }

        for input in [
            "https://mp.weixin.qq.com/s/abcdef",
            "mp.weixin.qq.com/s/abcdef",
            "s/abcdef",
        ] {
            assert_eq!(resolve_source(input, None).unwrap(), ReadSource::Wechat);
        }
    }

    #[test]
    fn bare_ids_require_an_explicit_source() {
        let error = resolve_source("123456", None).unwrap_err().to_string();
        assert!(error.contains("--source x|wechat"));
        assert_eq!(
            resolve_source("123456", Some(ReadSource::X)).unwrap(),
            ReadSource::X
        );
        assert_eq!(
            resolve_source("abcdef", Some(ReadSource::Wechat)).unwrap(),
            ReadSource::Wechat
        );
    }

    #[test]
    fn rejects_source_conflicts_and_unknown_hosts() {
        let conflict = resolve_source("https://x.com/alice/status/123", Some(ReadSource::Wechat))
            .unwrap_err()
            .to_string();
        assert!(conflict.contains("conflicts with the URL"));

        let unsupported = resolve_source("https://example.com/article", None)
            .unwrap_err()
            .to_string();
        assert!(unsupported.contains("unsupported content URL host"));
    }

    #[test]
    fn parses_common_and_source_specific_options() {
        let cli = TestCli::try_parse_from([
            "test",
            "https://x.com/alice/status/123",
            "--format",
            "mdx",
            "--output",
            "article.mdx",
            "--no-media",
            "--include-replies",
            "--auth-token",
            "secret",
            "--ct0",
            "csrf",
        ])
        .unwrap();

        assert_eq!(cli.args.format, ReadOutputFormat::Mdx);
        assert!(cli.args.no_media);
        assert!(cli.args.include_replies);
        assert_eq!(cli.args.auth_token.as_deref(), Some("secret"));
        assert_eq!(cli.args.ct0.as_deref(), Some("csrf"));
    }

    #[test]
    fn accepts_wx_as_a_source_alias() {
        let cli = TestCli::try_parse_from(["test", "abcdef", "--source", "wx"]).unwrap();
        assert_eq!(cli.args.source, Some(ReadSource::Wechat));
    }

    #[test]
    fn normalizes_scheme_less_x_urls_before_dispatch() {
        assert_eq!(
            normalize_input(ReadSource::X, "x.com/alice/status/123".to_string()),
            "https://x.com/alice/status/123"
        );
        assert_eq!(normalize_input(ReadSource::X, "123".to_string()), "123");
    }

    #[test]
    fn rejects_x_only_options_for_wechat() {
        let args = TestCli::try_parse_from([
            "test",
            "https://mp.weixin.qq.com/s/abcdef",
            "--include-replies",
        ])
        .unwrap()
        .args;
        let error = validate_source_options(ReadSource::Wechat, &args)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--include-replies"));
    }
}
