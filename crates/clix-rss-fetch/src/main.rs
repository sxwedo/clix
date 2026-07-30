use anyhow::Result;
use clap::Parser;
use clix_core::ui;
use clix_rss_fetch::FetchArgs;

#[derive(Debug, Parser)]
#[command(
    name = "clix-rss-fetch",
    author,
    version,
    about = "📰 Fetch configured RSS, Atom, and JSON Feed subscriptions."
)]
struct StandaloneCli {
    #[command(flatten)]
    args: FetchArgs,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let cli = StandaloneCli::parse();
    ui::exit_code(try_main(cli).await)
}

async fn try_main(cli: StandaloneCli) -> Result<()> {
    let settings = clix_core::settings::Settings::load()?;
    clix_rss_fetch::run(cli.args, &settings).await
}
