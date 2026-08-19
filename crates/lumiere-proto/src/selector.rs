use crate::LightId;
use serde::{Deserialize, Serialize};

/// Selects all lights or a specific set of light identifiers.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Selector {
    All,
    Ids { ids: Vec<LightId> },
}
