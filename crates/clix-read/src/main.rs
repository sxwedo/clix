use clap::Parser;
use clix_core::ui;
use clix_read::ReadArgs;

#[derive(Debug, Parser)]
#[command(
    name = "clix-read",
    author,
    version,
    about = "Read an X status or WeChat article into a local Markdown, MDX, or JSON document."
)]
struct StandaloneCli {
    #[command(flatten)]
    args: ReadArgs,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let cli = StandaloneCli::parse();
    ui::exit_code(clix_read::run(cli.args).await)
}
