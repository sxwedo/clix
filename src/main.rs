use std::future::Future;

use anyhow::Result;
use clap::{Parser, Subcommand};
use clix_core::ui;
use clix_gh_stars::StarsArgs;
use clix_view::ViewArgs;
use clix_x_bookmarks::BookmarksArgs;
use clix_x_read::ReadArgs;

#[derive(Debug, Parser)]
#[command(
    name = "clix",
    author,
    version,
    about = "⚡ CLI Extensions for daily developer superpowers.",
    long_about = "clix is a collection of fast, lightweight developer CLI tools for GitHub, social media, and developer workflows."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// GitHub developer tools & utilities
    Gh {
        #[command(subcommand)]
        command: GhCommands,
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
    /// Markdown and MDX tools
    Md {
        #[command(subcommand)]
        command: MdCommands,
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

#[derive(Debug, Subcommand)]
enum MdCommands {
    /// View a Markdown or MDX file in the terminal
    View(ViewArgs),
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
        Commands::Config { command } => run_config(command),
        Commands::Gh { command } => {
            let settings = clix_core::settings::Settings::load()?;
            run_async(async move {
                match command {
                    GhCommands::Stars(args) => clix_gh_stars::run(args, &settings).await,
                }
            })
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
        Commands::Md { command } => match command {
            MdCommands::View(args) => clix_view::run(args),
        },
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

    use super::{Cli, Commands, GhCommands, MdCommands, WxCommands};

    #[test]
    fn command_tree_is_internally_consistent() {
        Cli::command().debug_assert();
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
    fn parses_the_documented_markdown_view_invocation() {
        let cli = Cli::try_parse_from(["clix", "md", "view", "article.md"])
            .expect("documented Markdown view arguments should parse");

        assert!(matches!(
            cli.command,
            Commands::Md {
                command: MdCommands::View(args)
            } if args.path == std::path::Path::new("article.md")
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
