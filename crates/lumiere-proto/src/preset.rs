use core::fmt;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::{LightId, Mode};

/// A validated, URL-safe preset identifier.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PresetId(SmolStr);

impl PresetId {
    /// Parses a nonempty lowercase alphanumeric-and-dash slug.
    pub fn parse(value: &str) -> Result<Self, String> {
        if !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            Ok(Self(value.into()))
        } else {
            Err(format!("invalid preset id: {value}"))
        }
    }

    /// Returns the slug text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for PresetId {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<PresetId> for String {
    fn from(value: PresetId) -> Self {
        value.0.into()
    }
}

impl fmt::Display for PresetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A named, ordered collection of light modes.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Preset {
    pub id: PresetId,
    pub name: String,
    pub entries: Vec<PresetEntry>,
}

/// One target and mode captured in a preset.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PresetEntry {
    pub target: PresetTarget,
    pub mode: Mode,
}

/// A preset target applying globally or to one stable light identifier.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PresetTarget {
    Everything,
    Light { id: LightId },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_ids_follow_animation_slug_rules() {
        assert_eq!(PresetId::parse("warm-2").unwrap().as_str(), "warm-2");
        assert!(PresetId::parse("").is_err());
        assert!(PresetId::parse("Warm").is_err());
        assert!(PresetId::parse("warm light").is_err());
    }
}
