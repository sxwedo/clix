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

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let cli = StandaloneCli::parse();
    ui::exit_code(clix_x_bookmarks::run(cli.args).await)
}
