use std::{collections::BTreeMap, env, fs, io, path::{Path, PathBuf}, str::FromStr, time::{SystemTime, UNIX_EPOCH}};

use serde::Deserialize;
use toml_edit::{DocumentMut, table, value};

/// Metadata follows filenames by default; the original long layout is optional.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MetadataPosition {
    Left,
    #[default]
    Right,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub metadata_position: MetadataPosition,
    pub jumps: BTreeMap<String, String>,
    pub connections: BTreeMap<String, ConnectionConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionConfig {
    /// SSH host alias from ~/.ssh/config, optionally user@host.
    pub host: String,
    /// Absolute directory to show initially (default: /).
    #[serde(default = "default_remote_path")]
    pub path: PathBuf,
}
fn default_remote_path() -> PathBuf { PathBuf::from("/") }

impl Config {
    pub fn load() -> io::Result<Self> {
        let Some(path) = config_path() else {
            return Ok(Self::default());
        };
        match fs::read_to_string(&path) {
            Ok(source) => toml::from_str(&source).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid config {}: {error}", path.display()),
                )
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(io::Error::new(
                error.kind(),
                format!("cannot read config {}: {error}", path.display()),
            )),
        }
    }
    /// Persist a single jump while preserving all other TOML values and comments.
    pub fn update_jump(slot: u8, directory: Option<&Path>) -> io::Result<()> {
        if !(1..=9).contains(&slot) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Jump slot must be 1–9"));
        }
        let path = config_path().ok_or_else(|| io::Error::new(
            io::ErrorKind::NotFound, "HOME or XDG_CONFIG_HOME is required for persistent jumps",
        ))?;
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        let updated = edited_jump_config(&source, slot, directory)?;
        let parent = path.parent().ok_or_else(|| io::Error::other("Invalid config path"))?;
        fs::create_dir_all(parent)?;
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let temp = parent.join(format!(".config.toml.{}.{}.tmp", std::process::id(), nanos));
        let result = (|| {
            use std::io::Write;
            let mut file = fs::OpenOptions::new().create_new(true).write(true).open(&temp)?;
            if let Ok(meta) = fs::metadata(&path) {
                fs::set_permissions(&temp, meta.permissions())?;
            }
            file.write_all(updated.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temp, &path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

fn edited_jump_config(source: &str, slot: u8, directory: Option<&Path>) -> io::Result<String> {
    let mut document = DocumentMut::from_str(source)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if !document.as_table().contains_key("jumps") {
        document["jumps"] = table();
    }
    if !document["jumps"].is_table() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "[jumps] must be a TOML table"));
    }
    let key = slot.to_string();
    if let Some(directory) = directory {
        let text = directory.to_str().ok_or_else(|| io::Error::new(
            io::ErrorKind::InvalidInput, "Non-UTF-8 paths cannot be stored in TOML",
        ))?;
        document["jumps"][&key] = value(text);
    } else {
        document["jumps"].as_table_mut().unwrap().remove(&key);
    }
    Ok(document.to_string())
}


/// Respect XDG_CONFIG_HOME on Linux/macOS, otherwise use ~/.config.
fn config_path() -> Option<PathBuf> {
    let root = env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".config")))?;
    Some(root.join("rzpk").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_preserves_comments_metadata_and_other_jump_slots() {
        let original = "# keep this comment\nmetadata_position = 'left'\n\n[jumps]\n'2' = '/etc'\n";
        let saved = edited_jump_config(original, 1, Some(Path::new("/home/user/.config"))).unwrap();
        assert!(saved.contains("# keep this comment"));
        let config: Config = toml::from_str(&saved).unwrap();
        assert_eq!(config.metadata_position, MetadataPosition::Left);
        assert_eq!(config.jumps["1"], "/home/user/.config");
        assert_eq!(config.jumps["2"], "/etc");
        let deleted = edited_jump_config(&saved, 1, None).unwrap();
        let config: Config = toml::from_str(&deleted).unwrap();
        assert!(!config.jumps.contains_key("1"));
        assert_eq!(config.jumps["2"], "/etc");
    }

    #[test]
    fn rejects_invalid_jumps_table() {
        assert!(edited_jump_config("jumps = 3", 2, Some(Path::new("/etc"))).is_err());
    }

    #[test]
    fn sftp_profiles_parse_and_do_not_store_credentials() {
        let source = r#"
metadata_position = "right"
[connections.nas]
host = "my-nas"
path = "/volume1/projects"
[connections.prod]
host = "deploy@prod.example.org"
"#;
        let config: Config = toml::from_str(source).unwrap();
        assert_eq!(config.connections["nas"].host, "my-nas");
        assert_eq!(config.connections["nas"].path, Path::new("/volume1/projects"));
        assert_eq!(config.connections["prod"].path, Path::new("/"));
        assert!(toml::from_str::<Config>(
            "[connections.bad]\nhost = 'server'\npassword = 'unsafe'"
        ).is_err(), "Password fields must not be accepted");
    }

    #[test]
    fn no_configuration_uses_right_layout() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.metadata_position, MetadataPosition::Right);
    }

    #[test]
    fn can_switch_back_to_left() {
        let config: Config = toml::from_str("metadata_position = 'left'").unwrap();
        assert_eq!(config.metadata_position, MetadataPosition::Left);
    }

    #[test]
    fn explicit_right_and_bad_values() {
        let config: Config = toml::from_str("metadata_position = 'right'").unwrap();
        assert_eq!(config.metadata_position, MetadataPosition::Right);
        assert!(toml::from_str::<Config>("metadata_position = 'middle'").is_err());
    }
}
