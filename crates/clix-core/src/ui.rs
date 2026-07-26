use std::{
    fmt::Display,
    path::{Path, PathBuf},
};

use anstyle::{AnsiColor, Style};
use anyhow::Result;
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

pub fn print_terminal_image(image_path: &Path) -> Result<()> {
    if !image_path.exists() {
        return Ok(());
    }

    let config = viuer::Config {
        transparent: false,
        absolute_offset: false,
        width: Some(60),
        height: Some(25),
        restore_cursor: false,
        ..Default::default()
    };

    println!();
    let _ = viuer::print_from_file(image_path, &config);
    println!();
    Ok(())
}

pub fn render_markdown_in_terminal(content: &str, base_dir: &Path) {
    println!();
    let cyan = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Cyan))).bold();
    let yellow = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Yellow))).bold();
    let dim = Style::new().dimmed();

    let mut in_frontmatter = false;
    let mut line_idx = 0;

    for line in content.lines() {
        line_idx += 1;
        if line_idx == 1 && line.trim() == "---" {
            in_frontmatter = true;
            println!("{}", format!("{dim}{line}{}", Style::new()));
            continue;
        }

        if in_frontmatter {
            println!("{}", format!("{dim}{line}{}", Style::new()));
            if line.trim() == "---" {
                in_frontmatter = false;
            }
            continue;
        }

        let trimmed = line.trim();

        // Check for markdown image syntax: ![alt](path)
        if trimmed.starts_with("![") && trimmed.contains("](") && trimmed.ends_with(')') {
            if let (Some(start_paren), Some(end_paren)) = (trimmed.find("]("), trimmed.rfind(')')) {
                let img_rel_path = &trimmed[start_paren + 2..end_paren];
                let resolved_path = if img_rel_path.starts_with("./") || img_rel_path.starts_with("../") {
                    base_dir.join(img_rel_path)
                } else {
                    PathBuf::from(img_rel_path)
                };

                if resolved_path.exists() {
                    println!("  {}", format!("{yellow}🖼️  [Image: {}]{}", resolved_path.display(), Style::new()));
                    let _ = print_terminal_image(&resolved_path);
                    continue;
                }
            }
        }

        // Headers
        if trimmed.starts_with("# ") {
            println!("{}", format!("{cyan}{line}{}", Style::new()));
        } else if trimmed.starts_with("## ") || trimmed.starts_with("### ") {
            println!("{}", format!("{yellow}{line}{}", Style::new()));
        } else {
            println!("{line}");
        }
    }
    println!();
}
