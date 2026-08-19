//! Shared, platform-independent types used by Lumière.

mod animation;
mod caps;
mod id;
mod mode;
mod preset;
mod selector;
mod state;

pub use animation::{
    AnimTarget, Animation, AnimationId, AnimationSummary, Keyframe, PlaybackOptions,
    PlaybackStatus, TargetBinding,
};
pub use caps::Capabilities;
pub use id::{IdError, LightId};
pub use mode::{Hue, Kelvin, Mode, Percent, RangeError, SceneId};
pub use preset::{Preset, PresetEntry, PresetId, PresetTarget};
pub use selector::Selector;
pub use state::{
    ClientMsg, CommandRequest, CommandResponse, ConnState, Event, LightSnapshot, PerLightResult,
    ResyncReason, SeqEvent, ServerMsg, SkipReason, WS_PROTOCOL_VERSION, WorldSnapshot,
};
