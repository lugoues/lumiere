//! Deterministic in-memory implementation of the transport traits.

use std::{
    collections::HashMap,
    num::NonZeroU32,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use futures::{StreamExt, stream};
use lumiere_core::wire::{Decoded, decode};
use lumiere_proto::LightId;
use tokio::sync::{Mutex as AsyncMutex, Notify, broadcast};
use tokio::time::Instant;

use crate::{
    AdapterState, Discovered, Link, Scan, ScanFilter, Transport, TransportError, WriteKind,
};

/// Configuration for one simulated light.
#[derive(Debug, Clone)]
pub struct SimLightSpec {
    /// Stable identifier exposed by discovery and connection APIs.
    pub id: LightId,
    /// Device name emitted during discovery.
    pub advertised_name: String,
    /// Signal strength emitted during discovery.
    pub rssi: i16,
    /// Number of connection attempts that fail before one succeeds.
    pub connect_failures: u32,
}

/// Configuration for a simulated transport.
#[derive(Debug, Clone)]
pub struct SimConfig {
    /// Lights known to the transport.
    pub lights: Vec<SimLightSpec>,
    /// Delay applied to every write attempt.
    pub write_latency: Duration,
    /// Inject a failure into every Nth write across all lights.
    pub fail_every_nth_write: Option<NonZeroU32>,
}

/// A clonable deterministic transport for tests and local development.
#[derive(Clone)]
pub struct SimTransport {
    inner: Arc<Inner>,
}

struct Inner {
    lights: Vec<Arc<SimLight>>,
    by_id: HashMap<LightId, Arc<SimLight>>,
    write_latency: Duration,
    fail_every_nth_write: Option<NonZeroU32>,
    write_count: AtomicU64,
}

struct SimLight {
    spec: SimLightSpec,
    state: Mutex<LightState>,
    write_lock: AsyncMutex<()>,
}

struct LightState {
    connect_failures: u32,
    active: Option<Arc<Connection>>,
    timeline: Vec<(Instant, Decoded)>,
}

struct Connection {
    connected: AtomicBool,
    closed: Notify,
    notifications: broadcast::Sender<Vec<u8>>,
}

impl Connection {
    fn new() -> Self {
        let (notifications, _) = broadcast::channel(32);
        Self {
            connected: AtomicBool::new(true),
            closed: Notify::new(),
            notifications,
        }
    }

    fn close(&self) {
        if self.connected.swap(false, Ordering::AcqRel) {
            self.closed.notify_waiters();
        }
    }

    async fn wait_closed(&self) {
        loop {
            if !self.connected.load(Ordering::Acquire) {
                return;
            }
            let notified = self.closed.notified();
            if !self.connected.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

impl SimTransport {
    /// Creates a transport with the supplied deterministic behavior.
    pub fn new(config: SimConfig) -> Self {
        let lights: Vec<_> = config
            .lights
            .into_iter()
            .map(|spec| {
                let connect_failures = spec.connect_failures;
                Arc::new(SimLight {
                    spec,
                    state: Mutex::new(LightState {
                        connect_failures,
                        active: None,
                        timeline: Vec::new(),
                    }),
                    write_lock: AsyncMutex::new(()),
                })
            })
            .collect();
        let by_id = lights
            .iter()
            .map(|light| (light.spec.id.clone(), Arc::clone(light)))
            .collect();
        Self {
            inner: Arc::new(Inner {
                lights,
                by_id,
                write_latency: config.write_latency,
                fail_every_nth_write: config.fail_every_nth_write,
                write_count: AtomicU64::new(0),
            }),
        }
    }

    /// Returns an inspection handle for a simulated light.
    ///
    /// Panics when the identifier is unknown.
    pub fn light(&self, id: &LightId) -> SimLightHandle {
        SimLightHandle {
            light: Arc::clone(
                self.inner
                    .by_id
                    .get(id)
                    .unwrap_or_else(|| panic!("unknown simulated light {id}")),
            ),
        }
    }
}

#[async_trait::async_trait]
impl Transport for SimTransport {
    async fn scan(&self, filter: ScanFilter) -> Result<Scan, TransportError> {
        let discovered: Vec<_> = self
            .inner
            .lights
            .iter()
            .filter(|light| {
                filter
                    .name_prefix
                    .as_ref()
                    .is_none_or(|prefix| light.spec.advertised_name.starts_with(prefix))
            })
            .map(|light| Discovered {
                id: light.spec.id.clone(),
                name: Some(light.spec.advertised_name.clone()),
                rssi: Some(light.spec.rssi),
            })
            .collect();
        let events = stream::iter(discovered).chain(stream::pending()).boxed();
        Ok(Scan { events })
    }

    async fn connect(
        &self,
        id: &LightId,
        _timeout: Duration,
    ) -> Result<Box<dyn Link>, TransportError> {
        let light = self
            .inner
            .by_id
            .get(id)
            .ok_or_else(|| TransportError::NotFound { id: id.clone() })?;
        let connection = Arc::new(Connection::new());
        let replaced = {
            let mut state = light.state.lock().expect("simulator light mutex poisoned");
            if state.connect_failures > 0 {
                state.connect_failures -= 1;
                return Err(TransportError::ConnectFailed { id: id.clone() });
            }
            state.active.replace(Arc::clone(&connection))
        };
        if let Some(old) = replaced {
            old.close();
        }
        Ok(Box::new(SimLink {
            id: id.clone(),
            light: Arc::clone(light),
            inner: Arc::clone(&self.inner),
            connection,
        }))
    }

    async fn adapter_state(&self) -> AdapterState {
        AdapterState::Ready
    }
}

/// Read-only and fault-control access to one simulated light.
#[derive(Clone)]
pub struct SimLightHandle {
    light: Arc<SimLight>,
}

impl SimLightHandle {
    /// Returns accepted packets in arrival order with their acceptance times.
    pub fn timeline(&self) -> Vec<(Instant, Decoded)> {
        self.light
            .state
            .lock()
            .expect("simulator light mutex poisoned")
            .timeline
            .clone()
    }

    /// Returns the most recently accepted packet, if any.
    pub fn last(&self) -> Option<(Instant, Decoded)> {
        self.light
            .state
            .lock()
            .expect("simulator light mutex poisoned")
            .timeline
            .last()
            .copied()
    }

    /// Returns whether the light currently has a live link.
    pub fn is_connected(&self) -> bool {
        self.light
            .state
            .lock()
            .expect("simulator light mutex poisoned")
            .active
            .as_ref()
            .is_some_and(|connection| connection.connected.load(Ordering::Acquire))
    }

    /// Forcefully closes the light's active link.
    pub fn force_disconnect(&self) {
        let active = self
            .light
            .state
            .lock()
            .expect("simulator light mutex poisoned")
            .active
            .take();
        if let Some(connection) = active {
            connection.close();
        }
    }

    /// Sends a notification to subscribers of the active link.
    pub fn push_notification(&self, payload: Vec<u8>) {
        let active = self
            .light
            .state
            .lock()
            .expect("simulator light mutex poisoned")
            .active
            .clone();
        if let Some(connection) = active
            && connection.connected.load(Ordering::Acquire)
        {
            let _ = connection.notifications.send(payload);
        }
    }
}

struct SimLink {
    id: LightId,
    light: Arc<SimLight>,
    inner: Arc<Inner>,
    connection: Arc<Connection>,
}

impl SimLink {
    fn disconnected(&self) -> TransportError {
        TransportError::Disconnected {
            id: self.id.clone(),
        }
    }
}

#[async_trait::async_trait]
impl Link for SimLink {
    fn id(&self) -> &LightId {
        &self.id
    }

    fn is_connected(&self) -> bool {
        self.connection.connected.load(Ordering::Acquire)
    }

    async fn write(&self, payload: &[u8], _kind: WriteKind) -> Result<(), TransportError> {
        if !self.is_connected() {
            return Err(self.disconnected());
        }
        let _write_guard = self.light.write_lock.lock().await;
        if !self.is_connected() {
            return Err(self.disconnected());
        }

        let attempt = self.inner.write_count.fetch_add(1, Ordering::AcqRel) + 1;
        tokio::time::sleep(self.inner.write_latency).await;
        if !self.is_connected() {
            return Err(self.disconnected());
        }
        if self
            .inner
            .fail_every_nth_write
            .is_some_and(|n| attempt.is_multiple_of(u64::from(n.get())))
        {
            return Err(TransportError::WriteFailed {
                id: self.id.clone(),
                message: "injected simulator failure".into(),
            });
        }

        let decoded = decode(payload).map_err(|error| TransportError::WriteFailed {
            id: self.id.clone(),
            message: error.to_string(),
        })?;
        let mut state = self
            .light
            .state
            .lock()
            .expect("simulator light mutex poisoned");
        if !self.connection.connected.load(Ordering::Acquire)
            || !state
                .active
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, &self.connection))
        {
            return Err(self.disconnected());
        }
        state.timeline.push((Instant::now(), decoded));
        Ok(())
    }

    async fn notifications(
        &self,
    ) -> Result<futures::stream::BoxStream<'static, Vec<u8>>, TransportError> {
        if !self.is_connected() {
            return Err(self.disconnected());
        }
        let receiver = self.connection.notifications.subscribe();
        Ok(stream::unfold(receiver, |mut receiver| async move {
            loop {
                match receiver.recv().await {
                    Ok(payload) => return Some((payload, receiver)),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        })
        .boxed())
    }

    async fn closed(&self) {
        self.connection.wait_closed().await;
    }

    async fn disconnect(&self) -> Result<(), TransportError> {
        {
            let mut state = self
                .light
                .state
                .lock()
                .expect("simulator light mutex poisoned");
            if state
                .active
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, &self.connection))
            {
                state.active = None;
            }
        }
        self.connection.close();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use futures::StreamExt;
    use lumiere_core::wire::{Decoded, encode};
    use lumiere_proto::{Capabilities, Kelvin, Mode, Percent};
    use tokio::time::{Duration, Instant, advance, timeout};

    use super::*;

    fn light(name: &str, advertised_name: &str, connect_failures: u32) -> SimLightSpec {
        SimLightSpec {
            id: LightId::sim(name),
            advertised_name: advertised_name.into(),
            rssi: -42,
            connect_failures,
        }
    }

    fn transport(
        lights: Vec<SimLightSpec>,
        write_latency: Duration,
        fail_every_nth_write: Option<NonZeroU32>,
    ) -> SimTransport {
        SimTransport::new(SimConfig {
            lights,
            write_latency,
            fail_every_nth_write,
        })
    }

    fn cct_packet(bri: u8) -> Vec<u8> {
        let caps = Capabilities {
            cct_min: Kelvin::new(3200).unwrap(),
            cct_max: Kelvin::new(5600).unwrap(),
            rgb: true,
            scenes: true,
            cct_split_packets: false,
            reports_status: false,
        };
        encode(
            Mode::Cct {
                temp: Kelvin::new(4200).unwrap(),
                bri: Percent::new(bri).unwrap(),
            },
            &caps,
        )
        .packets()[0]
            .as_bytes()
            .to_vec()
    }

    #[tokio::test]
    async fn scan_filters_and_reports_each_light_once() {
        let sim = transport(
            vec![
                light("1", "NEEWER-RGB660 PRO", 0),
                light("2", "OTHER", 0),
                light("3", "NEEWER-CB60", 0),
            ],
            Duration::ZERO,
            None,
        );
        let mut scan = sim
            .scan(ScanFilter {
                name_prefix: Some("NEEWER-".into()),
            })
            .await
            .unwrap();

        assert_eq!(scan.events.next().await.unwrap().id, LightId::sim("1"));
        assert_eq!(scan.events.next().await.unwrap().id, LightId::sim("3"));
        assert!(
            timeout(Duration::from_millis(1), scan.events.next())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn connection_failures_are_consumed_before_success() {
        let id = LightId::sim("1");
        let sim = transport(vec![light("1", "NEEWER", 2)], Duration::ZERO, None);

        for _ in 0..2 {
            assert!(matches!(
                sim.connect(&id, Duration::from_secs(1)).await,
                Err(TransportError::ConnectFailed { .. })
            ));
            assert!(!sim.light(&id).is_connected());
        }
        let link = sim.connect(&id, Duration::from_secs(1)).await.unwrap();
        assert!(link.is_connected());
        assert!(sim.light(&id).is_connected());
    }

    #[tokio::test(start_paused = true)]
    async fn write_latency_and_decoding_are_recorded() {
        let id = LightId::sim("1");
        let latency = Duration::from_millis(25);
        let sim = transport(vec![light("1", "NEEWER", 0)], latency, None);
        let link = sim.connect(&id, Duration::from_secs(1)).await.unwrap();
        let started = Instant::now();
        let packet = cct_packet(75);

        let write =
            tokio::spawn(async move { link.write(&packet, WriteKind::WithoutResponse).await });
        tokio::task::yield_now().await;
        assert!(sim.light(&id).timeline().is_empty());
        advance(latency).await;
        write.await.unwrap().unwrap();

        assert_eq!(
            sim.light(&id).timeline(),
            vec![(
                started + latency,
                Decoded::Cct {
                    temp_hk: 42,
                    bri: 75
                }
            )]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn every_nth_write_fails_globally() {
        let id = LightId::sim("1");
        let sim = transport(
            vec![light("1", "NEEWER", 0)],
            Duration::ZERO,
            NonZeroU32::new(3),
        );
        let link = sim.connect(&id, Duration::from_secs(1)).await.unwrap();
        let mut outcomes = Vec::new();
        for bri in 1..=6 {
            outcomes.push(
                link.write(&cct_packet(bri), WriteKind::WithResponse)
                    .await
                    .is_ok(),
            );
        }

        assert_eq!(outcomes, [true, true, false, true, true, false]);
        let brightnesses: Vec<_> = sim
            .light(&id)
            .timeline()
            .into_iter()
            .map(|(_, decoded)| match decoded {
                Decoded::Cct { bri, .. } => bri,
                other => panic!("unexpected decoded command {other:?}"),
            })
            .collect();
        assert_eq!(brightnesses, [1, 2, 4, 5]);
    }

    #[tokio::test]
    async fn forced_disconnect_closes_link_and_preserves_timeline() {
        let id = LightId::sim("1");
        let sim = transport(vec![light("1", "NEEWER", 0)], Duration::ZERO, None);
        let link = sim.connect(&id, Duration::from_secs(1)).await.unwrap();
        link.write(&cct_packet(50), WriteKind::WithResponse)
            .await
            .unwrap();
        sim.light(&id).force_disconnect();

        timeout(Duration::from_millis(10), link.closed())
            .await
            .unwrap();
        assert!(!link.is_connected());
        assert!(matches!(
            link.write(&cct_packet(60), WriteKind::WithResponse).await,
            Err(TransportError::Disconnected { .. })
        ));
        assert_eq!(sim.light(&id).timeline().len(), 1);
    }

    #[tokio::test]
    async fn garbage_is_rejected_without_recording_it() {
        let id = LightId::sim("1");
        let sim = transport(vec![light("1", "NEEWER", 0)], Duration::ZERO, None);
        let link = sim.connect(&id, Duration::from_secs(1)).await.unwrap();

        assert!(matches!(
            link.write(&[0xde, 0xad], WriteKind::WithoutResponse).await,
            Err(TransportError::WriteFailed { .. })
        ));
        assert!(sim.light(&id).timeline().is_empty());
    }

    #[tokio::test]
    async fn notification_reaches_subscriber() {
        let id = LightId::sim("1");
        let sim = transport(vec![light("1", "NEEWER", 0)], Duration::ZERO, None);
        let link = sim.connect(&id, Duration::from_secs(1)).await.unwrap();
        let mut notifications = link.notifications().await.unwrap();

        sim.light(&id).push_notification(vec![1, 2, 3]);
        assert_eq!(notifications.next().await, Some(vec![1, 2, 3]));
    }

    #[tokio::test]
    async fn second_connection_replaces_first() {
        let id = LightId::sim("1");
        let sim = transport(vec![light("1", "NEEWER", 0)], Duration::ZERO, None);
        let old = sim.connect(&id, Duration::from_secs(1)).await.unwrap();
        let new = sim.connect(&id, Duration::from_secs(1)).await.unwrap();

        timeout(Duration::from_millis(10), old.closed())
            .await
            .unwrap();
        assert!(matches!(
            old.write(&cct_packet(10), WriteKind::WithResponse).await,
            Err(TransportError::Disconnected { .. })
        ));
        new.write(&cct_packet(20), WriteKind::WithResponse)
            .await
            .unwrap();
        assert!(new.is_connected());
        assert_eq!(sim.light(&id).timeline().len(), 1);
    }
}
