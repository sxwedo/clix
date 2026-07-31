use anyhow::Result;
use clap::Parser;
use clix_core::ui;
use clix_rss_list::ListArgs;

#[derive(Debug, Parser)]
#[command(
    name = "clix-rss-list",
    author,
    version,
    about = "📚 List entries stored in the local RSS redb database."
)]
struct StandaloneCli {
    #[command(flatten)]
    args: ListArgs,
}

fn main() -> std::process::ExitCode {
    let cli = StandaloneCli::parse();
    ui::exit_code(try_main(cli))
}

fn try_main(cli: StandaloneCli) -> Result<()> {
    let settings = clix_core::settings::Settings::load()?;
    clix_rss_list::run(cli.args, &settings)
}
