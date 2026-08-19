//! Shared, platform-independent types used by Lumière.

mod caps;
mod id;
mod mode;
mod selector;
mod state;

pub use caps::Capabilities;
pub use id::{IdError, LightId};
pub use mode::{Hue, Kelvin, Mode, Percent, RangeError, SceneId};
pub use selector::Selector;
pub use state::{
    ClientMsg, CommandRequest, CommandResponse, ConnState, Event, LightSnapshot, PerLightResult,
    ResyncReason, SeqEvent, ServerMsg, SkipReason, WS_PROTOCOL_VERSION, WorldSnapshot,
};
