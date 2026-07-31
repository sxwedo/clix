use std::future::Future;

use anyhow::Result;
use clap::{Parser, Subcommand};
use clix_agent::AgentArgs;
use clix_core::ui;
use clix_gh_stars::StarsArgs;
use clix_rss_list::ListArgs;
use clix_rss_sync::SyncArgs;
use clix_x_bookmarks::BookmarksArgs;
use clix_x_read::ReadArgs;

#[derive(Debug, Parser)]
#[command(
    name = "clix",
    author,
    version,
    about = "A fast, local-first control plane for developer agents.",
    long_about = "clix discovers, inspects, and controls local developer agents while retaining focused RSS, GitHub, social, and content utilities."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Monitor and control local developer-agent processes and sessions
    Agent(AgentArgs),
    /// GitHub developer tools & utilities
    Gh {
        #[command(subcommand)]
        command: GhCommands,
    },
    /// RSS, Atom, and JSON Feed subscription tools
    Rss {
        #[command(subcommand)]
        command: RssCommands,
    },
    /// X (Twitter) tools & utilities
    X {
        #[command(subcommand)]
        command: XCommands,
    },
    /// `WeChat` Official Account tools & utilities
    Wx {
        #[command(subcommand)]
        command: WxCommands,
    },
    /// Manage clix configuration (~/.config/clix/config.toml)
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
}

#[derive(Debug, Subcommand)]
enum GhCommands {
    /// Export all starred repositories for a GitHub user (Markdown, URLs, JSON)
    Stars(StarsArgs),
}

#[derive(Debug, Subcommand)]
enum RssCommands {
    /// List newest entries stored in the local redb database
    List(ListArgs),
    /// Sync configured subscriptions into redb and configured destinations
    Sync(SyncArgs),
}

#[derive(Debug, Subcommand)]
enum XCommands {
    /// Export all bookmarked tweets for an X account (Markdown, URLs, JSON)
    Bookmarks(BookmarksArgs),
    /// Download and convert a single X status URL/ID into a local Markdown/MDX file
    Read(ReadArgs),
}
#[derive(Debug, Subcommand)]
enum WxCommands {
    /// Download and convert a `WeChat` Official Account article URL into a local Markdown/MDX file
    Read(clix_wx_read::ReadArgs),
}

#[derive(Debug, Clone, Copy, Subcommand)]
enum ConfigCommands {
    /// Create ~/.config/clix/config.toml with a commented template (mode 0600)
    Init,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    ui::exit_code(run(cli))
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Agent(args) => {
            let settings = clix_core::settings::Settings::load()?;
            clix_agent::run(args, &settings)
        }
        Commands::Config { command } => run_config(command),
        Commands::Gh { command } => {
            let settings = clix_core::settings::Settings::load()?;
            run_async(async move {
                match command {
                    GhCommands::Stars(args) => clix_gh_stars::run(args, &settings).await,
                }
            })
        }
        Commands::Rss { command } => {
            let settings = clix_core::settings::Settings::load()?;
            match command {
                RssCommands::List(args) => clix_rss_list::run(args, &settings),
                RssCommands::Sync(args) => {
                    run_async(async move { clix_rss_sync::run(args, &settings).await })
                }
            }
        }
        Commands::X { command } => {
            let settings = clix_core::settings::Settings::load()?;
            run_async(async move {
                match command {
                    XCommands::Bookmarks(args) => clix_x_bookmarks::run(args, &settings).await,
                    XCommands::Read(args) => clix_x_read::run(args, &settings).await,
                }
            })
        }
        Commands::Wx { command } => run_async(async move {
            match command {
                WxCommands::Read(args) => clix_wx_read::run(args).await,
            }
        }),
    }
}

fn run_config(command: ConfigCommands) -> Result<()> {
    match command {
        ConfigCommands::Init => {
            let path = clix_core::settings::ensure_default_config()?;
            println!(
                "Created {} (mode 0600).\nEdit it with your credentials, then run any clix command.",
                path.display()
            );
            Ok(())
        }
    }
}

fn run_async(future: impl Future<Output = Result<()>>) -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(future)
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};
    use clix_gh_stars::OutputFormat;

    use super::{Cli, Commands, GhCommands, RssCommands, WxCommands};

    #[test]
    fn command_tree_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_agent_process_and_session_commands() {
        for invocation in [
            vec!["clix", "agent", "ps", "--json"],
            vec![
                "clix",
                "agent",
                "top",
                "--interval",
                "3",
                "--iterations",
                "1",
            ],
            vec!["clix", "agent", "inspect", "codex:42"],
            vec!["clix", "agent", "logs", "session-id", "-n", "20"],
            vec!["clix", "agent", "stop", "claude:42"],
            vec!["clix", "agent", "resume", "gemini:session-id"],
        ] {
            let cli = Cli::try_parse_from(&invocation)
                .unwrap_or_else(|error| panic!("failed to parse {invocation:?}: {error}"));
            assert!(matches!(cli.command, Commands::Agent(_)));
        }
    }

    #[test]
    fn removed_rss_commands_are_rejected() {
        for command in ["fetch", "push", "export"] {
            assert!(
                Cli::try_parse_from(["clix", "rss", command]).is_err(),
                "`clix rss {command}` should no longer be exposed"
            );
        }
    }

    #[test]
    fn parses_the_documented_github_stars_invocation() {
        let cli = Cli::try_parse_from([
            "clix",
            "gh",
            "stars",
            "octocat",
            "--format",
            "json",
            "--output",
            "stars.json",
        ])
        .expect("documented GitHub Stars arguments should parse");

        assert!(matches!(
            cli.command,
            Commands::Gh {
                command: GhCommands::Stars(args)
            } if args.username.as_deref() == Some("octocat")
                && args.format == OutputFormat::Json
                && args.output.as_deref() == Some(std::path::Path::new("stars.json"))
        ));
    }

    #[test]
    fn parses_the_documented_rss_sync_invocation() {
        let cli = Cli::try_parse_from([
            "clix",
            "rss",
            "sync",
            "--feed",
            "Rust Blog",
            "--state",
            "feeds.redb",
            "--limit",
            "50",
            "--push-to",
            "news,archive",
        ])
        .expect("documented RSS sync arguments should parse");

        assert!(matches!(
            cli.command,
            Commands::Rss {
                command: RssCommands::Sync(args)
            } if args.feeds == ["Rust Blog"]
                && args.state.as_deref() == Some(std::path::Path::new("feeds.redb"))
                && args.limit == Some(50)
                && args.push_to == ["news", "archive"]
                && !args.no_push
        ));
    }

    #[test]
    fn parses_rss_sync_no_push_override() {
        let cli = Cli::try_parse_from(["clix", "rss", "sync", "--no-push"])
            .expect("RSS sync no-push override should parse");

        assert!(matches!(
            cli.command,
            Commands::Rss {
                command: RssCommands::Sync(args)
            } if args.no_push
        ));
    }

    #[test]
    fn parses_the_documented_rss_list_invocation() {
        let cli = Cli::try_parse_from([
            "clix",
            "rss",
            "list",
            "--feed",
            "Rust Blog",
            "--limit",
            "10",
        ])
        .expect("documented RSS list arguments should parse");

        assert!(matches!(
            cli.command,
            Commands::Rss {
                command: RssCommands::List(args)
            } if args.feeds == ["Rust Blog"] && args.limit == Some(10)
        ));
    }

    #[test]
    fn parses_the_documented_wechat_read_invocation() {
        let cli = Cli::try_parse_from(["clix", "wx", "read", "https://mp.weixin.qq.com/s/123456"])
            .expect("documented WeChat read arguments should parse");

        assert!(matches!(
            cli.command,
            Commands::Wx {
                command: WxCommands::Read(args)
            } if args.url_or_id == "https://mp.weixin.qq.com/s/123456"
        ));
    }
}
