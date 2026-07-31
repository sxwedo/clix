use anyhow::Result;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "clix-rss-push",
    version,
    about = "Push stored RSS entries to a configured destination"
)]
struct Cli {
    #[command(flatten)]
    args: clix_rss_push::PushArgs,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    clix_core::ui::exit_code(run().await)
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let settings = clix_core::settings::Settings::load()?;
    clix_rss_push::run(cli.args, &settings).await
}
