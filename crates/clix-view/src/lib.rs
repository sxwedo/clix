use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use clix_core::ui;

#[derive(Debug, Args)]
pub struct ViewArgs {
    /// Path to the Markdown / MDX file to view
    pub path: PathBuf,
}

pub async fn run(args: ViewArgs) -> Result<()> {
    if !args.path.exists() {
        bail!("file not found: `{}`", args.path.display());
    }

    let content = fs::read_to_string(&args.path)
        .with_context(|| format!("failed to read file `{}`", args.path.display()))?;

    let base_dir = args.path.parent().unwrap_or_else(|| std::path::Path::new("."));

    ui::render_markdown_in_terminal(&content, base_dir);

    Ok(())
}
