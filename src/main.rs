use std::{
    env, fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    DefaultTerminal, Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

const TYPE_TIMEOUT: Duration = Duration::from_millis(900);
const FOLDER_ICON: &str = "\u{e5ff}";
const OPEN_FOLDER_ICON: &str = "\u{e5fe}";
const FILE_ICON: &str = "\u{f016}";
const LINK_ICON: &str = "\u{f0c1}";
const RUST_ICON: &str = "\u{e7a8}";
const CONFIG_ICON: &str = "\u{e615}";
const MARKDOWN_ICON: &str = "\u{e609}";
const CODE_ICON: &str = "\u{f121}";
const IMAGE_ICON: &str = "\u{f1c5}";
const ARCHIVE_ICON: &str = "\u{f1c6}";

#[derive(Debug)]
struct Entry {
    path: PathBuf,
    name: String,
    name_key: String,
    is_dir: bool,
    is_symlink: bool,
}

impl Entry {
    fn icon_and_color(&self, selected: bool) -> (&'static str, Color) {
        if self.is_symlink {
            return (LINK_ICON, Color::Cyan);
        }
        if self.is_dir {
            return (
                if selected {
                    OPEN_FOLDER_ICON
                } else {
                    FOLDER_ICON
                },
                Color::Rgb(121, 174, 239),
            );
        }

        let ext = self
            .path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");

        match ext.to_ascii_lowercase().as_str() {
            "rs" => (RUST_ICON, Color::Rgb(222, 165, 118)),
            "toml" | "yaml" | "yml" | "json" | "conf" | "ini" => {
                (CONFIG_ICON, Color::Rgb(197, 170, 237))
            }
            "md" | "markdown" | "txt" => (MARKDOWN_ICON, Color::Rgb(153, 180, 191)),
            "c" | "h" | "cpp" | "hpp" | "py" | "js" | "ts" | "sh" => {
                (CODE_ICON, Color::Rgb(135, 196, 177))
            }
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => {
                (IMAGE_ICON, Color::Rgb(212, 165, 200))
            }
            "zip" | "tar" | "gz" | "xz" | "7z" => {
                (ARCHIVE_ICON, Color::Rgb(212, 183, 123))
            }
            _ => (FILE_ICON, Color::Rgb(164, 177, 194)),
        }
    }
}

fn read_directory(path: &Path) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for item in fs::read_dir(path)? {
        let item = item?;
        let file_type = item.file_type()?;
        let path = item.path();
        let name = item.file_name().to_string_lossy().into_owned();
        entries.push(Entry {
            name_key: name.to_lowercase(),
            name,
            is_dir: file_type.is_dir() || (file_type.is_symlink() && path.is_dir()),
            is_symlink: file_type.is_symlink(),
            path,
        });
    }

    // Like a typical file explorer: folders first, then files, both alphabetically.
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name_key.cmp(&b.name_key))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(entries)
}

struct App {
    cwd: PathBuf,
    entries: Vec<Entry>,
    selected: usize,
    scroll: usize,
    typed: String,
    last_typed: Option<Instant>,
    error: Option<String>,
    running: bool,
}

impl App {
    fn new() -> io::Result<Self> {
        let cwd = env::current_dir()?;
        let entries = read_directory(&cwd)?;
        Ok(Self {
            cwd,
            entries,
            selected: 0,
            scroll: 0,
            typed: String::new(),
            last_typed: None,
            error: None,
            running: true,
        })
    }

    fn change_directory(&mut self, cwd: PathBuf, focus: Option<&Path>) -> io::Result<()> {
        let entries = read_directory(&cwd)?;
        let selected = focus
            .and_then(|path| entries.iter().position(|entry| entry.path == path))
            .unwrap_or(0);
        self.cwd = cwd;
        self.entries = entries;
        self.selected = selected;
        self.scroll = 0;
        self.clear_typed();
        self.error = None;
        Ok(())
    }

    fn parent(&mut self) -> io::Result<()> {
        let old = self.cwd.clone();
        if let Some(parent) = old.parent() {
            self.change_directory(parent.to_path_buf(), Some(&old))?;
        }
        Ok(())
    }

    fn open_selected(&mut self) -> io::Result<()> {
        if let Some(entry) = self.entries.get(self.selected)
            && entry.is_dir
        {
            self.change_directory(entry.path.clone(), None)?;
        }
        Ok(())
    }

    fn clear_typed(&mut self) {
        self.typed.clear();
        self.last_typed = None;
    }

    fn find_prefix(&self, prefix: &str) -> Option<usize> {
        let prefix = prefix.to_lowercase();
        self.entries
            .iter()
            .position(|entry| entry.name_key.starts_with(&prefix))
    }

    fn jump_to(&mut self, ch: char) {
        let now = Instant::now();
        let mut prefix = if self
            .last_typed
            .is_some_and(|last| now.duration_since(last) < TYPE_TIMEOUT)
        {
            self.typed.clone()
        } else {
            String::new()
        };
        prefix.push(ch);

        if let Some(index) = self.find_prefix(&prefix) {
            self.selected = index;
            self.typed = prefix;
        } else {
            // If the longer prefix has no match, treat the last key as a new search.
            let single = ch.to_string();
            if let Some(index) = self.find_prefix(&single) {
                self.selected = index;
                self.typed = single;
            } else {
                self.typed.clear();
            }
        }
        self.last_typed = Some(now);
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.running = false,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.running = false;
            }
            KeyCode::Backspace => {
                if let Err(error) = self.parent() {
                    self.error = Some(error.to_string());
                }
            }
            KeyCode::Enter => {
                if let Err(error) = self.open_selected() {
                    self.error = Some(error.to_string());
                }
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                self.clear_typed();
            }
            KeyCode::Down => {
                if !self.entries.is_empty() {
                    self.selected = (self.selected + 1).min(self.entries.len() - 1);
                }
                self.clear_typed();
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.jump_to(ch);
            }
            _ => {}
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.height == 0 || area.width == 0 {
            return;
        }

        let mut title = vec![
            Span::styled(
                " rzpk ",
                Style::default()
                    .fg(Color::Rgb(126, 184, 238))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(self.cwd.display().to_string(), Style::default().fg(Color::DarkGray)),
        ];
        if !self.typed.is_empty() {
            title.push(Span::styled(
                format!("  /{}", self.typed),
                Style::default().fg(Color::Yellow),
            ));
        }
        if let Some(error) = &self.error {
            title.push(Span::styled(
                format!("  {error}"),
                Style::default().fg(Color::LightRed),
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(title)),
            Rect::new(area.x, area.y, area.width, 1),
        );

        let height = usize::from(area.height.saturating_sub(1));
        if height == 0 {
            return;
        }
        if self.entries.is_empty() {
            frame.render_widget(
                Paragraph::new("  (empty directory)").style(Style::default().fg(Color::DarkGray)),
                Rect::new(area.x, area.y + 1, area.width, 1),
            );
            return;
        }

        // Render only the visible rows; never build a widget for every file.
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll.saturating_add(height) {
            self.scroll = self.selected + 1 - height;
        }

        for (row, entry) in self.entries.iter().skip(self.scroll).take(height).enumerate() {
            let selected = self.scroll + row == self.selected;
            let (icon, color) = entry.icon_and_color(selected);
            let background = if selected {
                Style::default().bg(Color::Rgb(36, 51, 69))
            } else {
                Style::default()
            };
            let text_color = if selected { Color::White } else { Color::Gray };
            let line = Line::from(vec![
                Span::raw("  "),
                Span::styled(icon, Style::default().fg(color)),
                Span::raw("  "),
                Span::styled(entry.name.as_str(), Style::default().fg(text_color)),
            ]);
            frame.render_widget(
                Paragraph::new(line).style(background),
                Rect::new(area.x, area.y + 1 + row as u16, area.width, 1),
            );
        }
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        let mut needs_redraw = true;
        while self.running {
            if needs_redraw {
                terminal.draw(|frame| self.draw(frame))?;
                needs_redraw = false;
            }

            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        self.handle_key(key);
                        needs_redraw = true;
                    }
                    Event::Resize(_, _) => needs_redraw = true,
                    _ => {}
                }
            }

            if self
                .last_typed
                .is_some_and(|last| last.elapsed() >= TYPE_TIMEOUT)
            {
                self.clear_typed();
                needs_redraw = true;
            }
        }
        Ok(())
    }
}

fn main() -> io::Result<()> {
    let mut app = App::new()?;
    let mut terminal = ratatui::init();
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str, is_dir: bool) -> Entry {
        Entry {
            path: PathBuf::from(name),
            name: name.to_owned(),
            name_key: name.to_lowercase(),
            is_dir,
            is_symlink: false,
        }
    }

    fn app(entries: Vec<Entry>) -> App {
        App {
            cwd: PathBuf::from("/"),
            entries,
            selected: 0,
            scroll: 0,
            typed: String::new(),
            last_typed: None,
            error: None,
            running: true,
        }
    }

    #[test]
    fn uppercase_key_matches_lowercase_folder() {
        let mut app = app(vec![sample("archive", true), sample("downloads", true)]);
        app.jump_to('D');
        assert_eq!(app.entries[app.selected].name, "downloads");
    }

    #[test]
    fn typing_multiple_letters_refines_selection() {
        let mut app = app(vec![sample("Documents", true), sample("Downloads", true)]);
        app.jump_to('d');
        app.jump_to('o');
        app.jump_to('w');
        assert_eq!(app.entries[app.selected].name, "Downloads");
    }

    #[test]
    fn backspace_moves_to_parent_and_reselects_child() -> io::Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is set")
            .as_nanos();
        let parent = env::temp_dir().join(format!("rzpk-{}-{suffix}", std::process::id()));
        let child = parent.join("Downloads");
        fs::create_dir_all(&child)?;

        let mut app = app(Vec::new());
        app.cwd = child.clone();
        app.parent()?;

        assert_eq!(app.cwd, parent);
        assert_eq!(app.entries[app.selected].path, child);
        fs::remove_dir_all(parent)?;
        Ok(())
    }

    #[test]
    fn no_match_does_not_move_selection() {
        let mut app = app(vec![sample("Cargo.toml", false), sample("src", true)]);
        app.selected = 1;
        app.jump_to('z');
        assert_eq!(app.selected, 1);
    }
}
