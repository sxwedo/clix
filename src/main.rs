use anyhow::Result;
use clap::{Parser, Subcommand};
use clix_core::ui;
use clix_gh_stars::StarsArgs;

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
}

#[derive(Debug, Subcommand)]
enum GhCommands {
    /// Export all starred repositories for a GitHub user (Markdown, URLs, JSON)
    Stars(StarsArgs),
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli).await {
        ui::error(&err);
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Gh { command } => match command {
            GhCommands::Stars(args) => clix_gh_stars::run(args).await,
        },
    }
}
