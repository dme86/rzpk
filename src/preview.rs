//! Small, bounded previews: filesystem I/O happens on selection changes, never in draw().
use std::{
    fs::{self, File},
    io::Read,
    path::Path,
};

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};

use crate::{Entry, highlight, image_preview::{self, ImagePreview}, remote::Remote};
use ratatui_image::{Resize, StatefulImage};

const MAX_BYTES: u64 = 24 * 1024;
const MAX_LINES: usize = 120;
const MAX_DIRECTORY_ITEMS: usize = 60;
const GRUVBOX_YELLOW: Color = Color::Rgb(215, 153, 33);
const GRUVBOX_AQUA: Color = Color::Rgb(142, 192, 124);

#[derive(Default)]
pub struct Preview {
    pub title: String,
    pub lines: Vec<String>,
    pub syntax: Option<String>,
    pub styled_lines: Option<Vec<Line<'static>>>,
    pub image: Option<ImagePreview>,
}

impl Preview {
    pub fn selected(entries: &[Entry], selected: usize) -> Option<Self> {
        entries.get(selected).map(Self::load)
    }

    fn load(entry: &Entry) -> Self {
        let title = entry.name.clone();
        let (lines, image) = if entry.is_symlink {
            (match fs::read_link(&entry.path) {
                Ok(target) => vec![format!("→ {}", target.display())],
                Err(error) => vec![format!("Cannot read link: {error}")],
            }, None)
        } else if entry.is_dir {
            (directory_preview(&entry.path), None)
        } else if fs::symlink_metadata(&entry.path)
            .is_ok_and(|metadata| metadata.file_type().is_file())
        {
            if image_preview::supports_path(&entry.path) {
                match ImagePreview::open(&entry.path) {
                    Ok(image) => (Vec::new(), Some(image)),
                    Err(error) => (vec![format!("(image preview unavailable: {error})")], None),
                }
            } else {
                (file_preview(&entry.path), None)
            }
        } else {
            (vec!["(not a regular file — no text preview)".to_owned()], None)
        };
        let highlighted = if image.is_none() && !entry.is_symlink && !entry.is_dir
            && !lines.first().is_some_and(|line| {
                line.starts_with("(binary") || line.starts_with("Cannot ")
                    || line.starts_with("(not a regular") || line.starts_with("(image preview")
            })
        {
            highlight::highlight(&entry.path, &lines)
        } else {
            None
        };
        let (syntax, styled_lines) = match highlighted {
            Some((syntax, styled_lines)) => (Some(syntax), Some(styled_lines)),
            None => (None, None),
        };
        Self { title, lines, syntax, styled_lines, image }
    }

    pub fn remote(entry: &Entry, remote: &Remote) -> Self {
        let lines = if entry.is_dir {
            match remote.list(&entry.path) {
                Ok(entries) => entries.into_iter().take(MAX_DIRECTORY_ITEMS)
                    .map(|entry| format!("{}{}", if entry.is_dir { "▸ " } else { "  " }, entry.name))
                    .collect(),
                Err(error) => vec![format!("Cannot read remote directory: {error}")],
            }
        } else if entry.is_symlink {
            vec!["(remote symlink preview unavailable)".to_owned()]
        } else {
            match remote.preview(&entry.path) {
                Ok(bytes) if bytes.contains(&0) => vec!["(binary remote file)".to_owned()],
                Ok(bytes) => {
                    let truncated = bytes.len() > MAX_BYTES as usize;
                    let bytes = &bytes[..bytes.len().min(MAX_BYTES as usize)];
                    let text = String::from_utf8_lossy(bytes);
                    let mut lines = text.lines().take(MAX_LINES).map(|line|
                        line.chars().map(|ch| if ch == '\t' { ' ' } else if ch.is_control() { ' ' } else { ch })
                            .take(4096).collect::<String>()
                    ).collect::<Vec<_>>();
                    if truncated || text.lines().count() > MAX_LINES {
                        lines.push("… preview truncated".to_owned());
                    }
                    lines
                }
                Err(error) => vec![format!("Cannot read remote file: {error}")],
            }
        };
        let highlighted = if !entry.is_dir && !entry.is_symlink
            && !lines.first().is_some_and(|line| {
                line.starts_with("(binary") || line.starts_with("Cannot ")
            })
        {
            highlight::highlight(&entry.path, &lines)
        } else { None };
        let (syntax, styled_lines) = highlighted
            .map_or((None, None), |(syntax, styled)| (Some(syntax), Some(styled)));
        Self { title: entry.name.clone(), lines, syntax, styled_lines, image: None }
    }

    pub fn poll_image(&mut self) -> bool {
        self.image.as_mut().is_some_and(ImagePreview::poll)
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

pub fn draw_preview(frame: &mut Frame, area: Rect, mut preview: Option<&mut Preview>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let title = preview
        .as_ref()
        .map(|preview| {
            if let Some(image) = &preview.image {
                return format!(
                    " PREVIEW · {} · {} · {}×{} ",
                    preview.title, image.format, image.dimensions.0, image.dimensions.1,
                );
            }
            let syntax = preview.syntax.as_deref()
                .map(|name| format!(" · {name}"))
                .unwrap_or_default();
            format!(" PREVIEW · {}{syntax} ", preview.title)
        })
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

    if let Some(image) = preview.as_mut().and_then(|preview| preview.image.as_mut()) {
        let inside = block.inner(area);
        frame.render_widget(block, area);
        if inside.width > 0 && inside.height > 0 {
            frame.render_stateful_widget(
                StatefulImage::default().resize(Resize::Fit(None)),
                inside,
                &mut image.protocol,
            );
        }
        return;
    }

    let lines = preview
        .as_ref()
        .map(|preview| {
            if let Some(lines) = &preview.styled_lines {
                return lines.clone();
            }
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
    fn rust_preview_has_cached_gruvbox_tokens() {
        let root = fixture();
        let file = root.join("hello.rs");
        fs::write(&file, "fn main() { println!(\"Hello\"); }\\n").unwrap();
        let entry = Entry {
            path: file,
            name: "hello.rs".to_owned(),
            name_key: "hello.rs".to_owned(),
            is_dir: false,
            is_symlink: false,
            permissions: String::new(),
            size: None,
            modified: None,
        };
        let preview = Preview::selected(&[entry], 0).unwrap();
        assert!(preview.syntax.is_some());
        assert_eq!(preview.styled_lines.as_ref().unwrap().len(), preview.lines.len());
        assert!(preview.styled_lines.unwrap()[0].spans.len() > 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn image_preview_opens_raster_without_text_decoding() {
        use image::{ImageBuffer, Rgb};
        let root = fixture();
        let file = root.join("picture.png");
        let raster = ImageBuffer::from_pixel(3, 2, Rgb([251u8, 73, 52]));
        raster.save(&file).unwrap();
        let entry = Entry {
            path: file,
            name: "picture.png".to_owned(),
            name_key: "picture.png".to_owned(),
            is_dir: false,
            is_symlink: false,
            permissions: String::new(),
            size: None,
            modified: None,
        };
        let preview = Preview::selected(&[entry], 0).unwrap();
        assert_eq!(preview.image.as_ref().unwrap().dimensions, (3, 2));
        assert!(preview.lines.is_empty());
        assert!(preview.styled_lines.is_none());
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
