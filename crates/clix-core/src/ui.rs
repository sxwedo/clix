use std::{
    env,
    fmt::Display,
    io::{IsTerminal, stderr, stdout},
    process::ExitCode,
};

use anstyle::{AnsiColor, Style};
use indicatif::{ProgressBar, ProgressStyle};

#[must_use]
pub fn style_bold(s: &str) -> String {
    apply_style(s, Style::new().bold(), stdout_colors_enabled())
}

#[must_use]
pub fn style_dim(s: &str) -> String {
    apply_style(s, Style::new().dimmed(), stdout_colors_enabled())
}

#[must_use]
pub fn style_green(s: &str) -> String {
    let style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)));
    apply_style(s, style, stdout_colors_enabled())
}

#[must_use]
pub fn style_yellow(s: &str) -> String {
    let style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Yellow)));
    apply_style(s, style, stdout_colors_enabled())
}

#[must_use]
pub fn style_cyan_bold(s: &str) -> String {
    let style = Style::new()
        .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Cyan)))
        .bold();
    apply_style(s, style, stdout_colors_enabled())
}

#[must_use]
pub fn style_yellow_bold(s: &str) -> String {
    let style = Style::new()
        .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Yellow)))
        .bold();
    apply_style(s, style, stdout_colors_enabled())
}

#[must_use]
pub fn style_red(s: &str) -> String {
    let style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Red)));
    apply_style(s, style, stderr_colors_enabled())
}

fn apply_style(value: &str, style: Style, enabled: bool) -> String {
    if enabled {
        format!("{style}{value}{style:#}")
    } else {
        value.to_string()
    }
}

fn colors_allowed() -> bool {
    env::var_os("NO_COLOR").is_none() && env::var_os("TERM").is_none_or(|term| term != "dumb")
}

fn stdout_colors_enabled() -> bool {
    colors_allowed() && stdout().is_terminal()
}

fn stderr_colors_enabled() -> bool {
    colors_allowed() && stderr().is_terminal()
}

pub fn info(msg: impl Display) {
    println!("  {} {msg}", style_dim("◇"));
}

pub fn success(msg: impl Display) {
    println!("  {} {msg}", style_green("✓"));
}

pub fn warn(msg: impl Display) {
    println!("  {} {msg}", style_yellow("⚠"));
}

pub fn error(msg: impl Display) {
    eprintln!("  {} {msg}", style_red("✗"));
}

/// Convert a CLI result into a conventional process exit code.
#[must_use]
pub fn exit_code(result: anyhow::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            self::error(format!("{error:#}"));
            ExitCode::FAILURE
        }
    }
}

#[must_use]
pub fn create_spinner(msg: &str) -> ProgressBar {
    const SPINNER_TEMPLATE: &str = "  {spinner:.cyan} {msg}";

    let pb = ProgressBar::new_spinner();
    let style = ProgressStyle::with_template(SPINNER_TEMPLATE)
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏");
    pb.set_style(style);
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(80));
    pb
}

#[cfg(test)]
mod tests {
    use anstyle::Style;
    use anyhow::{Context, anyhow};

    use super::apply_style;

    #[test]
    fn enabled_styles_reset_and_disabled_styles_are_plain() {
        let style = Style::new().bold();
        assert!(apply_style("value", style, true).ends_with("\u{1b}[0m"));
        assert_eq!(apply_style("value", style, false), "value");
    }

    #[test]
    fn alternate_error_format_keeps_the_context_chain() {
        let error = Err::<(), _>(anyhow!("disk full"))
            .context("failed to save output")
            .expect_err("fixture should fail");
        assert_eq!(format!("{error:#}"), "failed to save output: disk full");
    }
}
