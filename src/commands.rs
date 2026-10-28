//! Single source of truth for terminal shortcuts, command names and help text.
//! New bindings/commands must be registered here: the ? overlay is generated
//! from these tables, not from a separate hand-maintained help document.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Help, Quit, Command, SwitchPane, Parent, EnterDirectory, Open, Up, Down,
    MarkUp, MarkDown, Mark, Clear, Copy, Move, Archive, Extract, Jobs, Jump(u8),
}

#[derive(Clone, Copy)]
enum Input {
    Any(KeyCode),
    Plain(KeyCode),
    Shift(KeyCode),
    ShiftLetter(char),
    Literal(char),
    CtrlLetter(char),
    JumpDigit,
}

impl Input {
    fn matches(self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match self {
            Self::Any(code) => key.code == code,
            Self::Plain(code) => key.code == code && !ctrl && !alt && !shift,
            Self::Shift(code) => key.code == code && shift && !ctrl && !alt,
            Self::ShiftLetter(ch) => match key.code {
                KeyCode::Char(value) if !ctrl && !alt => {
                    value.to_ascii_lowercase() == ch
                        && (shift || value.is_ascii_uppercase())
                }
                _ => false,
            },
            Self::Literal(ch) => key.code == KeyCode::Char(ch) && !ctrl && !alt,
            Self::CtrlLetter(ch) => key.code == KeyCode::Char(ch) && ctrl && !alt,
            Self::JumpDigit => matches!(key.code, KeyCode::Char('1'..='9'))
                && !ctrl && !shift
                && (alt || key.modifiers.intersects(KeyModifiers::SUPER | KeyModifiers::META)),
        }
    }
}

pub struct Binding {
    pub label: &'static str,
    pub description: &'static str,
    pub action: Action,
    input: Input,
    pub section: &'static str,
}

// Multiple representations of Shift+C/J etc. are handled by Input::ShiftLetter
// and aren't duplicated in this table.
pub const BINDINGS: &[Binding] = &[
    Binding { label: "?", description: "Show / close this help", action: Action::Help, input: Input::Literal('?'), section: "GENERAL" },
    Binding { label: ":", description: "Enter a command", action: Action::Command, input: Input::Literal(':'), section: "GENERAL" },
    Binding { label: "Ctrl+C", description: "Exit rzpk immediately", action: Action::Quit, input: Input::CtrlLetter('c'), section: "GENERAL" },
    Binding { label: "Esc", description: "Clear marks / dismiss a dialog", action: Action::Clear, input: Input::Any(KeyCode::Esc), section: "GENERAL" },
    Binding { label: "Tab", description: "Switch split panes", action: Action::SwitchPane, input: Input::Plain(KeyCode::Tab), section: "NAVIGATION" },
    Binding { label: "Shift+Tab", description: "Switch split panes", action: Action::SwitchPane, input: Input::Any(KeyCode::BackTab), section: "NAVIGATION" },
    Binding { label: "Up", description: "Previous entry", action: Action::Up, input: Input::Plain(KeyCode::Up), section: "NAVIGATION" },
    Binding { label: "Down", description: "Next entry", action: Action::Down, input: Input::Plain(KeyCode::Down), section: "NAVIGATION" },
    Binding { label: "Enter", description: "Open file in $EDITOR / folder / toggle tree", action: Action::Open, input: Input::Any(KeyCode::Enter), section: "NAVIGATION" },
    Binding { label: "Backspace", description: "Go to parent folder", action: Action::Parent, input: Input::Any(KeyCode::Backspace), section: "NAVIGATION" },
    Binding { label: "←", description: "Go to parent folder", action: Action::Parent, input: Input::Plain(KeyCode::Left), section: "NAVIGATION" },
    Binding { label: "→", description: "Enter focused folder", action: Action::EnterDirectory, input: Input::Plain(KeyCode::Right), section: "NAVIGATION" },
    Binding { label: "Type", description: "Search filename (prefix / substring)", action: Action::Command, input: Input::Plain(KeyCode::Null), section: "NAVIGATION" },
    Binding { label: "Alt/Cmd+1..9", description: "Jump to saved directory in active pane", action: Action::Jump(0), input: Input::JumpDigit, section: "NAVIGATION" },
    Binding { label: "Shift+Up", description: "Extend selection upward", action: Action::MarkUp, input: Input::Shift(KeyCode::Up), section: "SELECTION" },
    Binding { label: "Shift+Down", description: "Extend selection downward", action: Action::MarkDown, input: Input::Shift(KeyCode::Down), section: "SELECTION" },
    Binding { label: "Shift+J", description: "Extend selection downward", action: Action::MarkDown, input: Input::ShiftLetter('j'), section: "SELECTION" },
    Binding { label: "Shift+K", description: "Extend selection upward", action: Action::MarkUp, input: Input::ShiftLetter('k'), section: "SELECTION" },
    Binding { label: "Space", description: "Toggle focused item mark", action: Action::Mark, input: Input::Literal(' '), section: "SELECTION" },
    Binding { label: "Shift+C", description: "Copy to other pane", action: Action::Copy, input: Input::ShiftLetter('c'), section: "FILES" },
    Binding { label: "Shift+M", description: "Move to other pane", action: Action::Move, input: Input::ShiftLetter('m'), section: "FILES" },
    Binding { label: "Shift+A", description: "Create a .tar.gz archive", action: Action::Archive, input: Input::ShiftLetter('a'), section: "FILES" },
    Binding { label: "Shift+E", description: "Extract the selected archive", action: Action::Extract, input: Input::ShiftLetter('e'), section: "FILES" },
    Binding { label: "Shift+Q", description: "Open the job queue (Del removes waiting jobs)", action: Action::Jobs, input: Input::ShiftLetter('q'), section: "FILES" },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandAction { Quit, Vertical, Preview, Tree, Normal, Copy, Move, Archive, Extract, Jobs, Keys, Jump(u8), JumpSet(u8), JumpDelete(u8) }

pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
    pub action: CommandAction,
}

pub const COMMANDS: &[Command] = &[
    Command { name: "q", description: "Exit rzpk", action: CommandAction::Quit },
    Command { name: "v", description: "Toggle vertical split", action: CommandAction::Vertical },
    Command { name: "p", description: "Toggle file preview", action: CommandAction::Preview },
    Command { name: "t", description: "Toggle tree view", action: CommandAction::Tree },
    Command { name: "n", description: "Return to normal view", action: CommandAction::Normal },
    Command { name: "cp", description: "Copy to other pane", action: CommandAction::Copy },
    Command { name: "mv", description: "Move to other pane", action: CommandAction::Move },
    Command { name: "archive", description: "Create a .tar.gz archive", action: CommandAction::Archive },
    Command { name: "extract", description: "Extract selected archive", action: CommandAction::Extract },
    Command { name: "jobs", description: "Show and manage the file-operation queue", action: CommandAction::Jobs },
    Command { name: "keys", description: "Inspect received keyboard events (Esc closes)", action: CommandAction::Keys },
    Command { name: "j1", description: "Jump to slot 1–9 (e.g. :j3)", action: CommandAction::Jump(1) },
    Command { name: "jset 1", description: "Save active directory to slot 1–9", action: CommandAction::JumpSet(1) },
    Command { name: "jdel 1", description: "Remove a saved jump (1–9)", action: CommandAction::JumpDelete(1) },
];

pub fn action_for(key: KeyEvent) -> Option<Action> {
    let action = BINDINGS.iter().find(|binding| binding.input.matches(key))?.action;
    if action == Action::Jump(0)
        && let KeyCode::Char(ch) = key.code
    {
        return ch.to_digit(10).map(|slot| Action::Jump(slot as u8));
    }
    Some(action)
}

pub fn command_for(text: &str) -> Option<CommandAction> {
    let text = text.trim();
    if let Some(slot) = text.strip_prefix("jset ").and_then(crate::jumps::parse_slot) {
        return Some(CommandAction::JumpSet(slot));
    }
    if let Some(slot) = text.strip_prefix("jdel ").and_then(crate::jumps::parse_slot) {
        return Some(CommandAction::JumpDelete(slot));
    }
    if let Some(slot) = text.strip_prefix('j').and_then(crate::jumps::parse_slot) {
        return Some(CommandAction::Jump(slot));
    }
    COMMANDS.iter().find(|command| command.name == text).map(|command| command.action)
}

pub fn help_lines() -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::styled("  rzpk · Keyboard reference", Style::default().fg(Color::Rgb(250, 189, 47)).add_modifier(Modifier::BOLD)));
    lines.push(Line::raw(""));
    let mut last_section = "";
    for binding in BINDINGS {
        if last_section != binding.section {
            if !last_section.is_empty() { lines.push(Line::raw("")); }
            lines.push(Line::styled(format!("  {}", binding.section), Style::default().fg(Color::Rgb(142, 192, 124)).add_modifier(Modifier::BOLD)));
            last_section = binding.section;
        }
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<17}", binding.label), Style::default().fg(Color::Rgb(235, 219, 178))),
            Span::styled(binding.description, Style::default().fg(Color::Rgb(189, 174, 147))),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("  COMMANDS   (type : then Enter)", Style::default().fg(Color::Rgb(142, 192, 124)).add_modifier(Modifier::BOLD)));
    for command in COMMANDS {
        lines.push(Line::from(vec![
            Span::styled(format!("  :{:<16}", command.name), Style::default().fg(Color::Rgb(235, 219, 178))),
            Span::styled(command.description, Style::default().fg(Color::Rgb(189, 174, 147))),
        ]));
    }
    lines
}

pub fn help_max_scroll(height: u16) -> usize {
    help_lines().len().saturating_sub(height.saturating_sub(4) as usize)
}

pub fn draw_help(frame: &mut Frame, area: Rect, scroll: usize) {
    if area.width < 12 || area.height < 6 {
        return;
    }
    let lines = help_lines();
    let width = area.width.saturating_sub(2).min(72);
    let height = area.height.saturating_sub(2).min((lines.len() + 2) as u16);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width, height,
    );
    let block = Block::default()
        .title(" ? Help · Esc to close · ↑↓ to scroll ")
        .title_style(Style::default().fg(Color::Rgb(250, 189, 47)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(146, 131, 116)))
        .style(Style::default().bg(Color::Rgb(40, 40, 40)));
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).scroll((scroll.min(u16::MAX as usize) as u16, 0)).block(block),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn listed_bindings_are_live() {
        for binding in BINDINGS {
            // Null is a documentation-only entry for the free-typing fallback.
            if binding.input.matches(KeyEvent::new(KeyCode::Null, KeyModifiers::NONE)) {
                continue;
            }
            assert!(!binding.label.is_empty());
            assert!(help_lines().iter().any(|line| {
                line.spans.iter().any(|span| span.content.contains(binding.label))
            }), "missing help for {}", binding.label);
        }
    }

    #[test]
    fn shifted_letter_keeps_plain_search_free() {
        assert_eq!(action_for(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE)), None);
        assert_eq!(action_for(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE)), Some(Action::Copy));
        assert_eq!(action_for(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::SHIFT)), Some(Action::Move));
        assert_eq!(action_for(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT)), Some(Action::Help));
    }

    #[test]
    fn alt_or_super_digit_activates_jump_without_disrupting_plain_numbers() {
        assert_eq!(action_for(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT)), Some(Action::Jump(1)));
        assert_eq!(action_for(KeyEvent::new(KeyCode::Char('9'), KeyModifiers::SUPER)), Some(Action::Jump(9)));
        assert_eq!(action_for(KeyEvent::new(KeyCode::Char('6'), KeyModifiers::META)), Some(Action::Jump(6)));
        assert_eq!(action_for(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE)), None);
        assert_eq!(action_for(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::ALT)), None);
        assert_eq!(action_for(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::CONTROL)), None);
    }

    #[test]
    fn jump_commands_accept_only_one_through_nine() {
        assert_eq!(command_for("jset 7"), Some(CommandAction::JumpSet(7)));
        assert_eq!(command_for("jdel 2"), Some(CommandAction::JumpDelete(2)));
        assert_eq!(command_for("j9"), Some(CommandAction::Jump(9)));
        for text in ["j0", "j10", "jset 0", "jdel 20", "jset", "j 1"] {
            assert_eq!(command_for(text), None);
        }
    }

    #[test]
    fn all_registered_commands_are_in_help() {
        for command in COMMANDS {
            assert_eq!(command_for(command.name), Some(command.action));
            assert!(help_lines().iter().any(|line| line.spans.iter().any(|span| span.content.contains(&format!(":{}", command.name)))));
        }
    }
}
