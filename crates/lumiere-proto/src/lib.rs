//! Shared, platform-independent types used by Lumière.

mod caps;
mod id;
mod mode;
mod selector;

pub use caps::Capabilities;
pub use id::{IdError, LightId};
pub use mode::{Hue, Kelvin, Mode, Percent, RangeError, SceneId};
pub use selector::Selector;
