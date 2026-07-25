use clap::Parser;
use clix_core::ui;
use clix_x_read::ReadArgs;

#[derive(Debug, Parser)]
#[command(
    name = "clix-x-read",
    author,
    version,
    about = "📖 Download and convert an X status URL/ID into a standalone Markdown/MDX file with local images."
)]
struct StandaloneCli {
    #[command(flatten)]
    args: ReadArgs,
}

#[tokio::main]
async fn main() {
    let cli = StandaloneCli::parse();
    if let Err(err) = clix_x_read::run(cli.args).await {
        ui::error(&err);
        std::process::exit(1);
    }
}
