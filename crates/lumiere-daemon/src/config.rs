use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
};

use directories::ProjectDirs;
use rand::{TryRngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Persistent daemon configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub bind: SocketAddr,
    pub token: String,
    #[serde(default)]
    pub cors_origins: Vec<String>,
}

impl Config {
    /// Loads or creates the Lumière configuration file.
    /// The file load_or_create reads and writes.
    pub fn path() -> Result<PathBuf, ConfigError> {
        Ok(config_dir()?.join("config.toml"))
    }

    pub fn load_or_create() -> Result<Self, ConfigError> {
        let path = Self::path()?;
        if path.exists() {
            return Ok(toml::from_str(&fs::read_to_string(path)?)?);
        }

        let mut token = [0_u8; 32];
        OsRng.try_fill_bytes(&mut token)?;
        let config = Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9091),
            token: base64url(&token),
            cors_origins: Vec::new(),
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let encoded = toml::to_string_pretty(&config)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        file.write_all(encoded.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        Ok(config)
    }

    /// Creates an in-memory configuration suitable for tests.
    pub fn for_tests(token: &str) -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            token: token.to_owned(),
            cors_origins: Vec::new(),
        }
    }
}

/// Failure to load or create daemon configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not determine the configuration directory")]
    NoConfigDirectory,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Decode(#[from] toml::de::Error),
    #[error(transparent)]
    Encode(#[from] toml::ser::Error),
    #[error("operating-system random generator failed: {0}")]
    Random(#[from] rand::rand_core::OsError),
}

fn config_dir() -> Result<PathBuf, ConfigError> {
    if let Some(path) = std::env::var_os("LUMIERE_CONFIG_DIR") {
        return Ok(path.into());
    }
    ProjectDirs::from("", "", "lumiere")
        .map(|dirs| dirs.config_dir().to_owned())
        .ok_or(ConfigError::NoConfigDirectory)
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = u32::from(chunk[0]) << 16
            | u32::from(*chunk.get(1).unwrap_or(&0)) << 8
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(ALPHABET[((value >> 18) & 0x3f) as usize] as char);
        output.push(ALPHABET[((value >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[((value >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[(value & 0x3f) as usize] as char);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::base64url;

    #[test]
    fn base64url_has_no_padding() {
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
    }
}
