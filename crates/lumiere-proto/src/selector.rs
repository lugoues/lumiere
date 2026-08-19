use crate::LightId;
use serde::{Deserialize, Serialize};

/// Selects all lights or a specific set of light identifiers.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Selector {
    #[default]
    All,
    Ids {
        ids: Vec<LightId>,
    },
}
