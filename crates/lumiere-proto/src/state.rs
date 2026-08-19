use serde::{Deserialize, Serialize};

use crate::{Capabilities, LightId, Mode};

/// WebSocket wire protocol version supported by this crate.
pub const WS_PROTOCOL_VERSION: u32 = 1;

/// The current connection lifecycle state of a light.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnState {
    Discovered,
    Connecting { attempt: u32 },
    Connected,
    Reconnecting { attempt: u32 },
    Lost,
}

/// The complete client-visible state of one light.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightSnapshot {
    pub id: LightId,
    pub model: String,
    pub label: String,
    pub caps: Capabilities,
    pub conn: ConnState,
    pub rssi: Option<i16>,
    pub desired: Option<Mode>,
    pub confirmed: Option<Mode>,
    pub power: Option<bool>,
    pub last_error: Option<String>,
}

/// A consistent view of all known lights.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldSnapshot {
    pub seq: u64,
    pub lights: Vec<LightSnapshot>,
}

/// A coarse world-state change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Event {
    Light { light: LightSnapshot },
    LightRemoved { id: LightId },
}

/// An event with its monotonically increasing world sequence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeqEvent {
    pub seq: u64,
    pub event: Event,
}

/// The result of applying one command to one light.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PerLightResult {
    Applied {
        id: LightId,
        mode: Mode,
    },
    Coalesced {
        id: LightId,
    },
    Adapted {
        id: LightId,
        requested: Mode,
        applied: Mode,
    },
    Skipped {
        id: LightId,
        reason: SkipReason,
    },
    Failed {
        id: LightId,
        error: String,
    },
}

/// Why a light command was intentionally not written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    NotConnected,
    UnsupportedMode,
}

/// A message sent from a WebSocket client to the daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        protocol_version: u32,
        ticket: String,
        last_seq: Option<u64>,
    },
    Ping {
        nonce: u64,
    },
}

/// A message sent from the daemon to a WebSocket client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome {
        protocol_version: u32,
        server_version: String,
        session: String,
        seq: u64,
        snapshot: Option<WorldSnapshot>,
    },
    Patch {
        events: Vec<SeqEvent>,
    },
    Resync {
        snapshot: WorldSnapshot,
        reason: ResyncReason,
    },
    Pong {
        nonce: u64,
    },
    Error {
        message: String,
    },
}

/// Why a connected WebSocket client needs a fresh snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResyncReason {
    ClientLagged,
    SessionChanged,
    SeqOutOfWindow,
}

/// Request body for applying a mode to selected lights.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRequest {
    pub selector: crate::Selector,
    pub mode: Mode,
    #[serde(default)]
    pub wait: bool,
}

/// Results from applying a command to selected lights.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandResponse {
    pub results: Vec<PerLightResult>,
}
