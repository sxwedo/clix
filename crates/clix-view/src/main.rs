use clap::Parser;
use clix_core::ui;
use clix_view::ViewArgs;

#[derive(Debug, Parser)]
#[command(
    name = "clix-view",
    author,
    version,
    about = "🖼️ View Markdown files directly in the terminal with inline rendered images."
)]
struct StandaloneCli {
    #[command(flatten)]
    args: ViewArgs,
}

#[tokio::main]
async fn main() {
    let cli = StandaloneCli::parse();
    if let Err(err) = clix_view::run(cli.args).await {
        ui::error(&err);
        std::process::exit(1);
    }
}
