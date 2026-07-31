//! Local-first process and session control for developer agents.

mod controller;
mod process;
mod session;
mod view;

use anyhow::Result;
use clap::{Args, Subcommand};
use clix_core::settings::Settings;
pub use process::{AgentKind, ProcessSnapshot};

/// Arguments shared by `clix agent` and the standalone `clix-agent` binary.
#[derive(Debug, Args)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub command: AgentCommand,
}

/// Local developer-agent process and session operations.
#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// List running developer-agent processes
    Ps(PsArgs),
    /// Refresh a live resource view of developer-agent processes
    Top(TopArgs),
    /// Show process and session details
    Inspect(TargetArgs),
    /// Show the tail of a local agent session log
    Logs(LogsArgs),
    /// Gracefully terminate a running agent process
    Stop(StopArgs),
    /// Resume a saved agent session with its native CLI
    Resume(ResumeArgs),
}

#[derive(Debug, Clone, Args)]
pub struct PsArgs {
    /// Emit stable machine-readable JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct TopArgs {
    /// Refresh interval in seconds
    #[arg(short, long, default_value_t = 2)]
    pub interval: u64,
    /// Stop after this many refreshes (defaults to unlimited on a terminal)
    #[arg(long)]
    pub iterations: Option<u64>,
}

#[derive(Debug, Clone, Args)]
pub struct TargetArgs {
    /// PID, provider:PID, session ID, or provider:session ID
    pub target: String,
    /// Emit stable machine-readable JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct LogsArgs {
    /// PID, provider:PID, session ID, or provider:session ID
    pub target: String,
    /// Number of records to show
    #[arg(short = 'n', long, default_value_t = 50)]
    pub lines: usize,
    /// Print original JSONL records instead of a redacted event summary
    #[arg(long)]
    pub raw: bool,
}

#[derive(Debug, Clone, Args)]
pub struct StopArgs {
    /// PID or provider:PID of the live agent to stop
    pub target: String,
    /// Send a forceful kill signal instead of a graceful termination signal
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Clone, Args)]
pub struct ResumeArgs {
    /// Session ID or provider:session ID
    pub target: String,
}

/// Execute one `clix agent` command.
///
/// # Errors
///
/// Returns an actionable error when discovery fails, a target is ambiguous, or
/// an operation is unsupported by the selected agent.
pub fn run(args: AgentArgs, settings: &Settings) -> Result<()> {
    process::validate_custom_agents(&settings.agent.custom)?;
    match args.command {
        AgentCommand::Ps(args) => controller::run_ps(&args, settings),
        AgentCommand::Top(args) => controller::run_top(&args, settings),
        AgentCommand::Inspect(args) => controller::run_inspect(&args, settings),
        AgentCommand::Logs(args) => controller::run_logs(&args, settings),
        AgentCommand::Stop(args) => controller::run_stop(&args, settings),
        AgentCommand::Resume(args) => controller::run_resume(&args, settings),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{AgentKind, ProcessSnapshot};
    use crate::process::{LiveAgent, recognize_agent};
    use crate::view::render_process_table;

    fn process(executable: &str, command: &[&str]) -> ProcessSnapshot {
        ProcessSnapshot {
            pid: 42,
            parent_pid: Some(1),
            executable: PathBuf::from(executable),
            command: command.iter().map(ToString::to_string).collect(),
            cwd: Some(PathBuf::from("/work/project")),
            started_at: 1,
            run_time: 2,
            cpu_percent: 3.0,
            memory_bytes: 4,
            status: "sleeping".to_owned(),
        }
    }

    #[test]
    fn recognizes_supported_agent_executables_without_matching_desktop_helpers() {
        let cases = [
            ("/opt/bin/claude", AgentKind::ClaudeCode),
            ("/opt/bin/codex", AgentKind::Codex),
            ("/opt/bin/gemini", AgentKind::GeminiCli),
            ("/opt/bin/opencode", AgentKind::OpenCode),
            ("/opt/bin/pi", AgentKind::Pi),
            ("/opt/bin/cursor-agent", AgentKind::Cursor),
        ];

        for (executable, expected) in cases {
            assert_eq!(
                recognize_agent(&process(executable, &[executable])),
                Some(expected),
                "failed to recognize {executable}"
            );
        }

        assert_eq!(
            recognize_agent(&process(
                "/Applications/Cursor.app/Contents/MacOS/Cursor",
                &["Cursor"]
            )),
            None
        );
        assert_eq!(
            recognize_agent(&process(
                "/Applications/ChatGPT.app/Helpers/Codex (Renderer)",
                &["Codex (Renderer)"]
            )),
            None
        );
    }

    #[test]
    fn recognizes_node_wrappers_by_package_marker() {
        assert_eq!(
            recognize_agent(&process(
                "/usr/bin/node",
                &["node", "/lib/node_modules/@anthropic-ai/claude-code/cli.js"]
            )),
            Some(AgentKind::ClaudeCode)
        );
        assert_eq!(
            recognize_agent(&process(
                "/usr/bin/node",
                &["node", "/lib/node_modules/@google/gemini-cli/dist/index.js"]
            )),
            Some(AgentKind::GeminiCli)
        );
    }

    #[test]
    fn process_table_exposes_stable_selectors_and_unknown_usage_honestly() {
        let agent = LiveAgent {
            kind: AgentKind::Codex,
            process: process("/opt/bin/codex", &["codex"]),
        };

        let rendered = render_process_table(&[agent], false);

        assert!(rendered.contains("ID"));
        assert!(rendered.contains("AGENT"));
        assert!(rendered.contains("PROJECT"));
        assert!(rendered.contains("STATUS"));
        assert!(rendered.contains("DURATION"));
        assert!(rendered.contains("TOKENS"));
        assert!(rendered.contains("COST"));
        assert!(rendered.contains("codex:42"));
        assert!(rendered.contains("project"));
        assert!(rendered.contains('-'));
    }
}
