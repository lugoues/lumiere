use std::sync::LazyLock;

use lumiere_proto::{Capabilities, Kelvin};
use serde::Deserialize;
use thiserror::Error;

#[derive(Deserialize)]
struct RawTable {
    defaults: RawCaps,
    #[serde(default, rename = "model")]
    models: Vec<RawModel>,
}

#[derive(Clone, Deserialize)]
struct RawCaps {
    cct_min: u16,
    cct_max: u16,
    rgb: bool,
    scenes: bool,
    cct_split_packets: bool,
    reports_status: bool,
}

#[derive(Deserialize)]
struct RawModel {
    patterns: Vec<String>,
    cct_min: Option<u16>,
    cct_max: Option<u16>,
    rgb: Option<bool>,
    scenes: Option<bool>,
    cct_split_packets: Option<bool>,
    reports_status: Option<bool>,
}

struct Model {
    patterns: Vec<String>,
    caps: Capabilities,
}

/// A parsed model-name capability database.
pub struct ModelTable {
    defaults: Capabilities,
    models: Vec<Model>,
}

/// An invalid model capability database.
#[derive(Debug, Error)]
pub enum ModelTableError {
    #[error("invalid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid {field} value {value} in {entry}: {source}")]
    Kelvin {
        entry: String,
        field: &'static str,
        value: u16,
        source: lumiere_proto::RangeError,
    },
    #[error("minimum CCT exceeds maximum CCT in {0}")]
    ReversedRange(String),
    #[error("model entry has no patterns")]
    EmptyPatterns,
    #[error("duplicate normalized model pattern {pattern:?}")]
    DuplicatePattern { pattern: String },
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|c| !matches!(c, ' ' | '-'))
        .flat_map(char::to_uppercase)
        .collect()
}

fn capabilities(raw: &RawCaps, entry: &str) -> Result<Capabilities, ModelTableError> {
    let make_kelvin = |field, value| {
        Kelvin::new(value).map_err(|source| ModelTableError::Kelvin {
            entry: entry.to_owned(),
            field,
            value,
            source,
        })
    };
    let cct_min = make_kelvin("cct_min", raw.cct_min)?;
    let cct_max = make_kelvin("cct_max", raw.cct_max)?;
    if cct_min > cct_max {
        return Err(ModelTableError::ReversedRange(entry.to_owned()));
    }
    Ok(Capabilities {
        cct_min,
        cct_max,
        rgb: raw.rgb,
        scenes: raw.scenes,
        cct_split_packets: raw.cct_split_packets,
        reports_status: raw.reports_status,
    })
}

impl ModelTable {
    /// Parses and validates a TOML capability table.
    pub fn parse(toml_src: &str) -> Result<Self, ModelTableError> {
        let raw: RawTable = toml::from_str(toml_src)?;
        let defaults = capabilities(&raw.defaults, "defaults")?;
        let mut seen: Vec<String> = Vec::new();
        let mut models = Vec::with_capacity(raw.models.len());
        for model in raw.models {
            if model.patterns.is_empty() {
                return Err(ModelTableError::EmptyPatterns);
            }
            let patterns: Vec<_> = model.patterns.iter().map(|p| normalize(p)).collect();
            for pattern in &patterns {
                if seen.iter().any(|prior| prior == pattern) {
                    return Err(ModelTableError::DuplicatePattern {
                        pattern: pattern.clone(),
                    });
                }
                seen.push(pattern.clone());
            }
            let merged = RawCaps {
                cct_min: model.cct_min.unwrap_or(defaults.cct_min.get()),
                cct_max: model.cct_max.unwrap_or(defaults.cct_max.get()),
                rgb: model.rgb.unwrap_or(defaults.rgb),
                scenes: model.scenes.unwrap_or(defaults.scenes),
                cct_split_packets: model
                    .cct_split_packets
                    .unwrap_or(defaults.cct_split_packets),
                reports_status: model.reports_status.unwrap_or(defaults.reports_status),
            };
            models.push(Model {
                caps: capabilities(&merged, &model.patterns.join("/"))?,
                patterns,
            });
        }
        Ok(Self { defaults, models })
    }

    /// Returns the capability table embedded in this crate.
    pub fn builtin() -> &'static Self {
        static TABLE: LazyLock<ModelTable> = LazyLock::new(|| {
            ModelTable::parse(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/models.toml"
            )))
            .expect("built-in model table must be valid")
        });
        &TABLE
    }

    /// Resolves an advertised device name using longest normalized substring match.
    pub fn resolve(&self, advertised_name: &str) -> Capabilities {
        let name = normalize(advertised_name);
        self.models
            .iter()
            .flat_map(|model| {
                model
                    .patterns
                    .iter()
                    .map(move |pattern| (pattern, &model.caps))
            })
            .filter(|(pattern, _)| name.contains(pattern.as_str()))
            .max_by_key(|(pattern, _)| pattern.len())
            .map_or_else(|| self.defaults.clone(), |(_, caps)| caps.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::collections::BTreeMap;

    #[derive(Deserialize)]
    struct Expected {
        cct_min: u16,
        cct_max: u16,
        cct_only: bool,
    }

    #[test]
    fn matches_python_fixture_with_documented_corrections() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/fixtures/model_parity.json"
        ));
        let expected: BTreeMap<String, Expected> = serde_json::from_str(fixture).unwrap();
        for (name, python) in expected {
            let actual = ModelTable::builtin().resolve(&name);
            // Python removed spaces only from names, leaving these patterns dead. Our normalization fixes them.
            let intended = ["RGBC80", "RGBCB60", "RGB176A1"]
                .iter()
                .any(|suffix| normalize(&name).ends_with(suffix));
            if intended {
                assert_eq!(
                    (actual.cct_min.get(), actual.cct_max.get(), actual.rgb),
                    (2500, 10000, true),
                    "{name}"
                );
            } else {
                assert_eq!(
                    (actual.cct_min.get(), actual.cct_max.get(), actual.rgb),
                    (python.cct_min, python.cct_max, !python.cct_only),
                    "{name}"
                );
            }
        }
        let apollo = ModelTable::builtin().resolve("Apollo");
        assert_eq!(
            (apollo.cct_min.get(), apollo.cct_max.get(), apollo.rgb),
            (5600, 5600, false)
        );
    }
}
