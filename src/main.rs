use std::{
    env, fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

mod config;
mod details;
mod pane;
mod preview;
mod render;
mod selection;
mod tree;

use config::{Config, MetadataPosition};
use details::permissions;
use render::maximum_name_width;
use pane::{PaneState, PaneView, draw_pane_content, draw_split_pane, use_two_columns};
use preview::Preview;
use tree::{RowKind, TreeState};
use selection::Selection;
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
    permissions: String,
    size: Option<u64>,
    modified: Option<SystemTime>,
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

fn directory_entry(item: fs::DirEntry) -> io::Result<Entry> {
    let file_type = item.file_type()?;
    let path = item.path();
    let name = item.file_name().to_string_lossy().into_owned();
    let metadata = fs::symlink_metadata(&path).ok();
    Ok(Entry {
        name_key: name.to_lowercase(),
        name,
        is_dir: file_type.is_dir() || (file_type.is_symlink() && path.is_dir()),
        is_symlink: file_type.is_symlink(),
        permissions: permissions(metadata.as_ref()),
        size: metadata.as_ref().map(fs::Metadata::len),
        modified: metadata.as_ref().and_then(|metadata| metadata.modified().ok()),
        path,
    })
}

fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name_key.cmp(&b.name_key))
            .then_with(|| a.name.cmp(&b.name))
    });
}

fn read_directory(path: &Path) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for item in fs::read_dir(path)? {
        entries.push(directory_entry(item?)?);
    }
    sort_entries(&mut entries);
    Ok(entries)
}

struct App {
    cwd: PathBuf,
    entries: Vec<Entry>,
    max_name_width: usize,
    metadata_position: MetadataPosition,
    selected: usize,
    selection: Selection,
    scroll: usize,
    typed: String,
    command: Option<String>,
    last_typed: Option<Instant>,
    error: Option<String>,
    running: bool,
    preview_mode: bool,
    preview: Option<Preview>,
    tree_mode: bool,
    tree: Option<TreeState>,
    other: Option<PaneState>,
    active_right: bool,
}

impl App {
    fn new() -> io::Result<Self> {
        let cwd = env::current_dir()?;
        let metadata_position = Config::load()?.metadata_position;
        let entries = read_directory(&cwd)?;
        let max_name_width = maximum_name_width(&entries);
        Ok(Self {
            cwd,
            entries,
            max_name_width,
            metadata_position,
            selected: 0,
            selection: Selection::default(),
            scroll: 0,
            typed: String::new(),
            command: None,
            last_typed: None,
            error: None,
            running: true,
            preview_mode: false,
            preview: None,
            tree_mode: false,
            tree: None,
            other: None,
            active_right: false,
        })
    }

    fn replace_visible(&mut self, entries: Vec<Entry>, focus: Option<&Path>, fallback: usize) {
        let marked = self.entries.iter().enumerate()
            .filter(|(index, _)| self.selection.contains(*index)
                && self.tree.as_ref().is_none_or(|tree| tree.is_real(*index)))
            .map(|(_, entry)| entry.path.clone())
            .collect::<Vec<_>>();
        self.max_name_width = maximum_name_width(&entries);
        self.selected = focus
            .and_then(|path| entries.iter().position(|entry| entry.path == path))
            .unwrap_or_else(|| fallback.min(entries.len().saturating_sub(1)));
        self.entries = entries;
        self.selection.clear();
        for (index, entry) in self.entries.iter().enumerate() {
            if marked.contains(&entry.path)
                && self.tree.as_ref().is_none_or(|tree| tree.is_real(index))
            {
                self.selection.toggle(index);
            }
        }
        self.scroll = 0;
        self.clear_typed();
        self.refresh_active_preview();
    }

    fn change_directory(&mut self, cwd: PathBuf, focus: Option<&Path>) -> io::Result<()> {
        let (tree, entries) = if self.tree_mode {
            let (tree, entries) = TreeState::open(&cwd)?;
            (Some(tree), entries)
        } else {
            (None, read_directory(&cwd)?)
        };
        self.cwd = cwd;
        self.entries = entries;
        self.max_name_width = maximum_name_width(&self.entries);
        self.tree = tree;
        self.selected = focus
            .and_then(|path| self.entries.iter().position(|entry| entry.path == path))
            .unwrap_or(0);
        self.selection.clear();
        self.scroll = 0;
        self.clear_typed();
        self.error = None;
        self.refresh_active_preview();
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
        if let Some(tree) = self.tree.as_mut() {
            let index = self.selected;
            let path = self.entries.get(index).map(|entry| entry.path.clone());
            let action = tree.rows.get(index).map(|row| &row.kind);
            match action {
                Some(RowKind::More(_)) => {
                    let entries = tree.activate(&self.cwd, index)?;
                    if !entries.is_empty() {
                        self.replace_visible(entries, None, index);
                    }
                    return Ok(());
                }
                Some(RowKind::Entry { .. }) => {
                    if let Some(path) = path
                        && self.entries[index].is_dir
                        && !self.entries[index].is_symlink
                    {
                        let entries = tree.toggle_directory(&self.cwd, &path)?;
                        self.replace_visible(entries, Some(&path), index);
                    }
                    return Ok(());
                }
                _ => return Ok(()),
            }
        }
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

    fn find_match(&self, query: &str) -> Option<usize> {
        let query = query.to_lowercase();
        // Prefer filenames beginning with the query. If none match, find the
        // query anywhere in the name (e.g. "vim" selects "neovim").
        self.entries
            .iter()
            .enumerate()
            .position(|(index, entry)| self.tree.as_ref().is_none_or(|tree| tree.is_real(index))
                && entry.name_key.starts_with(&query))
            .or_else(|| {
                self.entries
                    .iter()
                    .enumerate()
                    .position(|(index, entry)| self.tree.as_ref().is_none_or(|tree| tree.is_real(index))
                        && entry.name_key.contains(&query))
            })
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

        if let Some(index) = self.find_match(&prefix) {
            self.selection.commit();
            self.selected = index;
            self.refresh_active_preview();
            self.typed = prefix;
        } else {
            // If the longer prefix has no match, treat the last key as a new search.
            let single = ch.to_string();
            if let Some(index) = self.find_match(&single) {
                self.selection.commit();
                self.selected = index;
                self.refresh_active_preview();
                self.typed = single;
            } else {
                self.typed.clear();
            }
        }
        self.last_typed = Some(now);
    }

    fn move_cursor(&mut self, direction: i8, extend: bool) {
        if self.entries.is_empty() {
            return;
        }
        let next = if direction < 0 {
            self.selected.saturating_sub(1)
        } else {
            (self.selected + 1).min(self.entries.len() - 1)
        };
        if extend {
            if next != self.selected {
                self.selection.extend(self.selected, next);
            }
        } else {
            self.selection.commit();
        }
        if self.selected != next {
            self.selected = next;
            self.refresh_active_preview();
        }
        self.clear_typed();
    }

    fn toggle_current(&mut self) {
        if !self.entries.is_empty()
            && self.tree.as_ref().is_none_or(|tree| tree.is_real(self.selected))
        {
            self.selection.toggle(self.selected);
        }
        self.clear_typed();
    }

    fn switch_pane(&mut self) {
        let Some(other) = self.other.as_mut() else {
            return;
        };
        // Store the inactive pane in `other`, leaving the focused pane in the
        // existing fields. All filesystem and keyboard methods stay pane-local.
        std::mem::swap(&mut self.cwd, &mut other.cwd);
        std::mem::swap(&mut self.entries, &mut other.entries);
        std::mem::swap(&mut self.max_name_width, &mut other.max_name_width);
        std::mem::swap(&mut self.selected, &mut other.selected);
        std::mem::swap(&mut self.selection, &mut other.selection);
        std::mem::swap(&mut self.scroll, &mut other.scroll);
        std::mem::swap(&mut self.typed, &mut other.typed);
        std::mem::swap(&mut self.last_typed, &mut other.last_typed);
        std::mem::swap(&mut self.error, &mut other.error);
        std::mem::swap(&mut self.preview, &mut other.preview);
        std::mem::swap(&mut self.tree, &mut other.tree);
        self.active_right = !self.active_right;
    }

    fn toggle_vertical_split(&mut self) -> io::Result<()> {
        if self.other.is_some() {
            // Closing the split always keeps the left pane visible.
            if self.active_right {
                self.switch_pane();
            }
            self.other = None;
            self.active_right = false;
        } else {
            // Open the right pane in the same folder, but with independent
            // cursor, selection, search, scroll and navigation.
            let mut right = PaneState::open(&self.cwd)?;
            if self.preview_mode {
                right.refresh_preview();
            }
            if self.tree_mode {
                right.enable_tree()?;
            }
            self.other = Some(right);
            self.active_right = false;
        }
        Ok(())
    }

    fn refresh_active_preview(&mut self) {
        if self.preview_mode {
            self.preview = if self.tree.as_ref().is_none_or(|tree| tree.is_real(self.selected)) {
                Preview::selected(&self.entries, self.selected)
            } else {
                None
            };
        }
    }

    fn toggle_preview(&mut self) {
        self.preview_mode = !self.preview_mode;
        if self.preview_mode {
            self.refresh_active_preview();
            if let Some(other) = self.other.as_mut() {
                other.refresh_preview();
            }
        } else {
            self.preview = None;
            if let Some(other) = self.other.as_mut() {
                other.preview = None;
            }
        }
    }

    fn toggle_tree(&mut self) -> io::Result<()> {
        if self.tree_mode {
            let left_focus = self.entries.get(self.selected).map(|entry| entry.path.clone());
            let left_entries = read_directory(&self.cwd)?;
            let right_entries = if let Some(other) = self.other.as_ref() {
                Some(read_directory(&other.cwd)?)
            } else {
                None
            };
            self.tree_mode = false;
            self.tree = None;
            self.replace_visible(left_entries, left_focus.as_deref(), 0);
            if let (Some(other), Some(entries)) = (self.other.as_mut(), right_entries) {
                other.disable_tree_with_entries(entries);
            }
        } else {
            let left_focus = self.entries.get(self.selected).map(|entry| entry.path.clone());
            let (tree, entries) = TreeState::open(&self.cwd)?;
            let right = if let Some(other) = self.other.as_ref() {
                Some(TreeState::open(&other.cwd)?)
            } else {
                None
            };
            self.tree_mode = true;
            self.tree = Some(tree);
            self.replace_visible(entries, left_focus.as_deref(), 0);
            if let (Some(other), Some((tree, entries))) = (self.other.as_mut(), right) {
                other.enable_tree_with_entries(tree, entries);
            }
        }
        Ok(())
    }

    fn normal_mode(&mut self) {
        if self.active_right {
            self.switch_pane();
        }
        self.other = None;
        self.active_right = false;
        self.preview_mode = false;
        self.preview = None;
        if self.tree_mode {
            if let Err(error) = self.toggle_tree() {
                self.error = Some(error.to_string());
            }
        }
    }

    fn handle_command_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            // Escape only cancels command entry; it never exits rzpk.
            KeyCode::Esc => self.command = None,
            KeyCode::Enter => {
                let command = self.command.take().unwrap_or_default();
                match command.trim() {
                    "q" => self.running = false,
                    "v" => {
                        if let Err(error) = self.toggle_vertical_split() {
                            self.error = Some(error.to_string());
                        }
                    }
                    "p" => self.toggle_preview(),
                    "t" => {
                        if let Err(error) = self.toggle_tree() {
                            self.error = Some(error.to_string());
                        }
                    },
                    "n" => self.normal_mode(),
                    _ => self.error = Some(format!("unknown command: :{command}")),
                }
            }
            KeyCode::Backspace => {
                if let Some(command) = self.command.as_mut() {
                    command.pop();
                }
            }
            KeyCode::Char(ch) if !ctrl && !alt => {
                if let Some(command) = self.command.as_mut()
                    && command.len() < 64
                {
                    command.push(ch);
                }
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        // Ctrl+C always quits, even in command mode.
        if key.code == KeyCode::Char('c') && ctrl {
            self.running = false;
            return;
        }

        if self.command.is_some() {
            self.handle_command_key(key);
            return;
        }

        match key.code {
            // Clear all current marks without exiting or changing the cursor.
            KeyCode::Esc => {
                self.selection.clear();
                self.clear_typed();
            }
            // Colon enters command mode regardless of the terminal's Shift reporting.
            KeyCode::Char(':') if !ctrl && !alt => {
                self.command = Some(String::new());
                self.clear_typed();
                self.error = None;
            }
            KeyCode::Tab | KeyCode::BackTab if !ctrl && !alt => self.switch_pane(),
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
            KeyCode::Up if !ctrl && !alt => self.move_cursor(-1, shift),
            KeyCode::Down if !ctrl && !alt => self.move_cursor(1, shift),
            // Uppercase J/K also works when a terminal omits the Shift modifier.
            KeyCode::Char('J') if !ctrl && !alt => self.move_cursor(1, true),
            KeyCode::Char('K') if !ctrl && !alt => self.move_cursor(-1, true),
            KeyCode::Char('j') if shift && !ctrl && !alt => self.move_cursor(1, true),
            KeyCode::Char('k') if shift && !ctrl && !alt => self.move_cursor(-1, true),
            // A few terminals report Shift+Space as plain Space. Accept both.
            KeyCode::Char(' ') if !ctrl && !alt => self.toggle_current(),
            // Typed search must never swallow Shift-modified keys.
            KeyCode::Char(ch) if !shift && !ctrl && !alt => self.jump_to(ch),
            _ => {}
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.height == 0 || area.width == 0 {
            return;
        }

        let split = self.other.is_some();
        let mut title = vec![Span::styled(
            " rzpk ",
            Style::default()
                .fg(Color::Rgb(126, 184, 238))
                .add_modifier(Modifier::BOLD),
        )];

        if split {
            title.push(Span::styled(
                "[v]",
                Style::default().fg(Color::Rgb(131, 165, 152)), // Gruvbox blue
            ));
        }
        if self.preview_mode {
            title.push(Span::styled(
                if split { " [p]" } else { "[p]" },
                Style::default().fg(Color::Rgb(250, 189, 47)), // Gruvbox yellow
            ));
        }
        if split {
            title.push(Span::styled(
                "  Tab: switch",
                Style::default().fg(Color::DarkGray),
            ));
        } else {
            title.push(Span::raw("  "));
            title.push(Span::styled(
                self.cwd.display().to_string(),
                Style::default().fg(Color::DarkGray),
            ));
            if self.selection.count() > 0 {
                title.push(Span::styled(
                    format!("  {} marked", self.selection.count()),
                    Style::default().fg(Color::Rgb(91, 205, 160)),
                ));
            }
        }

        if let Some(command) = &self.command {
            title.push(Span::styled(
                format!("  :{command}█"),
                Style::default().fg(Color::Yellow),
            ));
        } else if !split && !self.typed.is_empty() {
            title.push(Span::styled(
                format!("  /{}", self.typed),
                Style::default().fg(Color::Yellow),
            ));
        }
        if !split && let Some(error) = &self.error {
            title.push(Span::styled(
                format!("  {error}"),
                Style::default().fg(Color::LightRed),
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(title)),
            Rect::new(area.x, area.y, area.width, 1),
        );

        let content = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(1),
        );

        let mut current = PaneView {
            cwd: &self.cwd,
            entries: &self.entries,
            max_name_width: self.max_name_width,
            selected: self.selected,
            selection: &self.selection,
            scroll: &mut self.scroll,
            typed: &self.typed,
            error: self.error.as_deref(),
            preview: self.preview.as_ref(),
            tree_rows: self.tree.as_ref().map(|tree| tree.rows.as_slice()),
        };

        let Some(other) = self.other.as_mut() else {
            draw_pane_content(frame, content, &mut current, self.metadata_position, true, self.preview_mode);
            return;
        };
        let mut inactive = other.view();

        if !use_two_columns(area.width) {
            draw_split_pane(frame, content, &mut current, self.metadata_position, true, self.preview_mode);
            return;
        }

        let left_width = area.width.saturating_sub(1) / 2;
        let right_width = area.width.saturating_sub(1 + left_width);
        let left = Rect::new(content.x, content.y, left_width, content.height);
        let divider_x = content.x.saturating_add(left_width);
        let right = Rect::new(
            divider_x.saturating_add(1),
            content.y,
            right_width,
            content.height,
        );

        for y in content.y..content.y.saturating_add(content.height) {
            frame.render_widget(
                Paragraph::new("│").style(Style::default().fg(Color::Rgb(51, 64, 78))),
                Rect::new(divider_x, y, 1, 1),
            );
        }

        // `current` is always the active pane; swapping it on Tab preserves
        // the left/right coordinates independently from each pane's state.
        if self.active_right {
            draw_split_pane(frame, left, &mut inactive, self.metadata_position, false, self.preview_mode);
            draw_split_pane(frame, right, &mut current, self.metadata_position, true, self.preview_mode);
        } else {
            draw_split_pane(frame, left, &mut current, self.metadata_position, true, self.preview_mode);
            draw_split_pane(frame, right, &mut inactive, self.metadata_position, false, self.preview_mode);
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
            if let Some(other) = self.other.as_mut()
                && other.expire_search(TYPE_TIMEOUT)
            {
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
            permissions: if is_dir { "drwxr-xr-x" } else { "-rw-r--r--" }.to_string(),
            size: Some(123),
            modified: Some(SystemTime::now()),
        }
    }

    fn app(entries: Vec<Entry>) -> App {
        App {
            cwd: PathBuf::from("/"),
            max_name_width: 0,
            metadata_position: MetadataPosition::Right,
            entries,
            selected: 0,
            selection: Selection::default(),
            scroll: 0,
            typed: String::new(),
            command: None,
            last_typed: None,
            error: None,
            running: true,
            preview_mode: false,
            preview: None,
            tree_mode: false,
            tree: None,
            other: None,
            active_right: false,
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
    fn typing_vim_selects_neovim_by_substring() {
        let mut app = app(vec![
            sample("downloads", true),
            sample("neovim", true),
            sample("notes", true),
        ]);
        for ch in "vim".chars() {
            app.jump_to(ch);
        }
        assert_eq!(app.entries[app.selected].name, "neovim");
        assert_eq!(app.typed, "vim");
    }

    #[test]
    fn prefix_match_has_priority_over_substring_match() {
        let mut app = app(vec![
            sample("neovim", true),
            sample("vim", true),
            sample("vimrc", false),
        ]);
        for ch in "vim".chars() {
            app.jump_to(ch);
        }
        assert_eq!(app.entries[app.selected].name, "vim");
    }

    #[test]
    fn substring_matching_is_case_insensitive() {
        let mut app = app(vec![sample("config", true), sample("NeoVim", true)]);
        for ch in "VIM".chars() {
            app.jump_to(ch);
        }
        assert_eq!(app.entries[app.selected].name, "NeoVim");
    }

    #[test]
    fn no_longer_match_starts_a_fresh_search() {
        let mut app = app(vec![sample("neovim", true), sample("downloads", true)]);
        app.jump_to('v');
        app.jump_to('z');
        assert_eq!(app.entries[app.selected].name, "neovim");
        assert!(app.typed.is_empty());
        app.jump_to('d');
        assert_eq!(app.entries[app.selected].name, "downloads");
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn shift_arrows_mark_ranges_and_can_shrink_them() {
        let mut app = app((0..6).map(|index| sample(&format!("file{index}"), false)).collect());
        app.selected = 1;
        app.handle_key(key(KeyCode::Down, KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(app.selected, 3);
        assert_eq!(app.selection.count(), 3);
        app.handle_key(key(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(app.selected, 2);
        assert_eq!(app.selection.count(), 2);
        assert!(!app.selection.contains(3));
    }

    #[test]
    fn normal_navigation_keeps_marked_range_and_new_range_adds() {
        let mut app = app((0..6).map(|index| sample(&format!("file{index}"), false)).collect());
        app.handle_key(key(KeyCode::Down, KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(app.selected, 3);
        assert_eq!(app.selection.count(), 4);
        assert!((0..4).all(|index| app.selection.contains(index)));
    }

    #[test]
    fn shift_j_and_k_extend_selection_instead_of_searching() {
        let mut app = app(vec![
            sample("apple", true),
            sample("jellyfish", true),
            sample("kiwi", true),
        ]);
        app.handle_key(key(KeyCode::Char('J'), KeyModifiers::SHIFT));
        assert_eq!(app.selected, 1);
        assert_eq!(app.selection.count(), 2);
        assert!(app.typed.is_empty());
        // Some terminals send a capital letter without the SHIFT modifier.
        app.handle_key(key(KeyCode::Char('K'), KeyModifiers::NONE));
        assert_eq!(app.selected, 0);
        assert_eq!(app.selection.count(), 1);
    }

    #[test]
    fn shift_space_toggles_distant_entries_without_clearing_previous_marks() {
        let mut app = app((0..6).map(|index| sample(&format!("file{index}"), false)).collect());
        app.handle_key(key(KeyCode::Char(' '), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Char(' '), KeyModifiers::SHIFT));
        assert_eq!(app.selection.count(), 2);
        assert!(app.selection.contains(0));
        assert!(app.selection.contains(2));
        app.handle_key(key(KeyCode::Char(' '), KeyModifiers::SHIFT));
        assert_eq!(app.selection.count(), 1);
        assert!(!app.selection.contains(2));
    }

    #[test]
    fn plain_space_is_a_fallback_if_terminal_does_not_report_shift() {
        let mut app = app(vec![sample("file", false)]);
        app.handle_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(app.selection.count(), 1);
        assert!(app.typed.is_empty());
    }

    #[test]
    fn shifted_other_letters_do_not_start_or_extend_search() {
        let mut app = app(vec![sample("apple", true), sample("zebra", true)]);
        app.handle_key(key(KeyCode::Char('z'), KeyModifiers::SHIFT));
        assert_eq!(app.selected, 0);
        assert!(app.typed.is_empty());
        app.handle_key(key(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(app.selected, 1);
        assert_eq!(app.typed, "z");
    }

    #[test]
    fn selection_navigation_at_bounds_does_not_invent_indices() {
        let mut empty = app(vec![]);
        empty.handle_key(key(KeyCode::Down, KeyModifiers::SHIFT));
        empty.handle_key(key(KeyCode::Char(' '), KeyModifiers::SHIFT));
        assert_eq!(empty.selection.count(), 0);

        let mut single = app(vec![sample("file", false)]);
        single.handle_key(key(KeyCode::Up, KeyModifiers::SHIFT));
        single.handle_key(key(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(single.selected, 0);
        assert_eq!(single.selection.count(), 0);
    }

    #[test]
    fn escape_clears_disjoint_and_range_marks_but_never_quits() {
        let mut app = app((0..6).map(|i| sample(&format!("file{i}"), false)).collect());
        app.selection.toggle(0);
        app.selected = 2;
        app.handle_key(key(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(app.selection.count(), 3);
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.selection.count(), 0);
        assert_eq!(app.selected, 3);
        assert!(app.running);

        // Esc remains harmless if there are no marks.
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.running);
    }

    #[test]
    fn ctrl_c_quits_with_or_without_selection() {
        for marked in [false, true] {
            let mut app = app(vec![sample("file", false)]);
            if marked {
                app.selection.toggle(0);
            }
            app.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
            assert!(!app.running);
        }
    }

    #[test]
    fn colon_q_enter_quits_and_never_runs_filename_search() {
        let mut app = app(vec![sample("quit", false), sample("README.md", false)]);
        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        assert_eq!(app.command.as_deref(), Some(""));
        app.handle_key(key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert_eq!(app.command.as_deref(), Some("q"));
        assert_eq!(app.selected, 0);
        assert!(app.typed.is_empty());
        assert!(app.running);
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.running);
        assert!(app.command.is_none());
    }

    #[test]
    fn escape_dismisses_command_without_losing_selection() {
        let mut app = app(vec![sample("file", false)]);
        app.selection.toggle(0);
        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Char('q'), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.running);
        assert!(app.command.is_none());
        assert_eq!(app.selection.count(), 1);
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.selection.count(), 0);
        assert!(app.running);
    }

    #[test]
    fn command_supports_backspace_and_reports_unknown_commands() {
        let mut app = app(vec![sample("file", false)]);
        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert_eq!(app.command.as_deref(), Some("q"));
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.running);
        assert_eq!(app.error.as_deref(), Some("unknown command: :x"));
        assert!(app.command.is_none());
    }

    #[test]
    fn ctrl_c_also_quits_during_command_entry() {
        let mut app = app(vec![sample("file", false)]);
        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!app.running);
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
        app.selection.toggle(0);
        app.parent()?;

        assert_eq!(app.cwd, parent);
        assert_eq!(app.entries[app.selected].path, child);
        assert_eq!(app.selection.count(), 0);
        fs::remove_dir_all(parent)?;
        Ok(())
    }

    fn split_fixture() -> io::Result<PathBuf> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is set")
            .as_nanos();
        let root = env::temp_dir().join(format!("rzpk-split-{}-{nonce}", std::process::id()));
        fs::create_dir_all(root.join("alpha"))?;
        fs::create_dir_all(root.join("beta"))?;
        fs::write(root.join("notes.txt"), b"sample")?;
        Ok(root)
    }

    #[test]
    fn command_v_creates_split_and_an_independent_right_pane() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();

        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Char('v'), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(app.other.is_some());
        assert!(!app.active_right);
        assert_eq!(app.other.as_ref().unwrap().cwd, root);
        assert_eq!(app.other.as_ref().unwrap().selected, 0);
        assert!(app.running);

        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn tab_switches_independent_cursor_selection_and_directory() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.toggle_vertical_split()?;

        // Left marks alpha and focuses beta.
        app.handle_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selected, 1);
        assert_eq!(app.selection.count(), 1);

        // Right starts unmarked at alpha and can navigate into it.
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert!(app.active_right);
        assert_eq!(app.selected, 0);
        assert_eq!(app.selection.count(), 0);
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.cwd, root.join("alpha"));
        app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.cwd, root);

        // Left kept its own cursor and mark; changes on right never touched it.
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert!(!app.active_right);
        assert_eq!(app.cwd, root);
        assert_eq!(app.selected, 1);
        assert_eq!(app.selection.count(), 1);
        assert!(app.selection.contains(0));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn pane_specific_search_and_escape_only_affect_active_side() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.toggle_vertical_split()?;
        app.jump_to('b');
        assert_eq!(app.selected, 1);
        assert_eq!(app.typed, "b");

        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert!(app.typed.is_empty());
        app.handle_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.selection.count(), 0);
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.typed, "b");
        assert_eq!(app.selected, 1);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn second_v_closes_split_and_preserves_left_pane() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.selected = 2;
        app.toggle_vertical_split()?;
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        app.selected = 1;
        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Char('v'), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.other.is_none());
        assert!(!app.active_right);
        assert_eq!(app.selected, 2);
        assert!(app.running);

        // Tab alone is harmless when split view is closed.
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.selected, 2);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn preview_command_opens_and_closes_independent_of_split() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.preview_mode);
        assert_eq!(app.preview.as_ref().unwrap().title, "alpha");

        app.toggle_vertical_split()?;
        assert!(app.preview_mode);
        assert_eq!(app.other.as_ref().unwrap().preview.as_ref().unwrap().title, "alpha");

        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.preview.as_ref().unwrap().title, "beta");
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.preview.as_ref().unwrap().title, "alpha");

        // Preview can be disabled while keeping both independent panes.
        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Char('p'), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.preview_mode);
        assert!(app.other.is_some());
        assert!(app.preview.is_none());
        assert!(app.other.as_ref().unwrap().preview.is_none());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn preview_updates_on_search_and_directory_navigation() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.toggle_preview();
        app.handle_key(key(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(app.preview.as_ref().unwrap().title, "beta");
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.cwd, root.join("beta"));
        assert!(app.preview.is_none());
        app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.cwd, root);
        assert_eq!(app.preview.as_ref().unwrap().title, "beta");
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn normal_command_resets_both_modes_without_quitting() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.toggle_vertical_split()?;
        app.toggle_preview();
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert!(app.active_right);
        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.running);
        assert!(app.other.is_none());
        assert!(!app.preview_mode);
        assert!(app.preview.is_none());
        assert!(!app.active_right);
        assert_eq!(app.cwd, root);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn esc_clears_marks_but_does_not_reset_preview_or_split() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.toggle_vertical_split()?;
        app.toggle_preview();
        app.handle_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.selection.count(), 0);
        assert!(app.preview_mode);
        assert!(app.other.is_some());
        assert!(app.running);
        fs::remove_dir_all(root)?;
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
