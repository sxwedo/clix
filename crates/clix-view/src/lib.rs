use std::{
    fs,
    io::{self, IsTerminal},
    ops::Range,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Args;
use clix_core::{fs::parent_or_current, ui};

#[derive(Debug, Args)]
pub struct ViewArgs {
    /// Path to the Markdown / MDX file to view
    pub path: PathBuf,
}

/// Render a Markdown or MDX file in the terminal.
///
/// # Errors
///
/// Returns an error when the path does not exist or cannot be read as UTF-8.
pub fn run(args: ViewArgs) -> Result<()> {
    let ViewArgs { path } = args;

    if !path.exists() {
        bail!("file not found: `{}`", path.display());
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read file `{}`", path.display()))?;

    let base_dir = parent_or_current(&path);

    render_markdown_in_terminal(&content, base_dir);

    Ok(())
}

fn native_image_protocol_available() -> bool {
    io::stdout().is_terminal()
        && (viuer::is_iterm_supported() || viuer::get_kitty_support() != viuer::KittySupport::None)
}

fn terminal_image_config(terminal_width: u16) -> viuer::Config {
    let available_width = terminal_width.saturating_sub(2);

    viuer::Config {
        transparent: false,
        absolute_offset: false,
        width: (available_width > 0).then_some(u32::from(available_width)),
        // Supplying only the width makes viuer preserve the image's aspect ratio.
        height: None,
        restore_cursor: false,
        ..Default::default()
    }
}

fn print_terminal_image(image_path: &Path) {
    let (terminal_width, _) = viuer::terminal_size();
    let config = terminal_image_config(terminal_width);

    println!();
    if let Err(error) = viuer::print_from_file(image_path, &config) {
        ui::warn(format!(
            "could not render image {}: {error}",
            image_path.display()
        ));
    }
    println!();
}

fn render_markdown_in_terminal(content: &str, base_dir: &Path) {
    println!();
    let mut in_frontmatter = false;
    let render_local_images = native_image_protocol_available();

    for (line_index, line) in content.lines().enumerate() {
        if line_index == 0 && line.trim() == "---" {
            in_frontmatter = true;
            println!("{}", ui::style_dim(line));
            continue;
        }

        if in_frontmatter {
            println!("{}", ui::style_dim(line));
            if line.trim() == "---" {
                in_frontmatter = false;
            }
            continue;
        }

        let trimmed = line.trim();
        if let Some(image) = markdown_image(trimmed) {
            let has_surrounding_text = !trimmed[..image.range.start].trim().is_empty()
                || !trimmed[image.range.end..].trim().is_empty();
            let resolved_path = if image.path.is_absolute() {
                image.path
            } else {
                base_dir.join(image.path)
            };
            if resolved_path.exists() && render_local_images {
                if has_surrounding_text {
                    println!("{line}");
                }
                println!(
                    "{}",
                    ui::style_yellow_bold(&format!("  🖼️  [Image: {}]", resolved_path.display()))
                );
                print_terminal_image(&resolved_path);
                continue;
            }
        }

        if trimmed.starts_with("# ") {
            println!("{}", ui::style_cyan_bold(line));
        } else if trimmed.starts_with("## ") || trimmed.starts_with("### ") {
            println!("{}", ui::style_yellow_bold(line));
        } else {
            println!("{line}");
        }
    }
    println!();
}

#[derive(Debug, PartialEq, Eq)]
struct MarkdownImage {
    path: PathBuf,
    range: Range<usize>,
}

fn markdown_image(line: &str) -> Option<MarkdownImage> {
    let image_start = line.find("![")?;
    let label_end = line[image_start + 2..].find("](")? + image_start + 2;
    let destination_start = label_end + 2;
    let (destination, markup_end) = parse_image_destination(line, destination_start)?;
    let path = decode_markdown_path(destination)?;
    Some(MarkdownImage {
        path,
        range: image_start..markup_end,
    })
}

fn parse_image_destination(line: &str, start: usize) -> Option<(&str, usize)> {
    let remaining = &line[start..];
    if let Some(angle) = remaining.strip_prefix('<') {
        let end = angle.find('>')?;
        let markup_end = start + end + 2;
        let closing = find_closing_parenthesis(&line[markup_end..])?;
        return Some((&angle[..end], markup_end + closing + 1));
    }

    let mut depth = 0_u32;
    let mut escaped = false;
    for (offset, character) in remaining.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '(' => depth += 1,
            ')' if depth == 0 => {
                return (offset > 0).then_some((&remaining[..offset], start + offset + 1));
            }
            ')' => depth -= 1,
            character if character.is_whitespace() && depth == 0 => {
                let closing = find_closing_parenthesis(&remaining[offset..])?;
                return (offset > 0)
                    .then_some((&remaining[..offset], start + offset + closing + 1));
            }
            _ => {}
        }
    }
    None
}

fn find_closing_parenthesis(value: &str) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
        } else if quote == Some(character) {
            quote = None;
        } else if quote.is_none() && matches!(character, '"' | '\'') {
            quote = Some(character);
        } else if quote.is_none() && character == ')' {
            return Some(index);
        }
    }
    None
}

fn decode_markdown_path(value: &str) -> Option<PathBuf> {
    if value.is_empty()
        || value.contains("://")
        || value.get(..5).is_some_and(|prefix| {
            prefix.eq_ignore_ascii_case("data:") || prefix.eq_ignore_ascii_case("file:")
        })
    {
        return None;
    }

    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if index + 1 < bytes.len() => {
                decoded.push(bytes[index + 1]);
                index += 2;
            }
            b'%' if index + 2 < bytes.len() => {
                let high = hex_value(bytes[index + 1])?;
                let low = hex_value(bytes[index + 2])?;
                decoded.push(high << 4 | low);
                index += 3;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }

    let decoded = String::from_utf8(decoded).ok()?;
    (!decoded.contains('\0')).then(|| PathBuf::from(decoded))
}

const fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{markdown_image, terminal_image_config};

    #[test]
    fn extracts_inline_titled_and_encoded_markdown_image_paths() {
        let inline = "prefix ![alt](media/image.png \"title\") suffix";
        let image = markdown_image(inline).expect("inline image should parse");
        assert_eq!(image.path, PathBuf::from("media/image.png"));
        assert_eq!(&inline[image.range], "![alt](media/image.png \"title\")");
        assert_eq!(
            markdown_image("![alt](</tmp/image%20one.png>)").map(|image| image.path),
            Some(PathBuf::from("/tmp/image one.png"))
        );
        assert_eq!(markdown_image("[link](image.png)"), None);
        assert_eq!(
            markdown_image("![remote](https://example.com/image.png)"),
            None
        );
        assert_eq!(markdown_image("![broken]()"), None);
    }

    #[test]
    fn terminal_image_config_preserves_aspect_ratio_and_uses_available_width() {
        let config = terminal_image_config(80);

        assert_eq!(config.width, Some(78));
        assert_eq!(config.height, None);
    }
}
