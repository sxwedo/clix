use clap::Parser;
use clix_core::ui;
use clix_x_bookmarks::BookmarksArgs;

#[derive(Debug, Parser)]
#[command(
    name = "clix-x-bookmarks",
    author,
    version,
    about = "🔖 Export all bookmarked tweets for an X (Twitter) account (Markdown, URLs, JSON)."
)]
struct StandaloneCli {
    #[command(flatten)]
    args: BookmarksArgs,
}

#[tokio::main]
async fn main() {
    let cli = StandaloneCli::parse();
    if let Err(err) = clix_x_bookmarks::run(cli.args).await {
        ui::error(&err);
        std::process::exit(1);
    }
}
