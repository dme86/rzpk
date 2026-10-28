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
    pub selection: Selection,
    pub scroll: usize,
    pub typed: String,
    pub last_typed: Option<Instant>,
    pub error: Option<String>,
    pub preview: Option<Preview>,
    pub tree: Option<TreeState>,
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
            selection: Selection::default(),
            scroll: 0,
            typed: String::new(),
            last_typed: None,
            error: None,
            preview: None,
            tree,
        })
    }

    pub fn view(&mut self) -> PaneView<'_> {
        PaneView {
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

    pub fn refresh_preview(&mut self) {
        self.preview = if self.tree.as_ref().is_none_or(|tree| tree.is_real(self.selected)) {
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
    pub entries: &'a [Entry],
    pub max_name_width: usize,
    pub selected: usize,
    pub selection: &'a Selection,
    pub scroll: &'a mut usize,
    pub typed: &'a str,
    pub error: Option<&'a str>,
    pub preview: Option<&'a Preview>,
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
            view.cwd.display().to_string(),
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

    if view.entries.is_empty() {
        frame.render_widget(
            Paragraph::new("  (empty directory)").style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let height = usize::from(area.height);
    if view.selected < *view.scroll {
        *view.scroll = view.selected;
    } else if view.selected >= view.scroll.saturating_add(height) {
        *view.scroll = view.selected + 1 - height;
    }

    let columns = Columns::for_width(area.width);
    let now = SystemTime::now();
    for (row, entry) in view
        .entries
        .iter()
        .skip(*view.scroll)
        .take(height)
        .enumerate()
    {
        let index = *view.scroll + row;
        let focused = active && index == view.selected;
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
            area.width,
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
            Rect::new(area.x, area.y + row as u16, area.width, 1),
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

pub fn draw_pane_content(
    frame: &mut Frame,
    area: Rect,
    view: &mut PaneView<'_>,
    metadata_position: MetadataPosition,
    active: bool,
    preview_enabled: bool,
) {
    let (listing, preview_area) = preview_layout(area, preview_enabled);
    draw_entries(frame, listing, view, metadata_position, active);
    if let Some(preview_area) = preview_area {
        draw_preview(frame, preview_area, view.preview);
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
