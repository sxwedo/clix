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

fn main() -> std::process::ExitCode {
    let cli = StandaloneCli::parse();
    ui::exit_code(clix_view::run(cli.args))
}
