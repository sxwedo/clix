use anyhow::Result;
use clap::Parser;
use clix_core::ui;
use clix_rss_export::ExportArgs;

#[derive(Debug, Parser)]
#[command(
    name = "clix-rss-export",
    author,
    version,
    about = "📤 Export the local RSS redb archive to Markdown or JSON."
)]
struct StandaloneCli {
    #[command(flatten)]
    args: ExportArgs,
}

fn main() -> std::process::ExitCode {
    let cli = StandaloneCli::parse();
    ui::exit_code(try_main(cli))
}

fn try_main(cli: StandaloneCli) -> Result<()> {
    let settings = clix_core::settings::Settings::load()?;
    clix_rss_export::run(cli.args, &settings)
}
