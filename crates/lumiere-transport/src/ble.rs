//! Bluetooth Low Energy transport backed by `btleplug`.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use btleplug::{
    api::{
        Central, CentralEvent, Characteristic, Manager as _, Peripheral as _,
        ScanFilter as BtleScanFilter, WriteType,
    },
    platform::{Adapter, Manager, Peripheral, PeripheralId},
};
use futures::{StreamExt, stream::BoxStream};
use lumiere_proto::LightId;
use tokio::sync::{Mutex as AsyncMutex, broadcast, oneshot};

use crate::{
    AdapterState, Discovered, Link, Scan, ScanFilter, Transport, TransportError, WriteKind,
};

const WRITE_UUID: &str = "69400002-b5a3-f393-e0a9-e50e24dcca99";
const NOTIFY_UUID: &str = "69400003-b5a3-f393-e0a9-e50e24dcca99";

type Connections = Arc<Mutex<HashMap<PeripheralId, Vec<Weak<AtomicBool>>>>>;

/// A hardware BLE transport using the first available system adapter.
pub struct BleTransport {
    manager: Manager,
    adapter: Adapter,
    ids: Arc<Mutex<HashMap<LightId, PeripheralId>>>,
    events: broadcast::Sender<CentralEvent>,
    connections: Connections,
}

impl BleTransport {
    /// Uses the first available adapter.
    pub async fn new() -> Result<Self, TransportError> {
        let manager = Manager::new().await.map_err(|error| backend(None, error))?;
        let adapter = manager
            .adapters()
            .await
            .map_err(|error| backend(None, error))?
            .into_iter()
            .next()
            .ok_or(TransportError::AdapterUnavailable)?;
        let (events, _) = broadcast::channel(256);
        let connections = Connections::default();
        let pump_adapter = adapter.clone();
        let pump_events = events.clone();
        let pump_connections = Arc::clone(&connections);
        tokio::spawn(async move {
            let mut platform_events = match pump_adapter.events().await {
                Ok(events) => events,
                Err(error) => {
                    tracing::error!(%error, "failed to open Bluetooth adapter event stream");
                    return;
                }
            };
            while let Some(event) = platform_events.next().await {
                if let CentralEvent::DeviceDisconnected(id) = &event {
                    let mut connections = pump_connections
                        .lock()
                        .expect("BLE connection registry mutex poisoned");
                    if let Some(flags) = connections.get_mut(id) {
                        flags.retain(|flag| {
                            if let Some(flag) = flag.upgrade() {
                                flag.store(false, Ordering::Release);
                                true
                            } else {
                                false
                            }
                        });
                    }
                }
                let _ = pump_events.send(event);
            }
        });
        Ok(Self {
            manager,
            adapter,
            ids: Arc::new(Mutex::new(HashMap::new())),
            events,
            connections,
        })
    }

    async fn refresh_peripherals(&self) -> Result<(), TransportError> {
        for peripheral in self
            .adapter
            .peripherals()
            .await
            .map_err(|error| backend(None, error))?
        {
            let peripheral_id = peripheral.id();
            if let Some(id) = light_id_from_peripheral(&peripheral_id) {
                self.ids
                    .lock()
                    .expect("BLE identifier map mutex poisoned")
                    .insert(id, peripheral_id);
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Transport for BleTransport {
    async fn scan(&self, filter: ScanFilter) -> Result<Scan, TransportError> {
        let receiver = self.events.subscribe();
        self.adapter
            .start_scan(BtleScanFilter::default())
            .await
            .map_err(|error| backend(None, error))?;
        let (stop, stopped) = oneshot::channel();
        let stop_adapter = self.adapter.clone();
        tokio::spawn(async move {
            let _ = stopped.await;
            if let Err(error) = stop_adapter.stop_scan().await {
                tracing::debug!(%error, "failed to stop Bluetooth scan");
            }
        });
        let state = ScanState {
            receiver,
            adapter: self.adapter.clone(),
            ids: Arc::clone(&self.ids),
            filter,
            reported: HashMap::new(),
            stop: Some(stop),
        };
        let events = futures::stream::unfold(state, |mut state| async move {
            loop {
                let event = match state.receiver.recv().await {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                };
                let peripheral_id = match event {
                    CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) => id,
                    _ => continue,
                };
                let Some(id) = light_id_from_peripheral(&peripheral_id) else {
                    continue;
                };
                let peripheral = match state.adapter.peripheral(&peripheral_id).await {
                    Ok(peripheral) => peripheral,
                    Err(error) => {
                        tracing::debug!(%error, "could not inspect discovered peripheral");
                        continue;
                    }
                };
                let (name, rssi) = match peripheral.properties().await {
                    Ok(Some(properties)) => (properties.local_name, properties.rssi),
                    Ok(None) => (None, None),
                    Err(error) => {
                        tracing::debug!(%error, %id, "could not read peripheral properties");
                        continue;
                    }
                };
                if state.filter.name_prefix.as_ref().is_some_and(|prefix| {
                    name.as_ref().is_none_or(|name| !name.starts_with(prefix))
                }) {
                    continue;
                }
                if state
                    .reported
                    .get(&id)
                    .is_some_and(|previous| match (previous, rssi) {
                        (Some(previous), Some(current)) => {
                            (i32::from(*previous) - i32::from(current)).abs() < 5
                        }
                        _ => true,
                    })
                {
                    continue;
                }
                state.reported.insert(id.clone(), rssi);
                state
                    .ids
                    .lock()
                    .expect("BLE identifier map mutex poisoned")
                    .insert(id.clone(), peripheral_id);
                return Some((Discovered { id, name, rssi }, state));
            }
        })
        .boxed();
        Ok(Scan { events })
    }

    async fn connect(
        &self,
        id: &LightId,
        timeout: Duration,
    ) -> Result<Box<dyn Link>, TransportError> {
        self.refresh_peripherals().await?;
        let peripheral_id = self
            .ids
            .lock()
            .expect("BLE identifier map mutex poisoned")
            .get(id)
            .cloned()
            .ok_or_else(|| TransportError::NotFound { id: id.clone() })?;
        let peripheral = self
            .adapter
            .peripheral(&peripheral_id)
            .await
            .map_err(|error| backend(Some(id.clone()), error))?;
        let result = tokio::time::timeout(timeout, async {
            peripheral.connect().await?;
            peripheral.discover_services().await
        })
        .await
        .map_err(|_| TransportError::TimedOut {
            id: id.clone(),
            timeout,
        })?;
        result.map_err(|error| backend(Some(id.clone()), error))?;
        let characteristics = peripheral.characteristics();
        let write = characteristics
            .iter()
            .find(|characteristic| characteristic.uuid.to_string() == WRITE_UUID)
            .cloned()
            .ok_or_else(|| {
                backend_message(
                    Some(id.clone()),
                    format!("required write characteristic {WRITE_UUID} is missing"),
                )
            })?;
        let notify = characteristics
            .iter()
            .find(|characteristic| characteristic.uuid.to_string() == NOTIFY_UUID)
            .cloned();
        let connected = Arc::new(AtomicBool::new(true));
        self.connections
            .lock()
            .expect("BLE connection registry mutex poisoned")
            .entry(peripheral_id.clone())
            .or_default()
            .push(Arc::downgrade(&connected));
        Ok(Box::new(BleLink {
            id: id.clone(),
            peripheral_id,
            peripheral,
            write,
            notify,
            subscribed: AsyncMutex::new(false),
            connected,
            events: self.events.clone(),
        }))
    }

    async fn adapter_state(&self) -> AdapterState {
        match self.manager.adapters().await {
            Ok(adapters) if !adapters.is_empty() => AdapterState::Ready,
            Ok(_) | Err(_) => AdapterState::Unavailable,
        }
    }
}

struct ScanState {
    receiver: broadcast::Receiver<CentralEvent>,
    adapter: Adapter,
    ids: Arc<Mutex<HashMap<LightId, PeripheralId>>>,
    filter: ScanFilter,
    reported: HashMap<LightId, Option<i16>>,
    stop: Option<oneshot::Sender<()>>,
}

impl Drop for ScanState {
    fn drop(&mut self) {
        self.stop.take();
    }
}

struct BleLink {
    id: LightId,
    peripheral_id: PeripheralId,
    peripheral: Peripheral,
    write: Characteristic,
    notify: Option<Characteristic>,
    subscribed: AsyncMutex<bool>,
    connected: Arc<AtomicBool>,
    events: broadcast::Sender<CentralEvent>,
}

#[async_trait::async_trait]
impl Link for BleLink {
    fn id(&self) -> &LightId {
        &self.id
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    async fn write(&self, payload: &[u8], kind: WriteKind) -> Result<(), TransportError> {
        if !self.is_connected() {
            return Err(TransportError::Disconnected {
                id: self.id.clone(),
            });
        }
        let write_type = match kind {
            WriteKind::WithResponse => WriteType::WithResponse,
            WriteKind::WithoutResponse => WriteType::WithoutResponse,
        };
        self.peripheral
            .write(&self.write, payload, write_type)
            .await
            .map_err(|error| TransportError::WriteFailed {
                id: self.id.clone(),
                message: error.to_string(),
            })
    }

    async fn notifications(&self) -> Result<BoxStream<'static, Vec<u8>>, TransportError> {
        let notify = self.notify.as_ref().ok_or_else(|| {
            backend_message(
                Some(self.id.clone()),
                format!("notify characteristic {NOTIFY_UUID} is missing"),
            )
        })?;
        let mut subscribed = self.subscribed.lock().await;
        if !*subscribed {
            self.peripheral
                .subscribe(notify)
                .await
                .map_err(|error| backend(Some(self.id.clone()), error))?;
            *subscribed = true;
        }
        drop(subscribed);
        let uuid = notify.uuid;
        let stream = self
            .peripheral
            .notifications()
            .await
            .map_err(|error| backend(Some(self.id.clone()), error))?;
        Ok(stream
            .filter_map(move |notification| async move {
                (notification.uuid == uuid).then_some(notification.value)
            })
            .boxed())
    }

    async fn closed(&self) {
        if !self.is_connected() {
            return;
        }
        let mut receiver = self.events.subscribe();
        if !self.is_connected() {
            return;
        }
        loop {
            match receiver.recv().await {
                Ok(CentralEvent::DeviceDisconnected(id)) if id == self.peripheral_id => {
                    self.connected.store(false, Ordering::Release);
                    return;
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    // The pump may have flipped the flag while we lagged.
                    if !self.is_connected() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }

    async fn disconnect(&self) -> Result<(), TransportError> {
        self.peripheral
            .disconnect()
            .await
            .map_err(|error| backend(Some(self.id.clone()), error))?;
        self.connected.store(false, Ordering::Release);
        Ok(())
    }
}

fn light_id_from_peripheral(id: &PeripheralId) -> Option<LightId> {
    let raw = id.to_string();
    LightId::mac(&raw)
        .or_else(|_| LightId::corebluetooth(&raw))
        .map_err(|error| {
            tracing::debug!(%error, peripheral_id = %raw, "unsupported peripheral identifier");
            error
        })
        .ok()
}

fn backend(id: Option<LightId>, error: impl std::fmt::Display) -> TransportError {
    backend_message(id, error.to_string())
}

fn backend_message(id: Option<LightId>, message: String) -> TransportError {
    TransportError::Backend { id, message }
}
