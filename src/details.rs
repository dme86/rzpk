//! Small, terminal-friendly metadata formatters. No filesystem I/O during rendering.
use std::{
    fs::Metadata,
    time::{Duration, SystemTime},
};

use chrono::{DateTime, Local};
use ratatui::style::Color;

pub const PERMISSIONS_WIDTH: usize = 10;
pub const SIZE_WIDTH: usize = 9;
pub const DATE_WIDTH: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Columns {
    Full,
    WithoutSize,
    Permissions,
    Names,
}

impl Columns {
    pub fn for_width(width: u16) -> Self {
        match width {
            64.. => Self::Full,
            50..=63 => Self::WithoutSize,
            32..=49 => Self::Permissions,
            _ => Self::Names,
        }
    }

    pub fn show_permissions(self) -> bool {
        !matches!(self, Self::Names)
    }

    pub fn show_size(self) -> bool {
        matches!(self, Self::Full)
    }

    pub fn show_date(self) -> bool {
        matches!(self, Self::Full | Self::WithoutSize)
    }
}

pub fn permissions(metadata: Option<&Metadata>) -> String {
    let Some(metadata) = metadata else {
        return "??????????".to_string();
    };
    let file_type = metadata.file_type();
    let kind = if file_type.is_symlink() {
        'l'
    } else if file_type.is_dir() {
        'd'
    } else {
        '-'
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        unix_permissions(kind, metadata.permissions().mode())
    }
    #[cfg(not(unix))]
    {
        let write = if metadata.permissions().readonly() { '-' } else { 'w' };
        format!("{kind}r{write}-------")
    }
}

#[cfg(unix)]
fn unix_permissions(kind: char, mode: u32) -> String {
    let mut result = String::with_capacity(PERMISSIONS_WIDTH);
    result.push(kind);
    for (index, shift) in [6, 3, 0].into_iter().enumerate() {
        result.push(if mode & (0o4 << shift) != 0 { 'r' } else { '-' });
        result.push(if mode & (0o2 << shift) != 0 { 'w' } else { '-' });

        let execute = mode & (0o1 << shift) != 0;
        let special = match index {
            0 => mode & 0o4000 != 0,
            1 => mode & 0o2000 != 0,
            _ => mode & 0o1000 != 0,
        };
        result.push(match (index, special, execute) {
            (2, true, true) => 't',
            (2, true, false) => 'T',
            (_, true, true) => 's',
            (_, true, false) => 'S',
            (_, false, true) => 'x',
            _ => '-',
        });
    }
    result
}

pub fn permission_color(ch: char) -> Color {
    match ch {
        'r' => Color::Rgb(116, 187, 151),
        'w' => Color::Rgb(218, 180, 113),
        'x' | 's' | 'S' | 't' | 'T' => Color::Rgb(210, 133, 135),
        'd' | 'l' => Color::Rgb(127, 169, 213),
        '-' | '?' => Color::Rgb(93, 106, 119),
        _ => Color::Gray,
    }
}

pub fn human_size(size: Option<u64>, is_dir: bool) -> String {
    if is_dir {
        return "-".to_string();
    }
    let Some(bytes) = size else {
        return "?".to_string();
    };
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut amount = bytes as f64;
    let mut index = 0;
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    while amount >= 1024.0 && index < UNITS.len() - 1 {
        amount /= 1024.0;
        index += 1;
    }
    if amount < 10.0 {
        format!("{amount:.1} {}", UNITS[index])
    } else {
        format!("{amount:.0} {}", UNITS[index])
    }
}

pub fn modified_date(modified: Option<SystemTime>) -> String {
    match modified {
        Some(time) => {
            let local: DateTime<Local> = time.into();
            local.format("%Y-%m-%d %H:%M").to_string()
        }
        None => "-".to_string(),
    }
}

/// Date hues get progressively less saturated as the file gets older.
pub fn modified_color(modified: Option<SystemTime>, now: SystemTime) -> Color {
    let Some(modified) = modified else {
        return Color::Rgb(93, 106, 119);
    };
    let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
    if age < Duration::from_secs(60 * 60) {
        Color::Rgb(58, 212, 121) // last hour: vivid green
    } else if age < Duration::from_secs(24 * 60 * 60) {
        Color::Rgb(95, 194, 118) // today: rich green
    } else if age < Duration::from_secs(7 * 24 * 60 * 60) {
        Color::Rgb(130, 178, 113)
    } else if age < Duration::from_secs(30 * 24 * 60 * 60) {
        Color::Rgb(153, 159, 116)
    } else if age < Duration::from_secs(180 * 24 * 60 * 60) {
        Color::Rgb(127, 140, 140)
    } else {
        Color::Rgb(97, 109, 121) // old: muted slate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn permissions_include_type_and_special_bits() {
        assert_eq!(unix_permissions('d', 0o755), "drwxr-xr-x");
        assert_eq!(unix_permissions('-', 0o644), "-rw-r--r--");
        assert_eq!(unix_permissions('-', 0o4755), "-rwsr-xr-x");
        assert_eq!(unix_permissions('d', 0o1777), "drwxrwxrwt");
        assert_eq!(unix_permissions('-', 0o6600), "-rwS--S---");
    }

    #[test]
    fn human_sizes_are_readable() {
        assert_eq!(human_size(Some(0), false), "0 B");
        assert_eq!(human_size(Some(1024), false), "1.0 KiB");
        assert_eq!(human_size(Some(1024 * 1024), false), "1.0 MiB");
        assert_eq!(human_size(Some(2048), true), "-");
    }

    #[test]
    fn dates_are_more_vivid_when_recent() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(365 * 24 * 60 * 60);
        let hour = now - Duration::from_secs(1800);
        let month = now - Duration::from_secs(40 * 24 * 60 * 60);
        assert_eq!(modified_color(Some(hour), now), Color::Rgb(58, 212, 121));
        assert_eq!(modified_color(Some(month), now), Color::Rgb(127, 140, 140));
    }

    #[test]
    fn narrow_terminals_drop_metadata_columns() {
        assert_eq!(Columns::for_width(120), Columns::Full);
        assert_eq!(Columns::for_width(60), Columns::WithoutSize);
        assert_eq!(Columns::for_width(42), Columns::Permissions);
        assert_eq!(Columns::for_width(25), Columns::Names);
    }

    #[test]
    fn missing_metadata_does_not_crash() {
        assert_eq!(permissions(None), "??????????");
        assert_eq!(human_size(None, false), "?");
        assert_eq!(modified_date(None), "-");
    }
}
