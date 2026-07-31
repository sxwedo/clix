use anyhow::Result;
use clap::Parser;
use clix_agent::AgentArgs;

#[derive(Debug, Parser)]
#[command(
    name = "clix-agent",
    author,
    version,
    about = "Local-first process and session control for developer agents"
)]
struct Cli {
    #[command(flatten)]
    args: AgentArgs,
}

fn main() -> std::process::ExitCode {
    clix_core::ui::exit_code(execute())
}

fn execute() -> Result<()> {
    let cli = Cli::parse();
    let settings = clix_core::settings::Settings::load()?;
    clix_agent::run(cli.args, &settings)
}
