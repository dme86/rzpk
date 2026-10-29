//! Independent directory view state and shared pane rendering.
use std::{
    io,
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    Entry, read_directory,
    preview::{Preview, draw_preview},
    config::MetadataPosition,
    jumps::Jumps,
    focus::FocusHistory,
    remote::Remote,
    details::Columns,
    render::{EntryHighlight, entry_line, maximum_name_width},
    selection::Selection,
    tree::{TreeRow, TreeState, RowKind},
};

pub struct PaneState {
    pub cwd: PathBuf,
    pub entries: Vec<Entry>,
    pub max_name_width: usize,
    pub selected: usize,
    pub parent_focused: bool,
    pub focus_history: FocusHistory,
    pub selection: Selection,
    pub scroll: usize,
    pub typed: String,
    pub last_typed: Option<Instant>,
    pub error: Option<String>,
    pub preview: Option<Preview>,
    pub tree: Option<TreeState>,
    pub remote: Option<std::sync::Arc<Remote>>,
}

impl PaneState {
    /// A tree pane starts from a bounded first page, never a full directory scan.
    pub fn open_mode(cwd: &Path, tree_enabled: bool) -> io::Result<Self> {
        let (tree, entries) = if tree_enabled {
            let (tree, entries) = TreeState::open(cwd)?;
            (Some(tree), entries)
        } else {
            (None, read_directory(cwd)?)
        };
        let max_name_width = maximum_name_width(&entries);
        Ok(Self {
            cwd: cwd.to_path_buf(),
            entries,
            max_name_width,
            selected: 0,
            parent_focused: false,
            focus_history: FocusHistory::default(),
            selection: Selection::default(),
            scroll: 0,
            typed: String::new(),
            last_typed: None,
            error: None,
            preview: None,
            tree,
            remote: None,
        })
    }

    pub fn view<'a>(&'a mut self, jumps: &'a Jumps) -> PaneView<'a> {
        PaneView {
            cwd: &self.cwd,
            remote_name: self.remote.as_ref().map(|remote| remote.name.as_str()),
            jumps,
            entries: &self.entries,
            max_name_width: self.max_name_width,
            selected: self.selected,
            parent_focused: self.parent_focused,
            selection: &self.selection,
            scroll: &mut self.scroll,
            typed: &self.typed,
            error: self.error.as_deref(),
            preview: self.preview.as_mut(),
            tree_rows: self.tree.as_ref().map(|tree| tree.rows.as_slice()),
        }
    }

    pub fn enable_tree_with_entries(&mut self, tree: TreeState, entries: Vec<Entry>) {
        let focus = self.entries.get(self.selected).map(|entry| entry.path.clone());
        self.replace_entries(entries, focus.as_deref());
        self.tree = Some(tree);
    }

    pub fn disable_tree_with_entries(&mut self, entries: Vec<Entry>) {
        let focus = self.entries.get(self.selected).map(|entry| entry.path.clone());
        self.replace_entries(entries, focus.as_deref());
        self.tree = None;
    }

    fn replace_entries(&mut self, entries: Vec<Entry>, focus: Option<&Path>) {
        let marked = self.entries.iter().enumerate()
            .filter(|(index, _)| self.selection.contains(*index)
                && self.tree.as_ref().is_none_or(|tree| tree.is_real(*index)))
            .map(|(_, entry)| entry.path.clone()).collect::<Vec<_>>();
        self.max_name_width = maximum_name_width(&entries);
        self.selected = focus.and_then(|path| entries.iter().position(|entry| entry.path == path)).unwrap_or(0);
        self.entries = entries;
        self.selection.clear();
        for (index, entry) in self.entries.iter().enumerate() {
            if marked.contains(&entry.path) {
                self.selection.toggle(index);
            }
        }
        self.scroll = 0;
        self.typed.clear();
        self.last_typed = None;
        self.refresh_preview();
    }

    pub fn refresh_listing(&mut self) -> io::Result<()> {
        let focus = self.entries.get(self.selected).map(|entry| entry.path.clone());
        let entries = if let Some(tree) = self.tree.as_mut() {
            tree.rebuild(&self.cwd)?
        } else {
            read_directory(&self.cwd)?
        };
        self.replace_entries(entries, focus.as_deref());
        Ok(())
    }

    pub fn refresh_preview(&mut self) {
        self.preview = if !self.parent_focused && self.tree.as_ref().is_none_or(|tree| tree.is_real(self.selected)) {
            Preview::selected(&self.entries, self.selected)
        } else {
            None
        };
    }

    pub fn expire_search(&mut self, timeout: std::time::Duration) -> bool {
        if self.last_typed.is_some_and(|time| time.elapsed() >= timeout) {
            self.typed.clear();
            self.last_typed = None;
            true
        } else {
            false
        }
    }
}

pub struct PaneView<'a> {
    pub cwd: &'a Path,
    pub remote_name: Option<&'a str>,
    pub jumps: &'a Jumps,
    pub entries: &'a [Entry],
    pub max_name_width: usize,
    pub selected: usize,
    pub parent_focused: bool,
    pub selection: &'a Selection,
    pub scroll: &'a mut usize,
    pub typed: &'a str,
    pub error: Option<&'a str>,
    pub preview: Option<&'a mut Preview>,
    pub tree_rows: Option<&'a [TreeRow]>,
}

pub fn draw_pane_header(frame: &mut Frame, area: Rect, view: &PaneView<'_>, active: bool) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let mut spans = vec![
        Span::styled(
            if active { " ▸ " } else { "   " },
            Style::default().fg(Color::Rgb(93, 203, 165)),
        ),
        Span::styled(
            view.remote_name.map(|name| format!("SFTP · {name}:{}", view.cwd.display()))
                .unwrap_or_else(|| view.cwd.display().to_string()),
            Style::default().fg(if active {
                Color::Rgb(218, 226, 236)
            } else {
                Color::Rgb(115, 129, 145)
            }),
        ),
    ];
    if view.selection.count() > 0 {
        spans.push(Span::styled(
            format!("  {} marked", view.selection.count()),
            Style::default().fg(Color::Rgb(91, 205, 160)),
        ));
    }
    if !view.typed.is_empty() {
        spans.push(Span::styled(
            format!("  /{}", view.typed),
            Style::default().fg(Color::Yellow),
        ));
    }
    if let Some(error) = view.error {
        spans.push(Span::styled(
            format!("  {error}"),
            Style::default().fg(Color::LightRed),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub fn draw_entries(
    frame: &mut Frame,
    area: Rect,
    view: &mut PaneView<'_>,
    metadata_position: MetadataPosition,
    active: bool,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    // The parent row is virtual and pinned to the top: it never participates
    // in marks, fuzzy search, tree pagination, or filesystem operations.
    let has_parent = view.cwd.parent().is_some();
    let mut listing = area;
    if has_parent {
        let selected = active && view.parent_focused;
        let background = if selected {
            Style::default().bg(Color::Rgb(36, 51, 69))
        } else {
            Style::default()
        };
        let marker = if selected { "▸ " } else { "  " };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Rgb(184, 187, 38))),
                Span::styled("[..]", Style::default().fg(Color::Rgb(131, 165, 152))),
            ])).style(background),
            Rect::new(area.x, area.y, area.width, 1),
        );
        listing.y = listing.y.saturating_add(1);
        listing.height = listing.height.saturating_sub(1);
    }
    if listing.height == 0 || listing.width == 0 {
        return;
    }
    if view.entries.is_empty() {
        frame.render_widget(
            Paragraph::new("  (empty directory)").style(Style::default().fg(Color::DarkGray)),
            listing,
        );
        return;
    }

    let height = usize::from(listing.height);
    if view.selected < *view.scroll {
        *view.scroll = view.selected;
    } else if view.selected >= view.scroll.saturating_add(height) {
        *view.scroll = view.selected + 1 - height;
    }

    let columns = Columns::for_width(listing.width);
    let now = SystemTime::now();
    for (row, entry) in view
        .entries
        .iter()
        .skip(*view.scroll)
        .take(height)
        .enumerate()
    {
        let index = *view.scroll + row;
        let focused = active && !view.parent_focused && index == view.selected;
        let marked = view.selection.contains(index);
        let background = match (focused, marked) {
            (true, true) => Style::default().bg(Color::Rgb(37, 75, 65)),
            (true, false) => Style::default().bg(Color::Rgb(36, 51, 69)),
            (false, true) => Style::default().bg(Color::Rgb(27, 53, 48)),
            (false, false) => Style::default(),
        };
        let mut line = entry_line(
            entry,
            EntryHighlight { focused, marked },
            if view.tree_rows.is_some() { Columns::Names } else { columns },
            metadata_position,
            view.max_name_width,
            listing.width,
            now,
        );
        if let Some(tree_row) = view.tree_rows.and_then(|rows| rows.get(index)) {
            // Insert an indented branch after the selection checkmark.
            let branch_color = Color::Rgb(146, 131, 116);
            let label = match &tree_row.kind {
                RowKind::More(_) => "… ",
                RowKind::Limit => "· ",
                RowKind::Entry { expanded: true } if entry.is_dir => "▾ ",
                RowKind::Entry { .. } if entry.is_dir => "▸ ",
                RowKind::Entry { .. } => "  ",
            };
            if !tree_row.real_entry() {
                line = Line::from(vec![
                    Span::raw("  "),
                    Span::styled(tree_row.prefix.clone(), Style::default().fg(branch_color)),
                    Span::styled(
                        entry.name.clone(),
                        Style::default().fg(Color::Rgb(250, 189, 47)),
                    ),
                ]);
            } else {
                line.spans.insert(1, Span::styled(
                    format!("{}{label}", tree_row.prefix),
                    Style::default().fg(branch_color),
                ));
            }
        }
        frame.render_widget(
            Paragraph::new(line).style(background),
            Rect::new(listing.x, listing.y + row as u16, listing.width, 1),
        );
    }
}

/// Lists keep priority. When narrow, previews stack below their own list;
/// if there is too little room, no preview is rendered at all.
pub fn preview_layout(area: Rect, enabled: bool) -> (Rect, Option<Rect>) {
    if !enabled || area.height == 0 || area.width == 0 {
        return (area, None);
    }
    if area.width >= 88 {
        let list_width = area.width * 55 / 100;
        let preview = Rect::new(
            area.x.saturating_add(list_width).saturating_add(1),
            area.y,
            area.width.saturating_sub(list_width + 1),
            area.height,
        );
        return (
            Rect::new(area.x, area.y, list_width, area.height),
            Some(preview),
        );
    }
    if area.width >= 24 && area.height >= 10 {
        let list_height = (area.height * 55 / 100).max(4);
        let preview = Rect::new(
            area.x,
            area.y.saturating_add(list_height).saturating_add(1),
            area.width,
            area.height.saturating_sub(list_height + 1),
        );
        return (
            Rect::new(area.x, area.y, area.width, list_height),
            Some(preview),
        );
    }
    (area, None)
}

pub fn draw_jump_strip(frame: &mut Frame, area: Rect, view: &PaneView<'_>) {
    if area.height == 0 || area.width == 0 { return; }
    let hints = view.jumps.hints(area.width, view.cwd);
    if hints.is_empty() { return; }
    let muted = Style::default().fg(Color::Rgb(146, 131, 116));
    let mut spans = vec![Span::styled(" ↗ ", Style::default().fg(Color::Rgb(131, 165, 152)))];
    for (index, hint) in hints.iter().enumerate() {
        if index > 0 { spans.push(Span::styled("  ", muted)); }
        let color = if hint.active { Color::Rgb(184, 187, 38) } else { Color::Rgb(250, 189, 47) };
        spans.push(Span::styled(format!("{}", hint.slot), Style::default().fg(color)));
        spans.push(Span::styled(format!(" {}", hint.label), muted));
    }
    let missing = view.jumps.len().saturating_sub(hints.len());
    if missing > 0 {
        spans.push(Span::styled(format!("  +{missing}"), muted));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(40, 40, 40))),
        area,
    );
}

pub fn draw_pane_content(
    frame: &mut Frame,
    area: Rect,
    view: &mut PaneView<'_>,
    metadata_position: MetadataPosition,
    active: bool,
    preview_enabled: bool,
) {
    let (listing, preview_area) = preview_layout(area, preview_enabled);
    let mut list = listing;
    if listing.height >= 2 && !view.jumps.hints(listing.width, view.cwd).is_empty() {
        draw_jump_strip(frame, Rect::new(listing.x, listing.y, listing.width, 1), view);
        list.y = list.y.saturating_add(1);
        list.height = list.height.saturating_sub(1);
    }
    draw_entries(frame, list, view, metadata_position, active);
    if let Some(preview_area) = preview_area {
        draw_preview(frame, preview_area, view.preview.as_deref_mut());
    }
}

pub fn draw_split_pane(
    frame: &mut Frame,
    area: Rect,
    view: &mut PaneView<'_>,
    metadata_position: MetadataPosition,
    active: bool,
    preview_enabled: bool,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    draw_pane_header(
        frame,
        Rect::new(area.x, area.y, area.width, 1),
        view,
        active,
    );
    draw_pane_content(
        frame,
        Rect::new(area.x, area.y.saturating_add(1), area.width, area.height - 1),
        view,
        metadata_position,
        active,
        preview_enabled,
    );
}

/// Two panes must be wide enough for both filenames; narrow windows show only
/// the active pane without destroying its companion's state.
pub fn use_two_columns(width: u16) -> bool {
    width >= 42
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responsive_preview_layout() {
        let wide = Rect::new(0, 0, 120, 30);
        let (list, preview) = preview_layout(wide, true);
        assert_eq!(list.height, 30);
        assert!(preview.unwrap().x >= list.width);
        let narrow = Rect::new(0, 0, 60, 30);
        let (list, preview) = preview_layout(narrow, true);
        assert!(list.height < narrow.height);
        assert!(preview.unwrap().y > list.height);
        let tiny = Rect::new(0, 0, 15, 8);
        assert!(preview_layout(tiny, true).1.is_none());
        assert!(preview_layout(wide, false).1.is_none());
    }

    #[test]
    fn responsive_split_threshold() {
        assert!(!use_two_columns(41));
        assert!(use_two_columns(42));
        assert!(use_two_columns(160));
    }
}
