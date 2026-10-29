//! Small Gruvbox keyboard-only batch rename dialog.
//! Preview is pure; validation and path checks occur before confirmation.
use std::path::PathBuf;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use crate::batch_rename::{BatchRename, proposed_name};

const FG: Color = Color::Rgb(235, 219, 178);
const MUTED: Color = Color::Rgb(146, 131, 116);
const GOLD: Color = Color::Rgb(250, 189, 47);
const GREEN: Color = Color::Rgb(184, 187, 38);
const BG: Color = Color::Rgb(40, 40, 40);

pub enum Result {
    Keep,
    Cancel,
    Submit(BatchRename),
}

pub struct RenameDialog {
    sources: Vec<PathBuf>,
    prefix: String,
    start: String,
    digits: String,
    field: usize,
    pending: Option<BatchRename>,
    error: Option<String>,
}

impl RenameDialog {
    pub fn new(mut sources: Vec<PathBuf>) -> Self {
        sources.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        let digits = sources.len().to_string().len().clamp(2, 8).to_string();
        Self { sources, prefix: String::new(), start: "1".into(), digits, field: 0, pending: None, error: None }
    }

    pub fn count(&self) -> usize { self.sources.len() }

    fn selected_field_mut(&mut self) -> &mut String {
        match self.field {
            0 => &mut self.prefix,
            1 => &mut self.start,
            _ => &mut self.digits,
        }
    }

    pub fn handle(&mut self, key: KeyEvent) -> Result {
        match key.code {
            KeyCode::Esc => return Result::Cancel,
            KeyCode::Enter => {
                if let Some(plan) = self.pending.take() {
                    return Result::Submit(plan);
                }
                let start = self.start.parse::<u32>();
                let digits = self.digits.parse::<usize>();
                match (start, digits) {
                    (Ok(start), Ok(digits)) => match BatchRename::build(
                        self.sources.clone(), &self.prefix, start, digits,
                    ) {
                        Ok(plan) => {
                            self.pending = Some(plan);
                            self.error = None;
                        }
                        Err(error) => self.error = Some(error.to_string()),
                    },
                    _ => self.error = Some("Start and digits must be positive integers".into()),
                }
            }
            KeyCode::Tab if self.pending.is_none() => self.field = (self.field + 1) % 3,
            KeyCode::BackTab if self.pending.is_none() => self.field = (self.field + 2) % 3,
            KeyCode::Up if self.pending.is_none() => self.field = (self.field + 2) % 3,
            KeyCode::Down if self.pending.is_none() => self.field = (self.field + 1) % 3,
            KeyCode::Backspace if self.pending.is_none() => { self.selected_field_mut().pop(); self.error = None; }
            KeyCode::Char(ch) if self.pending.is_none()
                && !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::META) =>
            {
                let limit = if self.field == 0 { 100 } else if self.field == 1 { 10 } else { 2 };
                if self.selected_field_mut().chars().count() < limit
                    && (self.field == 0 || ch.is_ascii_digit())
                {
                    self.selected_field_mut().push(ch);
                    self.error = None;
                }
            }
            _ => {}
        }
        Result::Keep
    }

    fn examples(&self) -> Vec<(String, String)> {
        let (Ok(start), Ok(digits)) = (self.start.parse::<u32>(), self.digits.parse::<usize>())
        else { return Vec::new(); };
        if self.prefix.is_empty() || !(1..=8).contains(&digits) { return Vec::new(); }
        self.sources.iter().take(4).enumerate().filter_map(|(offset, source)| {
            let number = start.checked_add(offset as u32)?;
            Some((
                source.file_name()?.to_string_lossy().into_owned(),
                proposed_name(source, &self.prefix, number, digits).to_string_lossy().into_owned(),
            ))
        }).collect()
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        if area.width < 30 || area.height < 10 { return; }
        let width = area.width.saturating_sub(2).min(78);
        let height = area.height.saturating_sub(2).min(17);
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width, height,
        );
        let block = Block::default()
            .title(format!(" ↻ Batch rename · {} files ", self.count()))
            .title_style(Style::default().fg(GOLD).add_modifier(Modifier::BOLD))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(MUTED))
            .style(Style::default().bg(BG));
        let mut lines = Vec::new();
        lines.push(Line::raw(""));
        for (index, (label, value)) in [
            ("Base name", self.prefix.as_str()),
            ("Start", self.start.as_str()),
            ("Digits", self.digits.as_str()),
        ].into_iter().enumerate() {
            let focused = self.pending.is_none() && self.field == index;
            lines.push(Line::from(vec![
                Span::styled(if focused { "  ▸ " } else { "    " }, Style::default().fg(GREEN)),
                Span::styled(format!("{label:<12}"), Style::default().fg(if focused { GOLD } else { MUTED })),
                Span::styled(if value.is_empty() { "(type here)".to_owned() } else { value.to_owned() },
                    Style::default().fg(if focused { FG } else { GREEN })),
                Span::styled(if focused { "█" } else { "" }, Style::default().fg(GOLD)),
            ]));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled("  Preview (original → new)", Style::default().fg(GOLD)));
        for (source, target) in self.examples() {
            lines.push(Line::from(vec![
                Span::styled(format!("  {source}"), Style::default().fg(MUTED)),
                Span::styled("  →  ", Style::default().fg(GREEN)),
                Span::styled(target, Style::default().fg(GREEN)),
            ]));
        }
        if self.sources.len() > 4 {
            lines.push(Line::styled(format!("  … and {} more", self.sources.len() - 4), Style::default().fg(MUTED)));
        }
        lines.push(Line::raw(""));
        if let Some(error) = &self.error {
            lines.push(Line::styled(format!("  ✕ {error}"), Style::default().fg(Color::Rgb(251, 73, 52))));
        } else if self.pending.is_some() {
            lines.push(Line::styled("  ✓ Names checked · Enter: rename now · Esc: cancel",
                Style::default().fg(GREEN)));
        } else {
            lines.push(Line::styled("  Tab/↑↓: field · Enter: review · Esc: cancel",
                Style::default().fg(MUTED)));
        }
        frame.render_widget(Clear, rect);
        frame.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    fn key(ch: char) -> KeyEvent { KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE) }
    #[test]
    fn preview_and_two_phase_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("a.jpg");
        let second = dir.path().join("b.jpg");
        std::fs::write(&first, b"a").unwrap();
        std::fs::write(&second, b"b").unwrap();
        let mut modal = RenameDialog::new(vec![second, first]);
        for ch in "Urlaub_in_China".chars() { assert!(matches!(modal.handle(key(ch)), Result::Keep)); }
        assert_eq!(modal.examples()[0].1, "Urlaub_in_China_01.jpg");
        assert_eq!(modal.examples()[1].1, "Urlaub_in_China_02.jpg");
        assert!(matches!(modal.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Result::Keep));
        assert!(modal.pending.is_some());
        assert!(matches!(modal.handle(key('x')), Result::Keep));
        assert!(matches!(modal.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Result::Submit(_)));
    }

    #[test]
    fn rejects_existing_target_and_escape_aborts() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("old.png");
        std::fs::write(&a, b"old").unwrap();
        std::fs::write(dir.path().join("new_01.png"), b"do not overwrite").unwrap();
        let mut modal = RenameDialog::new(vec![a]);
        for ch in "new".chars() { modal.handle(key(ch)); }
        modal.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(modal.pending.is_none());
        assert!(modal.error.as_deref().unwrap().contains("exists"));
        assert!(matches!(modal.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), Result::Cancel));
    }
}
