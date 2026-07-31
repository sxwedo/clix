use anyhow::Result;
use clap::Parser;
use clix_core::ui;
use clix_rss_sync::SyncArgs;

#[derive(Debug, Parser)]
#[command(
    name = "clix-rss-sync",
    author,
    version,
    about = "🔄 Incrementally sync configured RSS subscriptions into redb."
)]
struct StandaloneCli {
    #[command(flatten)]
    args: SyncArgs,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let cli = StandaloneCli::parse();
    ui::exit_code(try_main(cli).await)
}

async fn try_main(cli: StandaloneCli) -> Result<()> {
    let settings = clix_core::settings::Settings::load()?;
    clix_rss_sync::run(cli.args, &settings).await
}
