//! Small, bounded previews: filesystem I/O happens on selection changes, never in draw().
use std::{
    fs::{self, File},
    io::{self, Read},
    path::Path,
};

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};

use crate::Entry;

const MAX_BYTES: u64 = 24 * 1024;
const MAX_LINES: usize = 120;
const MAX_DIRECTORY_ITEMS: usize = 60;
const GRUVBOX_YELLOW: Color = Color::Rgb(215, 153, 33);
const GRUVBOX_AQUA: Color = Color::Rgb(142, 192, 124);

#[derive(Debug, Default)]
pub struct Preview {
    pub title: String,
    pub lines: Vec<String>,
}

impl Preview {
    pub fn selected(entries: &[Entry], selected: usize) -> Option<Self> {
        entries.get(selected).map(Self::load)
    }

    fn load(entry: &Entry) -> Self {
        let title = entry.name.clone();
        let lines = if entry.is_symlink {
            match fs::read_link(&entry.path) {
                Ok(target) => vec![format!("→ {}", target.display())],
                Err(error) => vec![format!("Cannot read link: {error}")],
            }
        } else if entry.is_dir {
            directory_preview(&entry.path)
        } else {
            file_preview(&entry.path)
        };
        Self { title, lines }
    }
}

fn directory_preview(path: &Path) -> Vec<String> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => return vec![format!("Cannot open directory: {error}")],
    };
    let mut names = Vec::new();
    for item in entries.take(MAX_DIRECTORY_ITEMS + 1) {
        match item {
            Ok(item) => {
                let kind = if item.path().is_dir() { "▸ " } else { "  " };
                names.push(format!("{kind}{}", item.file_name().to_string_lossy()));
            }
            Err(error) => names.push(format!("Cannot read entry: {error}")),
        }
    }
    let truncated = names.len() > MAX_DIRECTORY_ITEMS;
    names.truncate(MAX_DIRECTORY_ITEMS);
    names.sort_by_key(|name| name.to_lowercase());
    if truncated {
        names.push("… more entries".to_string());
    }
    if names.is_empty() {
        names.push("(empty directory)".to_string());
    }
    names
}

fn file_preview(path: &Path) -> Vec<String> {
    let mut input = match File::open(path) {
        Ok(input) => input.take(MAX_BYTES + 1),
        Err(error) => return vec![format!("Cannot open file: {error}")],
    };
    let mut bytes = Vec::new();
    if let Err(error) = input.read_to_end(&mut bytes) {
        return vec![format!("Cannot read file: {error}")];
    }
    let truncated = bytes.len() as u64 > MAX_BYTES;
    bytes.truncate(MAX_BYTES as usize);

    if bytes.contains(&0) {
        return vec!["(binary file — no text preview)".to_string()];
    }

    let raw = if truncated {
        // A UTF-8 sequence can be cut in the middle of the bounded buffer.
        let valid = match std::str::from_utf8(&bytes) {
            Ok(_) => bytes.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => return vec!["(binary or non-UTF-8 file)".to_string()],
        };
        &bytes[..valid]
    } else {
        &bytes
    };
    let text = match std::str::from_utf8(raw) {
        Ok(text) => text,
        Err(_) => return vec!["(binary or non-UTF-8 file)".to_string()],
    };

    let mut lines = text
        .lines()
        .take(MAX_LINES + 1)
        .map(|line| {
            line.chars()
                .flat_map(|ch| match ch {
                    '\t' => "    ".chars().collect::<Vec<_>>(),
                    ch if ch.is_control() => " ".chars().collect::<Vec<_>>(),
                    ch => vec![ch],
                })
                .take(4096)
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    let too_many_lines = lines.len() > MAX_LINES;
    lines.truncate(MAX_LINES);
    if too_many_lines || truncated {
        lines.push("… preview truncated".to_string());
    }
    if lines.is_empty() {
        lines.push("(empty file)".to_string());
    }
    lines
}

pub fn draw_preview(frame: &mut Frame, area: Rect, preview: Option<&Preview>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let title = preview
        .map(|preview| format!(" PREVIEW · {} ", preview.title))
        .unwrap_or_else(|| " PREVIEW ".to_string());
    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(GRUVBOX_YELLOW)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::LEFT | Borders::TOP)
        .border_style(Style::default().fg(Color::Rgb(80, 73, 69)));
    let lines = preview
        .map(|preview| {
            preview.lines.iter().map(|line| {
                let color = if line.starts_with('▸') {
                    GRUVBOX_AQUA
                } else if line.starts_with('…') || line.starts_with('(') {
                    Color::Rgb(146, 131, 116)
                } else {
                    Color::Rgb(189, 174, 147)
                };
                Line::styled(line.clone(), Style::default().fg(color))
            }).collect::<Vec<_>>()
        })
        .unwrap_or_default();
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, time::{SystemTime, UNIX_EPOCH}};

    fn fixture() -> PathBuf {
        let id = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("rzpk-preview-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn text_preview_displays_lines() {
        let root = fixture();
        let file = root.join("hello.rs");
        fs::write(&file, "fn main() {}\n// hello\n").unwrap();
        assert_eq!(file_preview(&file)[0], "fn main() {}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn binary_files_are_not_rendered() {
        let root = fixture();
        let file = root.join("image.png");
        fs::write(&file, [0, 1, 2, 3]).unwrap();
        assert!(file_preview(&file)[0].contains("binary"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_previews_are_bounded() {
        let root = fixture();
        let file = root.join("long.txt");
        fs::write(&file, "line\n".repeat(7000)).unwrap();
        let lines = file_preview(&file);
        assert!(lines.len() <= MAX_LINES + 1);
        assert_eq!(lines.last().unwrap(), "… preview truncated");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_preview_lists_children() {
        let root = fixture();
        fs::write(root.join("a.txt"), b"hello").unwrap();
        assert!(directory_preview(&root).iter().any(|line| line.contains("a.txt")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_files_show_errors_without_crashing() {
        assert!(file_preview(Path::new("/path/that/should/not/exist/rzpk")).first().unwrap().starts_with("Cannot open"));
    }
}
