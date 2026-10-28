use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime},
};

mod archive;
mod commands;
mod config;
mod details;
mod fileops;
mod focus;
mod jobs;
mod jumps;
mod pane;
mod preview;
mod queue;
mod render;
mod selection;
mod tree;

use archive::ArchiveJob;
use commands::{Action, CommandAction, action_for, command_for, draw_help, help_max_scroll};
use config::{Config, MetadataPosition};
use details::permissions;
use fileops::{Transfer, TransferKind};
use focus::FocusHistory;
use jobs::{Poll as JobPoll, RunningJob, Work};
use jumps::Jumps;
use render::maximum_name_width;
use pane::{PaneState, PaneView, draw_pane_content, draw_split_pane, use_two_columns};
use preview::Preview;
use queue::WorkQueue;
use tree::{RowKind, TreeState};
use selection::Selection;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    DefaultTerminal, Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

/// Parse EDITOR/VISUAL as executable plus arguments, without a shell.
fn editor_argv(value: &str) -> io::Result<Vec<String>> {
    let args = shlex::split(value)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid EDITOR quoting"))?;
    if args.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "EDITOR is empty"));
    }
    Ok(args)
}

fn launch_editor(terminal: &mut DefaultTerminal, path: &Path) -> io::Result<()> {
    let configured = env::var("EDITOR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| env::var("VISUAL").ok().filter(|value| !value.trim().is_empty()))
        .unwrap_or_else(|| "vi".to_owned());
    let args = editor_argv(&configured)?;
    // Release the alternate screen and raw mode before an interactive editor.
    // Always reinitialize afterwards, even when the editor fails to spawn.
    ratatui::restore();
    let result = Command::new(&args[0]).args(&args[1..]).arg(path).status();
    *terminal = ratatui::init();
    terminal.clear()?;
    let status = result?;
    if !status.success() {
        return Err(io::Error::other(format!("Editor exited with {status}")));
    }
    Ok(())
}

fn mode_indicator(vertical: bool, preview: bool, tree: bool) -> Vec<Span<'static>> {
    let modes = [
        (vertical, "v", Color::Rgb(131, 165, 152)), // Gruvbox blue
        (preview, "p", Color::Rgb(250, 189, 47)),  // Gruvbox yellow
        (tree, "t", Color::Rgb(184, 187, 38)),      // Gruvbox green
    ];
    let mut spans = Vec::new();
    let mut first = true;
    for (enabled, name, color) in modes {
        if !enabled {
            continue;
        }
        if first {
            spans.push(Span::styled("[", Style::default().fg(Color::Rgb(146, 131, 116))));
            first = false;
        } else {
            spans.push(Span::styled(", ", Style::default().fg(Color::Rgb(146, 131, 116))));
        }
        spans.push(Span::styled(name, Style::default().fg(color)));
    }
    if !first {
        spans.push(Span::styled("]", Style::default().fg(Color::Rgb(146, 131, 116))));
    }
    spans
}

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
    jumps: Jumps,
    selected: usize,
    parent_focused: bool,
    focus_history: FocusHistory,
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
    pending_transfer: Option<Transfer>,
    status: Option<String>,
    pending_archive: Option<ArchiveJob>,
    help_open: bool,
    help_scroll: usize,
    help_view_height: u16,
    job: Option<RunningJob>,
    queue: WorkQueue,
    queue_open: bool,
    queue_selected: usize,
    pending_edit: Option<PathBuf>,
}

impl App {
    fn new() -> io::Result<Self> {
        let cwd = env::current_dir()?;
        let config = Config::load()?;
        let metadata_position = config.metadata_position;
        let jumps = Jumps::from_config(&config.jumps)?;
        let entries = read_directory(&cwd)?;
        let max_name_width = maximum_name_width(&entries);
        Ok(Self {
            cwd,
            entries,
            max_name_width,
            metadata_position,
            jumps,
            selected: 0,
            parent_focused: false,
            focus_history: FocusHistory::default(),
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
            pending_transfer: None,
            status: None,
            pending_archive: None,
            help_open: false,
            help_scroll: 0,
            help_view_height: 24,
            job: None,
            queue: WorkQueue::default(),
            queue_open: false,
            queue_selected: 0,
            pending_edit: None,
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
        // Load first: an inaccessible destination must not disturb the current
        // pane or overwrite its last remembered focus.
        let (tree, entries) = if self.tree_mode {
            let (tree, entries) = TreeState::open(&cwd)?;
            (Some(tree), entries)
        } else {
            (None, read_directory(&cwd)?)
        };

        // A parent navigation explicitly focuses the child directory; for
        // ordinary re-entry or jumps, restore the last file focused here.
        let remembered = self.focus_history.recall(&cwd);
        let explicit = focus.and_then(|path| entries.iter().position(|entry| entry.path == path));
        let restored = remembered.and_then(|state| entries.iter()
            .position(|entry| entry.path == state.entry));
        let selected = explicit.or(restored).unwrap_or(0);
        let scroll = if explicit.is_none() && restored.is_some() {
            remembered.map_or(0, |state| state.scroll)
        } else {
            0
        };

        // Save only real entries. Focusing [..] must not replace a previously
        // remembered file with the underlying zero-based list entry.
        if !self.parent_focused
            && self.tree.as_ref().is_none_or(|tree| tree.is_real(self.selected))
            && let Some(entry) = self.entries.get(self.selected)
        {
            self.focus_history.remember(&self.cwd, &entry.path, self.scroll);
        }

        self.cwd = cwd;
        self.entries = entries;
        self.max_name_width = maximum_name_width(&self.entries);
        self.tree = tree;
        self.selected = selected;
        self.parent_focused = false;
        self.selection.clear();
        self.scroll = scroll;
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

    /// Right arrow always enters the chosen directory, including in tree
    /// mode. Enter keeps the distinct tree expand/collapse behavior.
    fn enter_directory(&mut self) -> io::Result<()> {
        if self.parent_focused {
            return self.parent();
        }
        if let Some(entry) = self.entries.get(self.selected)
            && entry.is_dir
            && self.tree.as_ref().is_none_or(|tree| tree.is_real(self.selected))
        {
            return self.change_directory(entry.path.clone(), None);
        }
        Ok(())
    }

    fn open_selected(&mut self) -> io::Result<()> {
        if self.parent_focused {
            return self.parent();
        }
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
                        return Ok(());
                    }
                    // Files inside expanded tree branches are editable too.
                }
                _ => return Ok(()),
            }
        }
        if let Some(entry) = self.entries.get(self.selected) {
            if entry.is_dir {
                self.change_directory(entry.path.clone(), None)?;
            } else if entry.path.is_file() {
                self.pending_edit = Some(entry.path.clone());
                self.clear_typed();
            }
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

        // Two consecutive dots select the virtual parent row instead of
        // searching filenames. Keep the first dot as an incremental prefix
        // even when there are no dotfiles in the current directory.
        if prefix == ".." && self.cwd.parent().is_some() {
            self.selection.commit();
            self.parent_focused = true;
            self.typed = prefix;
            self.last_typed = Some(now);
            self.refresh_active_preview();
            return;
        }

        if let Some(index) = self.find_match(&prefix) {
            self.selection.commit();
            self.selected = index;
            self.parent_focused = false;
            self.refresh_active_preview();
            self.typed = prefix;
        } else {
            // If the longer prefix has no match, treat the last key as a new search.
            let single = ch.to_string();
            if let Some(index) = self.find_match(&single) {
                self.selection.commit();
                self.selected = index;
                self.parent_focused = false;
                self.refresh_active_preview();
                self.typed = single;
            } else if ch == '.' {
                // Allow a second '.' to form ".." even if no file matched
                // the first keypress.
                self.typed = ".".to_owned();
            } else {
                self.typed.clear();
            }
        }
        self.last_typed = Some(now);
    }

    fn move_cursor(&mut self, direction: i8, extend: bool) {
        if self.parent_focused {
            if direction > 0 && !self.entries.is_empty() {
                self.parent_focused = false;
                self.selected = 0;
                self.refresh_active_preview();
            }
            self.clear_typed();
            return;
        }
        if self.entries.is_empty() {
            if direction < 0 && self.cwd.parent().is_some() {
                self.parent_focused = true;
                self.refresh_active_preview();
            }
            self.clear_typed();
            return;
        }
        if direction < 0 && self.selected == 0 && self.cwd.parent().is_some() {
            self.selection.commit();
            self.parent_focused = true;
            self.refresh_active_preview();
            self.clear_typed();
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
        if !self.parent_focused
            && !self.entries.is_empty()
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
        std::mem::swap(&mut self.parent_focused, &mut other.parent_focused);
        std::mem::swap(&mut self.focus_history, &mut other.focus_history);
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
            let mut right = PaneState::open_mode(&self.cwd, self.tree_mode)?;
            if self.preview_mode {
                right.refresh_preview();
            }
            self.other = Some(right);
            self.active_right = false;
        }
        Ok(())
    }

    fn refresh_active_preview(&mut self) {
        if self.preview_mode {
            self.preview = if !self.parent_focused && self.tree.as_ref().is_none_or(|tree| tree.is_real(self.selected)) {
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
        if self.tree_mode && let Err(error) = self.toggle_tree() {
            self.error = Some(error.to_string());
        }
    }

    fn start_transfer(&mut self, kind: TransferKind) {
        if self.queue.len() >= queue::MAX_WAITING {
            self.error = Some("Job queue is full".to_owned());
            return;
        }
        let Some(other) = self.other.as_ref() else {
            self.error = Some("Open split view (:v) to copy or move files".to_string());
            return;
        };
        let marked = self.selection.count() > 0;
        let sources = self.entries.iter().enumerate()
            .filter(|(index, _)| {
                if marked { self.selection.contains(*index) } else { !self.parent_focused && *index == self.selected }
            })
            .filter(|(index, _)| self.tree.as_ref().is_none_or(|tree| tree.is_real(*index)))
            .map(|(_, entry)| entry.path.clone())
            .collect::<Vec<_>>();
        match Transfer::new(kind, sources, other.cwd.clone()) {
            Ok(transfer) => {
                self.pending_transfer = Some(transfer);
                self.error = None;
                self.status = None;
                self.clear_typed();
            }
            Err(error) => {
                self.error = Some(error.to_string());
                self.status = None;
            }
        }
    }

    fn refresh_after_transfer(&mut self) -> io::Result<()> {
        let focus = self.entries.get(self.selected).map(|entry| entry.path.clone());
        let entries = if let Some(tree) = self.tree.as_mut() {
            tree.rebuild(&self.cwd)?
        } else {
            read_directory(&self.cwd)?
        };
        self.replace_visible(entries, focus.as_deref(), self.selected);
        if let Some(other) = self.other.as_mut() {
            other.refresh_listing()?;
        }
        Ok(())
    }

    /// Confirmed jobs are snapshotted and processed in FIFO order.
    /// Exactly one worker runs at a time; queued work is never spawned early.
    fn begin_work(&mut self, work: Work) {
        if self.job.is_some() || !self.queue.is_empty() {
            match self.queue.push(work) {
                Ok(()) => {
                    self.status = Some(format!("Queued operation ({} waiting)", self.queue.len()));
                    self.error = None;
                    self.selection.clear();
                    self.clear_typed();
                    self.start_next_job();
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        } else {
            match RunningJob::start(work) {
                Ok(job) => {
                    self.job = Some(job);
                    self.status = None;
                    self.error = None;
                    self.selection.clear();
                    self.clear_typed();
                }
                Err(error) => self.error = Some(format!("Cannot start file operation: {error}")),
            }
        }
    }

    fn start_next_job(&mut self) {
        if self.job.is_some() {
            return;
        }
        // A failed worker launch should never strand the rest of the queue.
        while let Some(work) = self.queue.pop() {
            match RunningJob::start(work) {
                Ok(job) => {
                    self.job = Some(job);
                    break;
                }
                Err(error) => {
                    self.status = None;
                    self.error = Some(format!("Cannot start queued job: {error}"));
                }
            }
        }
        self.queue_selected = self.queue_selected.min(self.queue.len().saturating_sub(1));
    }

    fn poll_job(&mut self) -> bool {
        let Some(job) = self.job.as_ref() else {
            if !self.queue.is_empty() {
                self.start_next_job();
                return true;
            }
            return false;
        };
        let result = match job.poll() {
            JobPoll::Busy => return true,
            JobPoll::Finished(result) => result,
        };
        self.job = None;
        let refreshed = self.refresh_after_transfer();
        match (result, refreshed) {
            (Ok(status), Ok(())) => {
                self.status = Some(status);
                self.error = None;
            }
            (Err(error), _) | (_, Err(error)) => {
                self.status = None;
                self.error = Some(format!("File operation failed: {error}"));
            }
        }
        // A failed operation does not block later independent queued jobs.
        self.start_next_job();
        true
    }

    fn handle_queue_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('Q') => self.queue_open = false,
            KeyCode::Up | KeyCode::Char('k') => {
                self.queue_selected = self.queue_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.queue_selected = (self.queue_selected + 1)
                    .min(self.queue.len().saturating_sub(1));
            }
            KeyCode::Delete | KeyCode::Backspace
                if self.queue.remove(self.queue_selected).is_some() =>
            {
                self.status = Some("Removed waiting job from queue".to_owned());
                self.queue_selected = self.queue_selected.min(self.queue.len().saturating_sub(1));
            }
            _ => {}
        }
    }

    fn handle_transfer_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.pending_transfer = None,
            KeyCode::Enter => {
                if let Some(transfer) = self.pending_transfer.take() {
                    self.begin_work(Work::Transfer(transfer));
                }
            }
            _ => {}
        }
    }

    fn draw_transfer_dialog(&self, frame: &mut Frame, area: Rect) {
        let Some(transfer) = self.pending_transfer.as_ref() else {
            return;
        };
        let width = area.width.saturating_sub(2).min(76);
        let height = area.height.saturating_sub(2).min(9);
        if width < 10 || height < 4 {
            return;
        }
        let dialog = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let bg = Style::default().bg(Color::Rgb(40, 40, 40)).fg(Color::Rgb(235, 219, 178));
        let from = transfer.sources.first()
            .and_then(|path| path.parent())
            .unwrap_or(&self.cwd);
        let lines = vec![
            Line::from(format!("  {} {} item(s)", transfer.kind.verb(), transfer.sources.len())),
            Line::from(""),
            Line::from(format!("  From: {}", from.display())),
            Line::from(format!("  To:   {}", transfer.destination.display())),
            Line::from(""),
            Line::from(Span::styled(
                "  Enter: confirm    Esc: cancel",
                Style::default().fg(Color::Rgb(184, 187, 38)),
            )),
        ];
        let block = Block::default()
            .title(format!(" {} confirmation ", transfer.kind.verb()))
            .title_style(Style::default().fg(Color::Rgb(250, 189, 47)))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(146, 131, 116)))
            .style(bg);
        frame.render_widget(Clear, dialog);
        frame.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), dialog);
    }

    fn start_archive(&mut self, extract: bool) {
        if self.queue.len() >= queue::MAX_WAITING {
            self.error = Some("Job queue is full".to_owned());
            return;
        }
        let marked = self.selection.count() > 0;
        let sources = self.entries.iter().enumerate()
            .filter(|(index, _)| if marked { self.selection.contains(*index) } else { !self.parent_focused && *index == self.selected })
            .filter(|(index, _)| self.tree.as_ref().is_none_or(|tree| tree.is_real(*index)))
            .map(|(_, entry)| entry.path.clone())
            .collect::<Vec<_>>();
        let result = if extract {
            if sources.len() != 1 {
                Err(io::Error::new(io::ErrorKind::InvalidInput, "Select one archive to extract"))
            } else {
                ArchiveJob::new_unpack(sources[0].clone(), &self.cwd)
            }
        } else {
            ArchiveJob::new_pack(sources, &self.cwd)
        };
        match result {
            Ok(job) => {
                self.pending_archive = Some(job);
                self.error = None;
                self.status = None;
                self.clear_typed();
            }
            Err(error) => {
                self.status = None;
                self.error = Some(error.to_string());
            }
        }
    }

    fn handle_archive_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.pending_archive = None,
            KeyCode::Enter => {
                if let Some(job) = self.pending_archive.take() {
                    self.begin_work(Work::Archive(job));
                }
            }
            _ => {}
        }
    }

    fn draw_archive_dialog(&self, frame: &mut Frame, area: Rect) {
        let Some(job) = self.pending_archive.as_ref() else { return };
        let width = area.width.saturating_sub(2).min(76);
        let height = area.height.saturating_sub(2).min(9);
        if width < 12 || height < 4 { return; }
        let dialog = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width, height,
        );
        let lines = vec![
            Line::from(format!("  {}", job.summary())),
            Line::from(""),
            Line::from(format!("  Destination: {}", job.target().display())),
            Line::from(""),
            Line::styled(
                "  Enter: confirm     Esc: cancel",
                Style::default().fg(Color::Rgb(184, 187, 38)),
            ),
        ];
        let block = Block::default()
            .title(" Archive confirmation ")
            .title_style(Style::default().fg(Color::Rgb(250, 189, 47)))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(146, 131, 116)))
            .style(Style::default().bg(Color::Rgb(40, 40, 40)).fg(Color::Rgb(235, 219, 178)));
        frame.render_widget(Clear, dialog);
        frame.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), dialog);
    }

    fn draw_queue_dialog(&self, frame: &mut Frame, area: Rect) {
        if !self.queue_open || area.width < 24 || area.height < 7 {
            return;
        }
        let width = area.width.saturating_sub(2).min(92);
        let height = area.height.saturating_sub(2).min(22);
        let dialog = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let visible = usize::from(height.saturating_sub(6)).max(1);
        let start = if self.queue_selected >= visible {
            self.queue_selected + 1 - visible
        } else {
            0
        };
        let mut lines = Vec::new();
        if let Some(job) = &self.job {
            lines.push(Line::from(vec![
                Span::styled("  ▶ ACTIVE  ", Style::default().fg(Color::Rgb(184, 187, 38))),
                Span::styled(job.description.clone(), Style::default().fg(Color::Rgb(235, 219, 178))),
            ]));
        } else {
            lines.push(Line::styled("  No active job", Style::default().fg(Color::Rgb(146, 131, 116))));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("  WAITING  ({})", self.queue.len()),
            Style::default().fg(Color::Rgb(250, 189, 47)),
        ));
        if self.queue.is_empty() {
            lines.push(Line::styled("  (empty)", Style::default().fg(Color::Rgb(146, 131, 116))));
        }
        for (index, work) in self.queue.iter().enumerate().skip(start).take(visible) {
            let selected = index == self.queue_selected;
            let color = if selected { Color::Rgb(250, 189, 47) } else { Color::Rgb(189, 174, 147) };
            lines.push(Line::styled(
                format!(" {} {:>2}. {}", if selected { "▸" } else { " " }, index + 1, work.description()),
                Style::default().fg(color),
            ));
        }
        let block = Block::default()
            .title(" Jobs · ↑↓ select · Delete remove waiting · Esc close ")
            .title_style(Style::default().fg(Color::Rgb(250, 189, 47)))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(146, 131, 116)))
            .style(Style::default().bg(Color::Rgb(40, 40, 40)));
        frame.render_widget(Clear, dialog);
        frame.render_widget(Paragraph::new(lines).block(block), dialog);
    }

    fn draw_overlays(&self, frame: &mut Frame, area: Rect) {
        self.draw_transfer_dialog(frame, area);
        self.draw_archive_dialog(frame, area);
        self.draw_queue_dialog(frame, area);
        if self.help_open {
            draw_help(frame, area, self.help_scroll);
        }
    }

    fn jump_to_slot(&mut self, slot: u8) {
        let Some(destination) = self.jumps.get(slot).map(Path::to_path_buf) else {
            self.error = Some(format!("Jump {slot} is not set (:jset {slot})"));
            return;
        };
        // Fail without changing the pane if the jump was removed, became
        // unreadable or points to a file.
        match fs::canonicalize(&destination)
            .and_then(|path| {
                if !path.is_dir() {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "Jump target is not a directory"));
                }
                self.change_directory(path, None)
            })
        {
            Ok(()) => self.status = Some(format!("Jump {slot}")),
            Err(error) => self.error = Some(format!("Jump {slot}: {error}")),
        }
    }

    fn save_jump(&mut self, slot: u8) {
        match fs::canonicalize(&self.cwd).and_then(|directory| {
            if !directory.is_dir() {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "Current path is not a directory"));
            }
            Config::update_jump(slot, Some(&directory))?;
            Ok(directory)
        }) {
            Ok(directory) => {
                if let Err(error) = self.jumps.set(slot, directory) {
                    self.error = Some(error.to_string());
                    return;
                }
                self.error = None;
                self.status = Some(format!("Saved current directory as jump {slot}"));
            }
            Err(error) => self.error = Some(format!("Cannot save jump {slot}: {error}")),
        }
    }

    fn delete_jump(&mut self, slot: u8) {
        if self.jumps.get(slot).is_none() {
            self.error = Some(format!("Jump {slot} is not set"));
            return;
        }
        match Config::update_jump(slot, None) {
            Ok(()) => {
                self.jumps.remove(slot);
                self.error = None;
                self.status = Some(format!("Removed jump {slot}"));
            }
            Err(error) => self.error = Some(format!("Cannot remove jump {slot}: {error}")),
        }
    }

    fn handle_command_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.command = None,
            KeyCode::Enter => {
                let command = self.command.take().unwrap_or_default();
                match command_for(&command) {
                    Some(CommandAction::Quit) => {
                        if self.job.is_some() || !self.queue.is_empty() {
                            self.error = Some("Finish pending jobs before quitting".into());
                        } else {
                            self.running = false;
                        }
                    },
                    Some(CommandAction::Vertical) => {
                        if let Err(error) = self.toggle_vertical_split() {
                            self.error = Some(error.to_string());
                        }
                    }
                    Some(CommandAction::Preview) => self.toggle_preview(),
                    Some(CommandAction::Tree) => {
                        if let Err(error) = self.toggle_tree() {
                            self.error = Some(error.to_string());
                        }
                    }
                    Some(CommandAction::Normal) => self.normal_mode(),
                    Some(CommandAction::Copy) => self.start_transfer(TransferKind::Copy),
                    Some(CommandAction::Move) => self.start_transfer(TransferKind::Move),
                    Some(CommandAction::Archive) => self.start_archive(false),
                    Some(CommandAction::Extract) => self.start_archive(true),
                    Some(CommandAction::Jobs) => { self.queue_open = true; self.queue_selected = 0; },
                    Some(CommandAction::Jump(slot)) => self.jump_to_slot(slot),
                    Some(CommandAction::JumpSet(slot)) => self.save_jump(slot),
                    Some(CommandAction::JumpDelete(slot)) => self.delete_jump(slot),
                    None => self.error = Some(format!("unknown command: :{command}")),
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
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        if action_for(key) == Some(Action::Quit) {
            if self.job.is_some() || !self.queue.is_empty() {
                self.error = Some("Finish the running job before quitting".into());
            } else {
                self.running = false;
            }
            return;
        }

        if self.help_open {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') => self.help_open = false,
                KeyCode::Up | KeyCode::Char('k') => self.help_scroll = self.help_scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.help_scroll = (self.help_scroll + 1).min(help_max_scroll(self.help_view_height));
                }
                KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(8),
                KeyCode::PageDown => self.help_scroll = (self.help_scroll + 8).min(help_max_scroll(40)),
                _ => {}
            }
            return;
        }

        if self.queue_open {
            self.handle_queue_key(key);
            return;
        }

        if self.pending_archive.is_some() {
            self.handle_archive_key(key);
            return;
        }
        if self.pending_transfer.is_some() {
            self.handle_transfer_key(key);
            return;
        }
        if self.command.is_some() {
            self.handle_command_key(key);
            return;
        }

        match action_for(key) {
            Some(Action::Help) => { self.help_open = true; self.help_scroll = 0; self.clear_typed(); }
            Some(Action::Command) => { self.command = Some(String::new()); self.clear_typed(); self.error = None; }
            Some(Action::SwitchPane) => self.switch_pane(),
            Some(Action::Parent) => {
                if let Err(error) = self.parent() { self.error = Some(error.to_string()); }
            }
            Some(Action::EnterDirectory) => {
                if let Err(error) = self.enter_directory() { self.error = Some(error.to_string()); }
            }
            Some(Action::Open) => {
                if let Err(error) = self.open_selected() { self.error = Some(error.to_string()); }
            }
            Some(Action::Up) => self.move_cursor(-1, false),
            Some(Action::Down) => self.move_cursor(1, false),
            Some(Action::MarkUp) => self.move_cursor(-1, true),
            Some(Action::MarkDown) => self.move_cursor(1, true),
            Some(Action::Mark) => self.toggle_current(),
            Some(Action::Clear) => { self.selection.clear(); self.clear_typed(); }
            Some(Action::Copy) => self.start_transfer(TransferKind::Copy),
            Some(Action::Move) => self.start_transfer(TransferKind::Move),
            Some(Action::Archive) => self.start_archive(false),
            Some(Action::Extract) => self.start_archive(true),
            Some(Action::Jobs) => { self.queue_open = true; self.queue_selected = 0; self.clear_typed(); },
            Some(Action::Jump(slot)) => self.jump_to_slot(slot),
            Some(Action::Quit) => {
                if self.job.is_none() && self.queue.is_empty() { self.running = false; }
            },
            None => {
                if let KeyCode::Char(ch) = key.code
                    && !shift && !ctrl && !alt
                {
                    self.jump_to(ch);
                }
            }
        }
    }

    fn footer_line(&self) -> Line<'static> {
        let subdued = Style::default().fg(Color::Rgb(146, 131, 116));
        let bright = Style::default().fg(Color::Rgb(235, 219, 178));
        if let Some(job) = &self.job {
            let snapshot = job.progress.snapshot();
            let frames = ["◐", "◓", "◑", "◒"];
            let tick = (job.elapsed().as_millis() / 130) as usize % frames.len();
            let mut spans = vec![
                Span::styled(format!("  {} {}  ", frames[tick], job.label), Style::default().fg(Color::Rgb(250, 189, 47))),
            ];
            if let Some(percent) = snapshot.percent() {
                let filled = (percent as usize * 12 / 100).min(12);
                let bar = format!("[{}{}]", "█".repeat(filled), "░".repeat(12 - filled));
                spans.push(Span::styled(bar, Style::default().fg(Color::Rgb(184, 187, 38))));
                let detail = if snapshot.total_bytes > 0 {
                    format!(
                        " {percent:>3}%  {:.1}/{:.1} MiB",
                        snapshot.bytes as f64 / 1_048_576.0,
                        snapshot.total_bytes as f64 / 1_048_576.0,
                    )
                } else {
                    format!(" {percent:>3}%  {}/{} items", snapshot.items, snapshot.total)
                };
                spans.push(Span::styled(detail, bright));
            } else if snapshot.items > 0 {
                spans.push(Span::styled(format!("{} entries", snapshot.items), bright));
            } else {
                spans.push(Span::styled("working…", subdued));
            }
            if snapshot.bytes > 0 && snapshot.total_bytes == 0 {
                let amount = if snapshot.bytes >= 1_048_576 {
                    format!("{:.1} MiB", snapshot.bytes as f64 / 1_048_576.0)
                } else {
                    format!("{:.1} KiB", snapshot.bytes as f64 / 1024.0)
                };
                spans.push(Span::styled(format!("  ·  {amount} processed"), subdued));
            }
            spans.push(Span::styled(format!("  ·  {}s", job.elapsed().as_secs()), subdued));
            return Line::from(spans);
        }
        if let Some(error) = &self.error {
            return Line::from(vec![
                Span::styled("  ✕ ", Style::default().fg(Color::Rgb(251, 73, 52))),
                Span::styled(error.clone(), Style::default().fg(Color::Rgb(251, 73, 52))),
                Span::styled("   ·   ? help", subdued),
            ]);
        }
        if let Some(status) = &self.status {
            return Line::from(vec![
                Span::styled("  ✓ ", Style::default().fg(Color::Rgb(184, 187, 38))),
                Span::styled(status.clone(), bright),
                Span::styled("   ·   ? help", subdued),
            ]);
        }
        Line::from(vec![
            Span::styled("  ? help", Style::default().fg(Color::Rgb(250, 189, 47))),
            Span::styled("   : commands   ·   Shift+C copy   Shift+M move   Shift+A pack   Shift+E extract   ·   :jobs", subdued),
        ])
    }

    fn queue_footer_line(&self, width: u16) -> Line<'static> {
        let muted = Style::default().fg(Color::Rgb(146, 131, 116));
        let accent = Style::default().fg(Color::Rgb(250, 189, 47));
        let mut spans = vec![
            Span::styled(format!("  ◷ {} waiting", self.queue.len()), accent),
        ];
        if width >= 64 {
            let max_items = if width >= 116 { 3 } else { 1 };
            for (i, work) in self.queue.iter().take(max_items).enumerate() {
                let description = work.description();
                let summary = description.chars().take(if width >= 116 { 28 } else { 34 }).collect::<String>();
                spans.push(Span::styled(format!("   {}. {summary}", i + 1), muted));
            }
            if self.queue.len() > max_items {
                spans.push(Span::styled(format!("  +{}", self.queue.len() - max_items), muted));
            }
        }
        spans.push(Span::styled("   ·   :jobs", accent));
        Line::from(spans)
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let background = Style::default().bg(Color::Rgb(40, 40, 40));
        frame.render_widget(
            Paragraph::new(self.footer_line()).style(background),
            Rect::new(area.x, area.y, area.width, 1),
        );
        if area.height >= 2 {
            frame.render_widget(
                Paragraph::new(self.queue_footer_line(area.width)).style(background),
                Rect::new(area.x, area.y + 1, area.width, 1),
            );
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let screen = frame.area();
        self.help_view_height = screen.height;
        if screen.height == 0 || screen.width == 0 {
            return;
        }
        let footer_height = if !self.queue.is_empty() && screen.height >= 5 { 2 } else { 1 };
        self.draw_footer(frame, Rect::new(
            screen.x,
            screen.y + screen.height - footer_height,
            screen.width,
            footer_height,
        ));
        let area = Rect::new(screen.x, screen.y, screen.width, screen.height - footer_height);
        if area.height == 0 {
            return;
        }

        let split = self.other.is_some();
        let mut title = vec![Span::styled(
            " rzpk ",
            Style::default()
                .fg(Color::Rgb(126, 184, 238))
                .add_modifier(Modifier::BOLD),
        )];

        title.extend(mode_indicator(split, self.preview_mode, self.tree_mode));
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
            jumps: &self.jumps,
            entries: &self.entries,
            max_name_width: self.max_name_width,
            selected: self.selected,
            parent_focused: self.parent_focused,
            selection: &self.selection,
            scroll: &mut self.scroll,
            typed: &self.typed,
            error: self.error.as_deref(),
            preview: self.preview.as_ref(),
            tree_rows: self.tree.as_ref().map(|tree| tree.rows.as_slice()),
        };

        let Some(other) = self.other.as_mut() else {
            draw_pane_content(frame, content, &mut current, self.metadata_position, true, self.preview_mode);
            self.draw_overlays(frame, area);
            return;
        };
        let mut inactive = other.view(&self.jumps);

        if !use_two_columns(area.width) {
            draw_split_pane(frame, content, &mut current, self.metadata_position, true, self.preview_mode);
            self.draw_overlays(frame, area);
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
        self.draw_overlays(frame, area);
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
                        if let Some(path) = self.pending_edit.take() {
                            match launch_editor(terminal, &path) {
                                Ok(()) => {
                                    if let Err(error) = self.refresh_after_transfer() {
                                        self.error = Some(format!("Cannot refresh after editing: {error}"));
                                    }
                                }
                                Err(error) => self.error = Some(format!("Cannot open editor: {error}")),
                            }
                        }
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
            if self.poll_job() {
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
            jumps: Jumps::default(),
            entries,
            selected: 0,
            parent_focused: false,
            focus_history: FocusHistory::default(),
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
            pending_transfer: None,
            status: None,
            pending_archive: None,
            help_open: false,
            help_scroll: 0,
            help_view_height: 24,
            job: None,
            queue: WorkQueue::default(),
            queue_open: false,
            queue_selected: 0,
            pending_edit: None,
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
    fn complete_job(app: &mut App) -> io::Result<()> {
        let started = Instant::now();
        while app.job.is_some() || !app.queue.is_empty() {
            app.poll_job();
            if started.elapsed() > Duration::from_secs(10) {
                return Err(io::Error::other("Test worker did not finish"));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
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
    fn modes_use_single_neutral_brackets_and_commas() {
        let all = mode_indicator(true, true, true);
        let label = all.iter().map(|span| span.content.as_ref()).collect::<String>();
        assert_eq!(label, "[v, p, t]");
        assert_eq!(
            mode_indicator(false, true, false)
                .iter().map(|span| span.content.as_ref()).collect::<String>(),
            "[p]"
        );
        assert!(mode_indicator(false, false, false).is_empty());
        let brackets = Color::Rgb(146, 131, 116);
        assert_eq!(all.first().unwrap().style.fg, Some(brackets));
        assert_eq!(all.last().unwrap().style.fg, Some(brackets));
        assert_eq!(all[2].style.fg, Some(brackets)); // comma
        assert_ne!(all[1].style.fg, Some(brackets)); // v
        assert_ne!(all[3].style.fg, Some(brackets)); // p
        assert_ne!(all[5].style.fg, Some(brackets)); // t
    }

    #[test]
    fn tree_command_toggles_and_expands_directory() -> io::Result<()> {
        let root = split_fixture()?;
        fs::write(root.join("alpha").join("nested.txt"), b"inside")?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Char('t'), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.tree_mode);
        assert!(app.tree.is_some());
        // The first two levels start open automatically.
        assert_eq!(app.entries.len(), 4);
        assert_eq!(app.entries[0].name, "alpha");
        assert_eq!(app.entries[1].name, "nested.txt");

        // Enter now collapses the already expanded branch.
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.entries.len(), 3);
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.entries.len(), 4);
        assert_eq!(app.entries[1].name, "nested.txt");
        assert_eq!(app.cwd, root);

        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selected, 1);
        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Char('t'), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.tree_mode);
        assert!(app.tree.is_none());
        assert_eq!(app.entries.len(), 3);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn tree_mode_works_per_split_pane_and_with_preview() -> io::Result<()> {
        let root = split_fixture()?;
        fs::write(root.join("alpha").join("child.txt"), b"hello")?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.toggle_vertical_split()?;
        app.toggle_tree()?;
        app.toggle_preview();
        assert!(app.tree_mode && app.preview_mode);
        assert!(app.other.as_ref().unwrap().tree.is_some());
        // Both independent panes start expanded.
        assert_eq!(app.entries.len(), 4);
        assert_eq!(app.other.as_ref().unwrap().entries.len(), 4);
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.entries.len(), 3); // collapse only the left pane

        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert!(app.active_right);
        assert_eq!(app.entries.len(), 4); // right stays expanded
        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.preview.as_ref().unwrap().title, "child.txt");
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.entries.len(), 3);
        assert_eq!(app.selected, 0);
        assert_eq!(app.preview.as_ref().unwrap().title, "alpha");
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn normal_command_resets_tree_split_and_preview() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.toggle_tree()?;
        app.toggle_vertical_split()?;
        app.toggle_preview();
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.other.is_none());
        assert!(app.tree.is_none());
        assert!(!app.tree_mode);
        assert!(!app.preview_mode);
        assert!(!app.active_right);
        assert!(app.running);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    fn transfer_fixture() -> io::Result<(App, PathBuf)> {
        let root = split_fixture()?;
        let left = root.join("left");
        let right = root.join("right");
        fs::create_dir_all(&left)?;
        fs::create_dir_all(&right)?;
        fs::write(left.join("copy.txt"), "example")?;
        fs::write(left.join("second.rs"), "fn main() {}")?;
        let mut app = app(read_directory(&left)?);
        app.cwd = left;
        app.toggle_vertical_split()?;
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        app.change_directory(right, None)?;
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        Ok((app, root))
    }

    #[test]
    fn shift_c_copies_marked_files_after_confirmation() -> io::Result<()> {
        let (mut app, root) = transfer_fixture()?;
        app.selection.toggle(0);
        app.selection.toggle(1);
        app.handle_key(key(KeyCode::Char('C'), KeyModifiers::SHIFT));
        assert_eq!(app.pending_transfer.as_ref().unwrap().sources.len(), 2);
        assert!(app.typed.is_empty());
        assert!(!root.join("right/copy.txt").exists());
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.pending_transfer.is_none());
        assert!(app.job.is_some());
        complete_job(&mut app)?;
        assert_eq!(fs::read_to_string(root.join("right/copy.txt"))?, "example");
        assert!(root.join("right/second.rs").exists());
        assert!(root.join("left/copy.txt").exists());
        assert_eq!(app.selection.count(), 0);
        assert_eq!(app.status.as_deref(), Some("Copied 2 item(s)"));
        assert_eq!(app.other.as_ref().unwrap().entries.len(), 2);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn shift_m_moves_from_right_to_left() -> io::Result<()> {
        let (mut app, root) = transfer_fixture()?;
        fs::write(root.join("right/from_right.txt"), "right")?;
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        app.other.as_mut().unwrap().refresh_listing()?;
        app.change_directory(root.join("right"), None)?;
        app.handle_key(key(KeyCode::Char('m'), KeyModifiers::SHIFT));
        assert_eq!(app.pending_transfer.as_ref().unwrap().kind, TransferKind::Move);
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        complete_job(&mut app)?;
        assert!(!root.join("right/from_right.txt").exists());
        assert_eq!(fs::read_to_string(root.join("left/from_right.txt"))?, "right");
        assert!(app.active_right);
        assert_eq!(app.status.as_deref(), Some("Moved 1 item(s)"));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn escape_cancels_pending_copy_but_keeps_selection() -> io::Result<()> {
        let (mut app, root) = transfer_fixture()?;
        app.selection.toggle(0);
        app.handle_key(key(KeyCode::Char('c'), KeyModifiers::SHIFT));
        assert!(app.pending_transfer.is_some());
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.pending_transfer.is_none());
        assert_eq!(app.selection.count(), 1);
        assert!(!root.join("right/copy.txt").exists());
        assert!(app.running);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn transfer_rejects_existing_target_without_confirmation() -> io::Result<()> {
        let (mut app, root) = transfer_fixture()?;
        fs::write(root.join("right/copy.txt"), "original")?;
        app.handle_key(key(KeyCode::Char('C'), KeyModifiers::NONE));
        assert!(app.pending_transfer.is_none());
        assert!(app.error.as_deref().unwrap().contains("already exists"));
        assert_eq!(fs::read_to_string(root.join("right/copy.txt"))?, "original");
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn typing_lowercase_letters_still_searches_instead_of_copying() -> io::Result<()> {
        let (mut app, root) = transfer_fixture()?;
        app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE));
        assert_eq!(app.entries[app.selected].name, "second.rs");
        app.handle_key(key(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(app.pending_transfer.is_none());
        assert!(app.running);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn copy_requires_split_view() {
        let mut app = app(vec![sample("hello.txt", false)]);
        app.handle_key(key(KeyCode::Char('C'), KeyModifiers::SHIFT));
        assert!(app.pending_transfer.is_none());
        assert!(app.error.as_deref().unwrap().contains(":v"));
    }

    #[test]
    fn selected_tree_items_are_not_confused_with_more_rows() -> io::Result<()> {
        let (mut app, root) = transfer_fixture()?;
        let dir = root.join("left").join("folder");
        fs::create_dir(&dir)?;
        fs::write(dir.join("child.txt"), "hello")?;
        app.toggle_tree()?;
        let folder_index = app.entries.iter().position(|entry| entry.name == "folder").unwrap();
        app.selected = folder_index;
        app.selection.toggle(folder_index);
        let child_index = app.entries.iter().position(|entry| entry.name == "child.txt").unwrap();
        app.selection.toggle(child_index);
        app.handle_key(key(KeyCode::Char('C'), KeyModifiers::SHIFT));
        assert_eq!(app.pending_transfer.as_ref().unwrap().sources.len(), 1);
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        complete_job(&mut app)?;
        assert!(root.join("right/folder/child.txt").exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn help_opens_scrolls_and_esc_only_closes_help() {
        let mut app = app(vec![sample("file.txt", false)]);
        app.selection.toggle(0);
        app.handle_key(key(KeyCode::Char('?'), KeyModifiers::SHIFT));
        assert!(app.help_open);
        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.help_scroll, 1);
        app.handle_key(key(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(app.typed.is_empty());
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.help_open);
        assert_eq!(app.selection.count(), 1);
        assert!(app.running);
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.selection.count(), 0);
    }

    #[test]
    fn archive_shortcut_confirmation_and_extraction_roundtrip() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        // Archive the explicitly marked notes.txt, not the focused folder.
        let index = app.entries.iter().position(|entry| entry.name == "notes.txt").unwrap();
        app.selection.toggle(index);
        app.handle_key(key(KeyCode::Char('A'), KeyModifiers::SHIFT));
        assert!(app.pending_archive.is_some());
        assert!(!root.join("notes.txt.tar.gz").exists());
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.job.is_some());
        complete_job(&mut app)?;
        assert_eq!(app.status.as_deref(), Some("Created notes.txt.tar.gz"));
        assert!(root.join("notes.txt.tar.gz").exists());
        assert!(root.join("notes.txt").exists());
        assert_eq!(app.selection.count(), 0);

        // Shift+E extracts into a new folder.
        let index = app.entries.iter().position(|entry| entry.name == "notes.txt.tar.gz").unwrap();
        app.selected = index;
        app.handle_key(key(KeyCode::Char('e'), KeyModifiers::SHIFT));
        assert!(app.pending_archive.is_some());
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        complete_job(&mut app)?;
        assert_eq!(app.status.as_deref(), Some("Extracted to notes.txt-extracted"));
        assert_eq!(fs::read_to_string(root.join("notes.txt-extracted/notes.txt"))?, "sample");
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn archive_dialog_escape_cancels_without_writing() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        let index = app.entries.iter().position(|entry| entry.name == "notes.txt").unwrap();
        app.selected = index;
        app.handle_key(key(KeyCode::Char('a'), KeyModifiers::SHIFT));
        assert!(app.pending_archive.is_some());
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.pending_archive.is_none());
        assert!(!root.join("notes.txt.tar.gz").exists());
        assert!(app.running);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn archive_commands_are_registered_and_lowercase_remains_search() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(app.pending_archive.is_none());
        assert_eq!(app.entries[app.selected].name, "notes.txt");
        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        for ch in "archive".chars() {
            app.handle_key(key(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.pending_archive.is_some());
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn footer_has_idle_hints_and_error_feedback() {
        let mut app = app(vec![]);
        let idle = app.footer_line().spans.iter().map(|span| span.content.as_ref()).collect::<String>();
        assert!(idle.contains("? help"));
        app.error = Some("Permission denied".to_string());
        let error = app.footer_line().spans.iter().map(|span| span.content.as_ref()).collect::<String>();
        assert!(error.contains("Permission denied"));
    }

    #[test]
    fn job_runs_without_blocking_navigation_and_second_operation_is_queued() -> io::Result<()> {
        let (mut app, root) = transfer_fixture()?;
        app.handle_key(key(KeyCode::Char('C'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.job.is_some());
        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selected, 1);
        app.handle_key(key(KeyCode::Char('A'), KeyModifiers::SHIFT));
        assert!(app.pending_archive.is_some());
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.queue.len(), 1);
        assert!(app.queue.iter().next().unwrap().description().contains("second.rs"));
        let queue_line = app.queue_footer_line(120);
        assert!(queue_line.spans.iter().any(|span| span.content.contains("1 waiting")));
        complete_job(&mut app)?;
        assert!(root.join("right/copy.txt").exists());
        assert!(root.join("left/second.rs.tar.gz").exists());
        assert!(app.queue.is_empty());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn jobs_modal_removes_waiting_job_without_canceling_active_worker() -> io::Result<()> {
        let (mut app, root) = transfer_fixture()?;
        // Running copy.
        app.handle_key(key(KeyCode::Char('C'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.job.is_some());

        // First queued job: archive second.rs.
        app.selected = 1;
        app.handle_key(key(KeyCode::Char('A'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.queue.len(), 1);

        // Second queued job: copy second.rs.
        app.handle_key(key(KeyCode::Char('C'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.queue.len(), 2);

        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        for ch in "jobs".chars() { app.handle_key(key(KeyCode::Char(ch), KeyModifiers::NONE)); }
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.queue_open);
        app.handle_key(key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(app.queue.len(), 1);
        assert!(app.queue.iter().next().unwrap().description().contains("Copy"));
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.queue_open);

        complete_job(&mut app)?;
        assert!(root.join("right/copy.txt").exists());
        assert!(root.join("right/second.rs").exists());
        assert!(!root.join("left/second.rs.tar.gz").exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn queue_keeps_target_directory_snapshot_after_pane_navigation() -> io::Result<()> {
        let (mut app, root) = transfer_fixture()?;
        app.handle_key(key(KeyCode::Char('C'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        app.selected = 1;
        app.handle_key(key(KeyCode::Char('C'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.queue.len(), 1);
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        app.change_directory(root.join("beta"), None)?;
        complete_job(&mut app)?;
        assert!(root.join("right/second.rs").exists());
        assert!(!root.join("beta/second.rs").exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn failed_waiting_job_does_not_prevent_following_jobs() -> io::Result<()> {
        let (mut app, root) = transfer_fixture()?;
        // First copy runs, second references a file removed before it starts,
        // and the third packs a different file. No timing-sensitive collisions.
        app.handle_key(key(KeyCode::Char('C'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        app.selected = 1;
        app.handle_key(key(KeyCode::Char('C'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        app.selected = 0;
        app.handle_key(key(KeyCode::Char('A'), KeyModifiers::SHIFT));
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.queue.len(), 2);
        fs::remove_file(root.join("left/second.rs"))?;
        complete_job(&mut app)?;
        assert_eq!(fs::read_to_string(root.join("right/copy.txt"))?, "example");
        assert!(!root.join("right/second.rs").exists());
        assert!(root.join("left/copy.txt.tar.gz").exists());
        assert!(app.queue.is_empty());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn parent_row_is_focusable_and_enter_goes_back() -> io::Result<()> {
        let root = split_fixture()?;
        let child = root.join("alpha");
        let mut app = app(read_directory(&child)?);
        app.cwd = child.clone();

        // Even empty directories have a selectable [..] above their content.
        assert!(app.entries.is_empty());
        app.handle_key(key(KeyCode::Up, KeyModifiers::NONE));
        assert!(app.parent_focused);
        app.handle_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(app.selection.count(), 0);
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.cwd, root);
        assert!(!app.parent_focused);
        assert_eq!(app.entries[app.selected].name, "alpha");
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn typing_double_dot_focuses_parent_and_enter_navigates_up() -> io::Result<()> {
        let root = split_fixture()?;
        let child = root.join("alpha");
        let mut app = app(read_directory(&child)?);
        app.cwd = child;

        app.handle_key(key(KeyCode::Char('.'), KeyModifiers::NONE));
        assert_eq!(app.typed, ".");
        assert!(!app.parent_focused);

        app.handle_key(key(KeyCode::Char('.'), KeyModifiers::NONE));
        assert_eq!(app.typed, "..");
        assert!(app.parent_focused);

        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.cwd, root);
        assert!(!app.parent_focused);
        assert_eq!(app.entries[app.selected].name, "alpha");
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn double_dot_works_even_when_hidden_files_match_first_dot() -> io::Result<()> {
        let root = split_fixture()?;
        fs::write(root.join(".hidden"), "secret")?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();

        app.handle_key(key(KeyCode::Char('.'), KeyModifiers::NONE));
        assert_eq!(app.entries[app.selected].name, ".hidden");
        assert!(!app.parent_focused);

        app.handle_key(key(KeyCode::Char('.'), KeyModifiers::NONE));
        assert!(app.parent_focused);
        // Ordinary name search from the parent row still works.
        app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(!app.parent_focused);
        assert_eq!(app.entries[app.selected].name, "notes.txt");
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn double_dot_is_pane_local_and_works_in_tree_mode() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.toggle_tree()?;
        app.toggle_vertical_split()?;

        for _ in 0..2 {
            app.handle_key(key(KeyCode::Char('.'), KeyModifiers::NONE));
        }
        assert!(app.parent_focused);
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert!(!app.parent_focused);
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert!(app.parent_focused);
        assert_eq!(app.cwd, root);

        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn double_dot_respects_search_timeout_and_filesystem_root() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();

        app.handle_key(key(KeyCode::Char('.'), KeyModifiers::NONE));
        app.last_typed = Some(Instant::now() - TYPE_TIMEOUT - Duration::from_millis(1));
        app.handle_key(key(KeyCode::Char('.'), KeyModifiers::NONE));
        assert!(!app.parent_focused);
        assert_eq!(app.typed, ".");

        app.cwd = PathBuf::from("/");
        app.handle_key(key(KeyCode::Char('.'), KeyModifiers::NONE));
        assert!(!app.parent_focused);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn left_right_keys_navigate_directories_without_search() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        let before = app.entries[app.selected].path.clone();
        app.handle_key(key(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.cwd, before);
        app.handle_key(key(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.cwd, root);
        assert_eq!(app.entries[app.selected].name, "alpha");
        assert!(app.typed.is_empty());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn parent_row_does_not_interfere_with_marks_or_incremental_search() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.handle_key(key(KeyCode::Up, KeyModifiers::NONE));
        assert!(app.parent_focused);
        app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(!app.parent_focused);
        assert_eq!(app.entries[app.selected].name, "notes.txt");
        app.handle_key(key(KeyCode::Up, KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Up, KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Up, KeyModifiers::NONE));
        assert!(app.parent_focused);
        app.handle_key(key(KeyCode::Char('C'), KeyModifiers::SHIFT));
        assert!(app.pending_transfer.is_none());
        assert!(app.error.as_deref().unwrap().contains(":v"));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn enter_on_regular_file_requests_editor_in_normal_and_tree_views() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        let index = app.entries.iter().position(|entry| entry.name == "notes.txt").unwrap();
        app.selected = index;
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.pending_edit.as_deref(), Some(root.join("notes.txt").as_path()));
        app.pending_edit = None;
        app.toggle_tree()?;
        let index = app.entries.iter().position(|entry| entry.name == "notes.txt").unwrap();
        app.selected = index;
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.pending_edit.as_deref(), Some(root.join("notes.txt").as_path()));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn split_panes_keep_independent_parent_focus_and_navigation() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.toggle_vertical_split()?;
        app.handle_key(key(KeyCode::Up, KeyModifiers::NONE));
        assert!(app.parent_focused);
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert!(!app.parent_focused);
        app.handle_key(key(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.cwd, root.join("alpha"));
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert!(app.parent_focused);
        assert_eq!(app.cwd, root);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn tree_mode_parent_row_and_right_arrow_work_independently() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.toggle_tree()?;
        app.handle_key(key(KeyCode::Up, KeyModifiers::NONE));
        assert!(app.parent_focused);
        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
        assert!(!app.parent_focused);
        assert_eq!(app.entries[app.selected].name, "alpha");
        app.handle_key(key(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.cwd, root.join("alpha"));
        assert!(app.tree.is_some());
        app.handle_key(key(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.cwd, root);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn editor_argument_parsing_is_safe_and_handles_spaces() -> io::Result<()> {
        let args = editor_argv("nvim -u 'some config.lua' --")?;
        assert_eq!(args, vec!["nvim", "-u", "some config.lua", "--"]);
        assert!(editor_argv(" ").is_err());
        assert!(editor_argv("nvim 'unclosed").is_err());
        Ok(())
    }

    #[test]
    fn jumping_changes_only_active_pane_and_keeps_other_state() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.jumps.set(1, root.join("alpha"))?;
        app.jumps.set(2, root.join("beta"))?;
        app.toggle_vertical_split()?;
        app.selection.toggle(1);

        app.handle_key(key(KeyCode::Char('1'), KeyModifiers::ALT));
        assert_eq!(app.cwd, root.join("alpha"));
        assert!(app.entries.is_empty());
        assert_eq!(app.selection.count(), 0);
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.cwd, root);
        assert_eq!(app.selection.count(), 0);
        app.handle_key(key(KeyCode::Char('2'), KeyModifiers::SUPER));
        assert_eq!(app.cwd, root.join("beta"));
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.cwd, root.join("alpha"));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn jumps_preserve_tree_and_preview_modes() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.jumps.set(3, root.join("alpha"))?;
        app.toggle_tree()?;
        app.toggle_preview();
        app.handle_key(key(KeyCode::Char('3'), KeyModifiers::ALT));
        assert_eq!(app.cwd, root.join("alpha"));
        assert!(app.tree.is_some());
        assert!(app.tree_mode);
        assert!(app.preview_mode);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn missing_jump_and_deleted_directory_do_not_change_pane() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.jump_to_slot(9);
        assert_eq!(app.cwd, root);
        assert!(app.error.as_deref().unwrap().contains("not set"));
        let vanished = root.join("vanished");
        app.jumps.set(9, vanished)?;
        app.jump_to_slot(9);
        assert_eq!(app.cwd, root);
        assert!(app.error.as_deref().unwrap().contains("Jump 9"));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn colon_jump_invocation_does_not_start_search() -> io::Result<()> {
        let root = split_fixture()?;
        let mut app = app(read_directory(&root)?);
        app.cwd = root.clone();
        app.jumps.set(4, root.join("beta"))?;
        app.handle_key(key(KeyCode::Char(':'), KeyModifiers::SHIFT));
        for ch in "j4".chars() {
            app.handle_key(key(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.cwd, root.join("beta"));
        assert!(app.typed.is_empty());
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
