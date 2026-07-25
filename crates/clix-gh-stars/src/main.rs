use clap::Parser;
use clix_core::ui;
use clix_gh_stars::StarsArgs;

#[derive(Debug, Parser)]
#[command(
    name = "clix-gh-stars",
    author,
    version,
    about = "🌟 Export all starred repositories for a GitHub user (Markdown, URLs, JSON)."
)]
struct StandaloneCli {
    #[command(flatten)]
    args: StarsArgs,
}

#[tokio::main]
async fn main() {
    let cli = StandaloneCli::parse();
    if let Err(err) = clix_gh_stars::run(cli.args).await {
        ui::error(&err);
        std::process::exit(1);
    }
}
