use std::{env, fs, io, path::{Path, PathBuf}};

use serde::Deserialize;

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
}

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
