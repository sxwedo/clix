use anyhow::Result;
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let cli = StandaloneCli::parse();
    ui::exit_code(try_main(cli).await)
}

async fn try_main(cli: StandaloneCli) -> Result<()> {
    let settings = clix_core::settings::Settings::load()?;
    clix_gh_stars::run(cli.args, &settings).await
}
