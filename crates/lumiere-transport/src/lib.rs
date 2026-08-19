//! Transport abstractions and a deterministic in-memory light simulator.

use std::time::Duration;

use futures::stream::BoxStream;
use lumiere_proto::LightId;

pub mod sim;

#[cfg(feature = "btleplug")]
pub mod ble;

/// An error produced while discovering, connecting to, or writing to a light.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// No usable Bluetooth adapter is available.
    #[error("Bluetooth adapter is unavailable")]
    AdapterUnavailable,
    /// The requested light is unknown to the transport.
    #[error("light {id} was not found")]
    NotFound { id: LightId },
    /// A connection attempt failed.
    #[error("failed to connect to light {id}")]
    ConnectFailed { id: LightId },
    /// An operation did not complete within its deadline.
    #[error("operation on light {id} timed out after {timeout:?}")]
    TimedOut { id: LightId, timeout: Duration },
    /// A packet could not be written or accepted by the light.
    #[error("write to light {id} failed: {message}")]
    WriteFailed { id: LightId, message: String },
    /// The link to a light is no longer connected.
    #[error("light {id} is disconnected")]
    Disconnected { id: LightId },
    /// A platform Bluetooth operation failed.
    #[error("Bluetooth operation failed{context}: {message}", context = id.as_ref().map(|id| format!(" for light {id}")).unwrap_or_default())]
    Backend {
        /// Light involved in the operation, when known.
        id: Option<LightId>,
        /// Human-readable platform error or missing-profile detail.
        message: String,
    },
}

/// Whether the transport has an adapter ready for use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterState {
    /// The adapter is present and ready.
    Ready,
    /// The adapter is absent or unavailable.
    Unavailable,
}

/// Constraints applied to a discovery scan.
#[derive(Debug, Clone, Default)]
pub struct ScanFilter {
    /// Only report devices whose advertised name starts with this prefix.
    pub name_prefix: Option<String>,
}

/// A light reported by a discovery scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// Stable transport-specific light identifier.
    pub id: LightId,
    /// Advertised device name, when available.
    pub name: Option<String>,
    /// Received signal strength in dBm, when available.
    pub rssi: Option<i16>,
}

/// A running discovery scan.
///
/// Dropping this value, including its event stream, stops the scan.
pub struct Scan {
    /// Devices discovered by this scan.
    pub events: BoxStream<'static, Discovered>,
}

/// A source of discoverable, connectable lights.
#[async_trait::async_trait]
pub trait Transport: Send + Sync + 'static {
    /// Starts a discovery scan.
    async fn scan(&self, filter: ScanFilter) -> Result<Scan, TransportError>;

    /// Connects to a light and returns once the link is usable for writes.
    async fn connect(
        &self,
        id: &LightId,
        timeout: Duration,
    ) -> Result<Box<dyn Link>, TransportError>;

    /// Returns the current adapter state.
    async fn adapter_state(&self) -> AdapterState;
}

/// The acknowledgement behavior requested for a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteKind {
    /// Request an acknowledgement from the peer.
    WithResponse,
    /// Complete without waiting for a peer acknowledgement.
    WithoutResponse,
}

/// A usable connection to one light.
#[async_trait::async_trait]
pub trait Link: Send + Sync {
    /// Returns the connected light's identifier.
    fn id(&self) -> &LightId;

    /// Returns whether this link is still connected.
    fn is_connected(&self) -> bool;

    /// Writes one complete wire packet.
    async fn write(&self, payload: &[u8], kind: WriteKind) -> Result<(), TransportError>;

    /// Subscribes to device notifications.
    async fn notifications(&self) -> Result<BoxStream<'static, Vec<u8>>, TransportError>;

    /// Resolves when the peer goes away.
    async fn closed(&self);

    /// Disconnects this link.
    async fn disconnect(&self) -> Result<(), TransportError>;
}
