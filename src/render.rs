//! Single-pane row rendering and responsive metadata placement.
use std::time::SystemTime;

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    Entry,
    config::MetadataPosition,
    details::{
        Columns, DATE_WIDTH, PERMISSIONS_WIDTH, SIZE_WIDTH, human_size, modified_color,
        modified_date, permission_color,
    },
};

pub fn maximum_name_width(entries: &[Entry]) -> usize {
    entries
        .iter()
        .map(|entry| UnicodeWidthStr::width(entry.name.as_str()))
        .max()
        .unwrap_or(0)
}

fn metadata_width(columns: Columns) -> usize {
    let mut width = 0;
    let mut fields = 0;
    if columns.show_permissions() {
        width += PERMISSIONS_WIDTH;
        fields += 1;
    }
    if columns.show_size() {
        width += SIZE_WIDTH;
        fields += 1;
    }
    if columns.show_date() {
        width += DATE_WIDTH;
        fields += 1;
    }
    width + 2 * fields.saturating_sub(1)
}

fn fit_filename(name: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let original_width = UnicodeWidthStr::width(name);
    if original_width <= width {
        return format!("{name}{}", " ".repeat(width - original_width));
    }

    let mut output = String::new();
    let mut used = 0;
    for ch in name.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + char_width > width - 1 {
            break;
        }
        output.push(ch);
        used += char_width;
    }
    output.push('…');
    output
}

fn metadata_spans(entry: &Entry, columns: Columns, now: SystemTime) -> Vec<Span<'static>> {
    let mut spans = Vec::new();

    if columns.show_permissions() {
        spans.extend(entry.permissions.chars().map(|ch| {
            Span::styled(ch.to_string(), Style::default().fg(permission_color(ch)))
        }));
    }

    if columns.show_size() {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!(
                "{:>width$}",
                human_size(entry.size, entry.is_dir),
                width = SIZE_WIDTH
            ),
            Style::default().fg(Color::Rgb(149, 161, 174)),
        ));
    }

    if columns.show_date() {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!(
                "{:>width$}",
                modified_date(entry.modified),
                width = DATE_WIDTH
            ),
            Style::default().fg(modified_color(entry.modified, now)),
        ));
    }

    spans
}

pub fn entry_line(
    entry: &Entry,
    selected: bool,
    columns: Columns,
    position: MetadataPosition,
    max_name_width: usize,
    terminal_width: u16,
    now: SystemTime,
) -> Line<'static> {
    let (icon, icon_color) = entry.icon_and_color(selected);
    let name_color = if selected { Color::White } else { Color::Gray };
    let mut spans = vec![Span::raw("  ")];
    let metadata = metadata_spans(entry, columns, now);

    if position == MetadataPosition::Left {
        if !metadata.is_empty() {
            spans.extend(metadata);
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(icon, Style::default().fg(icon_color)));
        spans.push(Span::raw("  "));
        spans.push(Span::styled(entry.name.clone(), Style::default().fg(name_color)));
    } else {
        // Leave room for an icon (1 terminal cell), its surrounding whitespace,
        // and a metadata block. Filenames get a common width, then truncate
        // gracefully instead of pushing the columns out of view.
        let overhead = 5 + if metadata.is_empty() {
            0
        } else {
            2 + metadata_width(columns)
        };
        let available = usize::from(terminal_width).saturating_sub(overhead);
        let name_width = max_name_width.min(available);

        spans.push(Span::styled(icon, Style::default().fg(icon_color)));
        spans.push(Span::raw("  "));
        if metadata.is_empty() {
            spans.push(Span::styled(entry.name.clone(), Style::default().fg(name_color)));
        } else {
            spans.push(Span::styled(
                fit_filename(&entry.name, name_width),
                Style::default().fg(name_color),
            ));
            spans.push(Span::raw("  "));
            spans.extend(metadata);
        }
    }

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample(name: &str) -> Entry {
        Entry {
            path: PathBuf::from(name),
            name: name.to_string(),
            name_key: name.to_lowercase(),
            is_dir: false,
            is_symlink: false,
            permissions: "-rw-r--r--".to_string(),
            size: Some(1024),
            modified: Some(SystemTime::UNIX_EPOCH),
        }
    }

    fn plain(line: Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn default_side_puts_name_before_metadata() {
        let file = sample("Cargo.toml");
        let line = plain(entry_line(
            &file, false, Columns::Full, MetadataPosition::Right, 20, 120,
            SystemTime::UNIX_EPOCH,
        ));
        assert!(line.find("Cargo.toml").unwrap() < line.find("-rw-r--r--").unwrap());
    }

    #[test]
    fn legacy_side_puts_metadata_before_name() {
        let file = sample("Cargo.toml");
        let line = plain(entry_line(
            &file, false, Columns::Full, MetadataPosition::Left, 20, 120,
            SystemTime::UNIX_EPOCH,
        ));
        assert!(line.find("-rw-r--r--").unwrap() < line.find("Cargo.toml").unwrap());
    }

    #[test]
    fn right_side_metadata_has_a_consistent_start_column() {
        let a = sample("a");
        let b = sample("a-much-longer-file.txt");
        let max_width = maximum_name_width(&[sample("a"), sample("a-much-longer-file.txt")]);
        let first = plain(entry_line(
            &a, false, Columns::Full, MetadataPosition::Right, max_width, 120,
            SystemTime::UNIX_EPOCH,
        ));
        let second = plain(entry_line(
            &b, false, Columns::Full, MetadataPosition::Right, max_width, 120,
            SystemTime::UNIX_EPOCH,
        ));
        assert_eq!(first.find("-rw-r--r--"), second.find("-rw-r--r--"));
    }

    #[test]
    fn narrow_terminals_do_not_render_metadata() {
        let file = sample("Cargo.toml");
        let line = plain(entry_line(
            &file, false, Columns::Names, MetadataPosition::Right, 20, 25,
            SystemTime::UNIX_EPOCH,
        ));
        assert!(line.contains("Cargo.toml"));
        assert!(!line.contains("-rw-r--r--"));
    }

    #[test]
    fn long_names_truncate_and_keep_metadata_visible() {
        let file = sample("longer-than-our-column-width-filename.rs");
        let line = plain(entry_line(
            &file, false, Columns::Full, MetadataPosition::Right, 100, 64,
            SystemTime::UNIX_EPOCH,
        ));
        assert!(line.contains('…'));
        assert!(line.contains("-rw-r--r--"));
        assert!(UnicodeWidthStr::width(line.as_str()) <= 64);
    }

    #[test]
    fn unicode_filenames_use_terminal_cell_width_not_bytes() {
        assert_eq!(fit_filename("文件.txt", 10), "文件.txt  ");
        assert_eq!(fit_filename("文件.txt", 5), "文件…");
    }
}
