//! Gruvbox modal for one new file or one new folder with optional subfolders.
//! Keyboard-only, small and responsive. Enter reviews; Enter again confirms.
use std::{path::PathBuf, sync::Arc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use crate::{create::{CreatePlan, Kind}, remote::Remote};

const BG: Color = Color::Rgb(40, 40, 40);
const FG: Color = Color::Rgb(235, 219, 178);
const GOLD: Color = Color::Rgb(250, 189, 47);
const GREEN: Color = Color::Rgb(184, 187, 38);
const MUTED: Color = Color::Rgb(146, 131, 116);
const RED: Color = Color::Rgb(251, 73, 52);

pub enum Result {
    Keep,
    Cancel,
    Submit(CreatePlan),
}

pub struct CreateDialog {
    base: PathBuf,
    remote: Option<Arc<Remote>>,
    kind: Kind,
    name: String,
    children: String,
    field: usize,
    ready: Option<CreatePlan>,
    error: Option<String>,
}

impl CreateDialog {
    pub fn new(base: PathBuf, remote: Option<Arc<Remote>>, kind: Kind) -> Self {
        Self {
            base, remote, kind, name: String::new(), children: String::new(),
            field: 1, ready: None, error: None,
        }
    }

    pub fn handle(&mut self, key: KeyEvent) -> Result {
        if key.code == KeyCode::Esc { return Result::Cancel; }
        if self.ready.is_some() {
            return match key.code {
                KeyCode::Enter => Result::Submit(self.ready.take().unwrap()),
                KeyCode::Backspace | KeyCode::Tab => {
                    self.ready = None;
                    Result::Keep
                }
                _ => Result::Keep,
            };
        }
        match key.code {
            KeyCode::Enter => {
                match CreatePlan::build(
                    self.base.clone(), &self.name, self.kind, &self.children,
                    self.remote.clone(),
                ) {
                    Ok(plan) => { self.ready = Some(plan); self.error = None; }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
            KeyCode::Tab => self.next_field(),
            KeyCode::BackTab => self.prev_field(),
            KeyCode::Up => self.prev_field(),
            KeyCode::Down => self.next_field(),
            KeyCode::Left | KeyCode::Right if self.field == 0 => self.toggle_kind(),
            KeyCode::Backspace if self.field == 1 || self.field == 2 => {
                if self.field == 1 { self.name.pop(); } else { self.children.pop(); }
                self.error = None;
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::META
                ) =>
            {
                if self.field == 0 {
                    if ch.eq_ignore_ascii_case(&'f') { self.set_kind(Kind::File); }
                    if ch.eq_ignore_ascii_case(&'d') { self.set_kind(Kind::Folder); }
                } else if self.field == 1 && self.name.chars().count() < 120 {
                    self.name.push(ch);
                    self.error = None;
                } else if self.field == 2 && self.kind == Kind::Folder
                    && self.children.chars().count() < 500
                {
                    self.children.push(ch);
                    self.error = None;
                }
            }
            _ => {}
        }
        Result::Keep
    }

    fn set_kind(&mut self, kind: Kind) {
        self.kind = kind;
        if kind == Kind::File { self.children.clear(); }
        self.error = None;
    }

    fn toggle_kind(&mut self) {
        let next = if self.kind == Kind::Folder { Kind::File } else { Kind::Folder };
        self.set_kind(next);
    }

    fn next_field(&mut self) {
        self.field = match (self.field, self.kind) {
            (0, _) => 1,
            (1, Kind::Folder) => 2,
            _ => 0,
        };
    }
    fn prev_field(&mut self) {
        self.field = match (self.field, self.kind) {
            (0, Kind::Folder) => 2,
            (0, Kind::File) => 1,
            (2, _) => 1,
            _ => 0,
        };
    }

    fn preview(&self) -> Vec<String> {
        if self.name.trim().is_empty() { return Vec::new(); }
        let kind = match self.kind { Kind::File => "•", Kind::Folder => "▸" };
        let mut tree = vec![format!("  {kind} {}", self.name)];
        if self.kind == Kind::Folder {
            // Display a few paths immediately while typing. Full validation
            // happens on Enter, before a second confirmation is allowed.
            let entries = self.children.split(',').map(str::trim)
                .filter(|entry| !entry.is_empty()).collect::<Vec<_>>();
            for entry in entries.iter().take(5) {
                tree.push(format!("    └─ {entry}/"));
            }
            if entries.len() > 5 {
                tree.push(format!("    … {} more", entries.len() - 5));
            }
        }
        tree
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        if area.width < 32 || area.height < 10 { return; }
        let width = area.width.saturating_sub(2).min(76);
        let height = area.height.saturating_sub(2).min(17);
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width, height,
        );
        let block = Block::default()
            .title(" ✚ New · file / folder ")
            .title_style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(MUTED))
            .style(Style::default().bg(BG));

        let mut lines = vec![Line::raw("")];
        lines.push(Line::from(vec![
            Span::styled(if self.field == 0 && self.ready.is_none() { "  ▸ " } else { "    " },
                Style::default().fg(GREEN)),
            Span::styled("Type       ", Style::default().fg(if self.field == 0 { GOLD } else { MUTED })),
            Span::styled(if self.kind == Kind::Folder { "[ Folder ]   File" } else { "Folder   [ File ]" },
                Style::default().fg(GREEN)),
        ]));
        for (index, label, value) in [(1, "Name", self.name.as_str()), (2, "Subfolders", self.children.as_str())] {
            if index == 2 && self.kind == Kind::File { continue; }
            let focused = self.field == index && self.ready.is_none();
            lines.push(Line::from(vec![
                Span::styled(if focused { "  ▸ " } else { "    " }, Style::default().fg(GREEN)),
                Span::styled(format!("{label:<11}"), Style::default().fg(if focused { GOLD } else { MUTED })),
                Span::styled(if value.is_empty() { "(optional)" .to_owned() } else { value.to_owned() },
                    Style::default().fg(if focused { FG } else { GREEN })),
                Span::styled(if focused { "█" } else { "" }, Style::default().fg(GOLD)),
            ]));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled("  Preview", Style::default().fg(GOLD)));
        for entry in self.preview() {
            lines.push(Line::styled(entry, Style::default().fg(GREEN)));
        }
        lines.push(Line::raw(""));
        if let Some(error) = &self.error {
            lines.push(Line::styled(format!("  ✕ {error}"), Style::default().fg(RED)));
        } else if self.ready.is_some() {
            lines.push(Line::styled("  ✓ Paths checked · Enter: create · Backspace: edit · Esc: cancel",
                Style::default().fg(GREEN)));
        } else {
            lines.push(Line::styled(
                "  Tab/↑↓: field · ←→: type · Enter: review · Esc: cancel",
                Style::default().fg(MUTED)));
            if self.kind == Kind::Folder {
                lines.push(Line::styled(
                    "  Subfolders: Fotos, Videos, Dokumente/Originale",
                    Style::default().fg(MUTED)));
            }
        }
        // Truncate to the modal's usable rows, never bleed into other panes.
        lines.truncate(rect.height.saturating_sub(2) as usize);
        frame.render_widget(Clear, rect);
        frame.render_widget(Paragraph::new(lines).block(block), rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent { KeyEvent::new(code, KeyModifiers::NONE) }
    #[test]
    fn creates_folder_with_preview_and_two_step_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let mut modal = CreateDialog::new(dir.path().to_path_buf(), None, Kind::Folder);
        for ch in "Urlaub China".chars() { modal.handle(key(KeyCode::Char(ch))); }
        modal.handle(key(KeyCode::Tab));
        for ch in "Fotos/RAW, Videos".chars() { modal.handle(key(KeyCode::Char(ch))); }
        assert!(modal.preview().iter().any(|line| line.contains("Fotos/RAW")));
        assert!(matches!(modal.handle(key(KeyCode::Enter)), Result::Keep));
        assert!(modal.ready.is_some());
        assert!(matches!(modal.handle(key(KeyCode::Enter)), Result::Submit(_)));
    }

    #[test]
    fn file_mode_prevents_children_and_esc_cancels() {
        let dir = tempfile::tempdir().unwrap();
        let mut modal = CreateDialog::new(dir.path().to_path_buf(), None, Kind::File);
        for ch in "hello.txt".chars() { modal.handle(key(KeyCode::Char(ch))); }
        modal.handle(key(KeyCode::Tab));
        assert_eq!(modal.field, 0);
        modal.handle(key(KeyCode::Char('d')));
        assert_eq!(modal.kind, Kind::Folder);
        modal.handle(key(KeyCode::Left));
        assert_eq!(modal.kind, Kind::File);
        assert!(matches!(modal.handle(key(KeyCode::Esc)), Result::Cancel));
    }

    #[test]
    fn invalid_paths_cannot_be_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let mut modal = CreateDialog::new(dir.path().to_path_buf(), None, Kind::Folder);
        for ch in "../oops".chars() { modal.handle(key(KeyCode::Char(ch))); }
        modal.handle(key(KeyCode::Enter));
        assert!(modal.ready.is_none());
        assert!(modal.error.is_some());
    }
}
