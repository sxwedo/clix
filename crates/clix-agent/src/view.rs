use std::fmt::Write as _;

use unicode_width::UnicodeWidthStr;

use crate::process::LiveAgent;

pub fn render_process_table(agents: &[LiveAgent], resources: bool) -> String {
    if agents.is_empty() {
        return "No running developer agents found.\n".to_owned();
    }

    let mut headers = vec!["ID", "AGENT", "PROJECT", "STATUS", "DURATION"];
    if resources {
        headers.extend(["CPU", "MEMORY"]);
    }
    headers.extend(["TOKENS", "COST"]);

    let rows: Vec<Vec<String>> = agents
        .iter()
        .map(|agent| {
            let mut row = vec![
                format!("{}:{}", agent.kind.slug(), agent.process.pid),
                agent.kind.to_string(),
                agent
                    .process
                    .cwd
                    .as_deref()
                    .map_or_else(|| "-".to_owned(), project_label),
                agent.process.status.clone(),
                format_duration(agent.process.run_time),
            ];
            if resources {
                row.push(format!("{:.1}%", agent.process.cpu_percent));
                row.push(format_bytes(agent.process.memory_bytes));
            }
            row.extend(["-".to_owned(), "-".to_owned()]);
            row
        })
        .collect();
    render_table(&headers, &rows)
}

fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers
        .iter()
        .map(|header| UnicodeWidthStr::width(*header))
        .collect();
    for row in rows {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(UnicodeWidthStr::width(value.as_str()));
        }
    }

    let mut output = String::new();
    for (index, header) in headers.iter().enumerate() {
        if index > 0 {
            output.push_str("  ");
        }
        write_cell(&mut output, header, widths[index]);
    }
    output.push('\n');
    for row in rows {
        for (index, value) in row.iter().enumerate() {
            if index > 0 {
                output.push_str("  ");
            }
            write_cell(&mut output, value, widths[index]);
        }
        output.push('\n');
    }
    output
}

fn write_cell(output: &mut String, value: &str, width: usize) {
    output.push_str(value);
    let padding = width.saturating_sub(UnicodeWidthStr::width(value));
    let _ = write!(output, "{:padding$}", "");
}

fn project_label(path: &std::path::Path) -> String {
    path.file_name()
        .filter(|name| !name.is_empty())
        .map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
}

pub fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = seconds % 86_400 / 3_600;
    let minutes = seconds % 3_600 / 60;
    let seconds = seconds % 60;
    if days > 0 {
        format!("{days}d {hours:02}h")
    } else if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1_024;
    const MIB: u64 = KIB * 1_024;
    const GIB: u64 = MIB * 1_024;
    if bytes >= GIB {
        format_scaled(bytes, GIB, "GiB")
    } else if bytes >= MIB {
        format_scaled(bytes, MIB, "MiB")
    } else if bytes >= KIB {
        format_scaled(bytes, KIB, "KiB")
    } else {
        format!("{bytes} B")
    }
}

fn format_scaled(bytes: u64, unit: u64, suffix: &str) -> String {
    let whole = bytes / unit;
    let decimal = bytes % unit * 10 / unit;
    format!("{whole}.{decimal} {suffix}")
}
