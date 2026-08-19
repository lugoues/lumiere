use crate::Kelvin;
use serde::{Deserialize, Serialize};

/// Features and limits exposed by a light model.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Capabilities {
    pub cct_min: Kelvin,
    pub cct_max: Kelvin,
    pub rgb: bool,
    pub scenes: bool,
    pub cct_split_packets: bool,
    pub reports_status: bool,
}
