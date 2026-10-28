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
    config::MetadataPosition,
    details::Columns,
    render::{EntryHighlight, entry_line, maximum_name_width},
    selection::Selection,
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
}

impl PaneState {
    /// The new pane opens the same folder, with independent cursor and marks.
    pub fn open(cwd: &Path) -> io::Result<Self> {
        let entries = read_directory(cwd)?;
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
        }
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
        let line = entry_line(
            entry,
            EntryHighlight { focused, marked },
            columns,
            metadata_position,
            view.max_name_width,
            area.width,
            now,
        );
        frame.render_widget(
            Paragraph::new(line).style(background),
            Rect::new(area.x, area.y + row as u16, area.width, 1),
        );
    }
}

pub fn draw_split_pane(
    frame: &mut Frame,
    area: Rect,
    view: &mut PaneView<'_>,
    metadata_position: MetadataPosition,
    active: bool,
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
    draw_entries(
        frame,
        Rect::new(area.x, area.y.saturating_add(1), area.width, area.height - 1),
        view,
        metadata_position,
        active,
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
    fn responsive_split_threshold() {
        assert!(!use_two_columns(41));
        assert!(use_two_columns(42));
        assert!(use_two_columns(160));
    }
}
