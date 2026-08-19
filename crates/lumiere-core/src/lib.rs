//! Pure protocol encoding and model capability resolution.

pub mod anim;
pub mod caps;
pub mod wire;

pub use anim::{Frame, playback_duration, schedule};
pub use caps::{ModelTable, ModelTableError};
pub use wire::{DecodeError, Decoded, Encoded, Packet, checksum, clamp_to_device, decode, encode};
