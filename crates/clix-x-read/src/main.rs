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

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let cli = StandaloneCli::parse();
    ui::exit_code(clix_x_read::run(cli.args).await)
}
