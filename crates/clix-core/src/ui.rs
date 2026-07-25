use std::fmt::Display;

use anstyle::{AnsiColor, Style};
use indicatif::{ProgressBar, ProgressStyle};

pub fn style_bold(s: &str) -> String {
    format!("{}{s}{}", Style::new().bold(), Style::new())
}

#[allow(dead_code)]
pub fn style_dim(s: &str) -> String {
    let dim = Style::new().dimmed();
    format!("{dim}{s}{}", Style::new())
}

pub fn style_green(s: &str) -> String {
    let green = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)));
    format!("{green}{s}{}", Style::new())
}

#[allow(dead_code)]
pub fn style_yellow(s: &str) -> String {
    let yellow = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Yellow)));
    format!("{yellow}{s}{}", Style::new())
}

pub fn style_red(s: &str) -> String {
    let red = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Red)));
    format!("{red}{s}{}", Style::new())
}

#[allow(dead_code)]
pub fn info(msg: impl Display) {
    println!("  {} {msg}", style_dim("◇"));
}

pub fn success(msg: impl Display) {
    println!("  {} {msg}", style_green("✓"));
}

#[allow(dead_code)]
pub fn warn(msg: impl Display) {
    println!("  {} {msg}", style_yellow("⚠"));
}

pub fn error(msg: impl Display) {
    eprintln!("  {} {msg}", style_red("✗"));
}

pub fn create_spinner(msg: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
            .template("  {spinner:.cyan} {msg}")
            .expect("valid template"),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(80));
    pb
}
