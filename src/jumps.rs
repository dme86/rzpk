//! Persistent nine-slot directory jumps and compact per-pane hints.
//! Paths live in [jumps] in the user's regular config.toml.
use std::{collections::BTreeMap, env, io, path::{Path, PathBuf}};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const MAX_SLOT: u8 = 9;

#[derive(Clone, Debug, Default)]
pub struct Jumps {
    entries: BTreeMap<u8, PathBuf>,
}

impl Jumps {
    pub fn from_config(values: &BTreeMap<String, String>) -> io::Result<Self> {
        let mut jumps = Self::default();
        for (key, path) in values {
            let slot = parse_slot(key).ok_or_else(|| io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid jump slot '{key}': only 1-9 are allowed"),
            ))?;
            let expanded = expand_home(path)?;
            jumps.entries.insert(slot, expanded);
        }
        Ok(jumps)
    }

    pub fn get(&self, slot: u8) -> Option<&Path> {
        self.entries.get(&slot).map(PathBuf::as_path)
    }

    pub fn set(&mut self, slot: u8, path: PathBuf) -> io::Result<()> {
        if !(1..=MAX_SLOT).contains(&slot) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Jump slot must be 1–9"));
        }
        self.entries.insert(slot, path);
        Ok(())
    }

    pub fn remove(&mut self, slot: u8) -> bool {
        self.entries.remove(&slot).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (u8, &Path)> {
        self.entries.iter().map(|(slot, path)| (*slot, path.as_path()))
    }

    /// Produce a bounded, responsive jump bar without splitting multibyte text.
    /// Examples: "↗ 1 ~/.config  2 /etc" or "↗ 1 …/some-project  +3".
    pub fn hints(&self, width: u16, cwd: &Path) -> Vec<JumpHint> {
        let available = width.saturating_sub(1) as usize;
        if self.is_empty() || available < 5 {
            return Vec::new();
        }
        let all = self.entries.len();
        let mut used = 2usize; // "↗ "
        let mut hints = Vec::new();
        for (slot, path) in self.iter() {
            let remaining = all - hints.len();
            let reserved = if remaining > 1 { 4 } else { 0 }; // potential " +N"
            let room = available.saturating_sub(used + reserved);
            if room < 5 {
                break;
            }
            let label = compact_path(path, room.saturating_sub(3).min(22));
            let label_width = UnicodeWidthStr::width(label.as_str());
            let cost = 3 + label_width + if hints.is_empty() { 0 } else { 2 };
            if cost + used > available.saturating_sub(reserved) {
                break;
            }
            used += cost;
            hints.push(JumpHint {
                slot,
                label,
                active: path == cwd,
            });
        }
        hints
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JumpHint {
    pub slot: u8,
    pub label: String,
    pub active: bool,
}

pub fn parse_slot(value: &str) -> Option<u8> {
    let bytes = value.as_bytes();
    if bytes.len() == 1 && (b'1'..=b'9').contains(&bytes[0]) {
        Some(bytes[0] - b'0')
    } else {
        None
    }
}

fn expand_home(path: &str) -> io::Result<PathBuf> {
    if path == "~" || path.starts_with("~/") {
        let home = env::var_os("HOME")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is unset"))?;
        if path == "~" {
            return Ok(PathBuf::from(home));
        }
        return Ok(PathBuf::from(home).join(&path[2..]));
    }
    let absolute = PathBuf::from(path);
    if absolute.is_absolute() {
        Ok(absolute)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Jump target must be absolute or start with ~/: {path}"),
        ))
    }
}

fn compact_path(path: &Path, budget: usize) -> String {
    if budget == 0 { return String::new(); }
    let display = path.to_string_lossy();
    let home = env::var_os("HOME").map(PathBuf::from);
    let shown = if let Some(home) = home {
        if let Ok(relative) = path.strip_prefix(&home) {
            if relative.as_os_str().is_empty() {
                "~".to_owned()
            } else {
                format!("~/{}", relative.display())
            }
        } else {
            display.into_owned()
        }
    } else {
        display.into_owned()
    };
    if UnicodeWidthStr::width(shown.as_str()) <= budget {
        return shown;
    }
    if budget < 2 { return "…".to_owned(); }
    let final_component = path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or(shown);
    let available = budget - 2;
    let shortened = if UnicodeWidthStr::width(final_component.as_str()) <= available {
        final_component
    } else {
        // Preserve end of long project names: "…/…very-long-name".
        let chars: Vec<char> = final_component.chars().collect();
        let mut result = String::new();
        for ch in chars.into_iter().rev() {
            if UnicodeWidthStr::width(result.as_str()) + UnicodeWidthStr::width(ch.to_string().as_str()) > available.saturating_sub(1) {
                break;
            }
            result.insert(0, ch);
        }
        format!("…{result}")
    };
    let label = format!("…/{shortened}");
    if UnicodeWidthStr::width(label.as_str()) <= budget {
        label
    } else {
        let mut output = String::new();
        let mut remaining = budget;
        for character in label.chars() {
            let width = UnicodeWidthChar::width(character).unwrap_or(0);
            if width > remaining { break; }
            remaining -= width;
            output.push(character);
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_strictly_one_to_nine() {
        assert_eq!(parse_slot("1"), Some(1));
        assert_eq!(parse_slot("9"), Some(9));
        for bad in ["0", "10", "-1", "a", "01", ""] {
            assert_eq!(parse_slot(bad), None);
        }
    }

    #[test]
    fn paths_are_expanded_and_rejected_when_relative() {
        let mut values = BTreeMap::new();
        values.insert("1".to_owned(), "/etc".to_owned());
        values.insert("2".to_owned(), "~/projects".to_owned());
        let jumps = Jumps::from_config(&values).unwrap();
        assert_eq!(jumps.get(1), Some(Path::new("/etc")));
        assert!(jumps.get(2).unwrap().is_absolute());
        values.insert("3".to_owned(), "relative".to_owned());
        assert!(Jumps::from_config(&values).is_err());
    }

    #[test]
    fn cjk_project_names_never_overflow_cells() {
        let path = Path::new("/home/dev/非常に長いプロジェクトディレクトリ名");
        for budget in 1..=18 {
            let name = compact_path(path, budget);
            assert!(UnicodeWidthStr::width(name.as_str()) <= budget);
        }
    }

    #[test]
    fn long_paths_fit_even_in_narrow_panes() {
        let mut jumps = Jumps::default();
        jumps.set(1, PathBuf::from("/very/long/path/to/project-with-a-long-name")).unwrap();
        jumps.set(2, PathBuf::from("/etc")).unwrap();
        let wide = jumps.hints(120, Path::new("/etc"));
        assert_eq!(wide.len(), 2);
        assert!(wide[1].active);
        let tiny = jumps.hints(12, Path::new("/"));
        assert!(tiny.len() <= 1);
        assert!(tiny.iter().all(|hint| UnicodeWidthStr::width(hint.label.as_str()) <= 7));
    }
}
