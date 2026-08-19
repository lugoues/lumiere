use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use futures::{StreamExt, future::join_all};
use lumiere_core::caps::ModelTable;
use lumiere_proto::{
    ConnState, Event, LightId, LightSnapshot, Mode, PerLightResult, Selector, SeqEvent, SkipReason,
    WorldSnapshot,
};
use lumiere_transport::{Discovered, ScanFilter, Transport};
use tokio::sync::{Semaphore, broadcast, mpsc, oneshot, watch};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{
    light::{LightActor, LightActorArgs, LightOp},
    store::StoreUpdate,
};

const EVENT_RING_CAPACITY: usize = 256;
const REGISTRY_CHANNEL_CAPACITY: usize = 256;
const COMMAND_CHANNEL_CAPACITY: usize = 32;
const LIGHT_OP_CAPACITY: usize = 8;
const GLOBAL_WRITE_PERMITS: usize = 4;

/// Configuration shared by all light actors in a registry.
#[derive(Clone, Debug)]
pub struct RegistryConfig {
    pub min_write_interval: Duration,
    pub connect_timeout: Duration,
    pub labels: HashMap<LightId, String>,
    pub store_updates: Option<mpsc::Sender<StoreUpdate>>,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            min_write_interval: Duration::from_millis(50),
            connect_timeout: Duration::from_secs(10),
            labels: HashMap::new(),
            store_updates: None,
        }
    }
}

/// A command accepted by the registry actor.
pub enum RegistryCmd {
    Discover {
        duration: Duration,
    },
    Connect {
        id: LightId,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Disconnect {
        id: LightId,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SetLabel {
        id: LightId,
        label: String,
        reply: oneshot::Sender<Result<LightSnapshot, String>>,
    },
    SetMode {
        selector: Selector,
        mode: Mode,
        wait: bool,
        reply: oneshot::Sender<Vec<PerLightResult>>,
    },
    Shutdown,
}

pub(crate) enum RegistryInput {
    Discovered(Discovered),
    Connection {
        id: LightId,
        conn: ConnState,
        error: Option<String>,
        ack: oneshot::Sender<()>,
    },
    WriteOutcome {
        result: PerLightResult,
        ack: oneshot::Sender<()>,
    },
}

struct LightHandle {
    snapshot: LightSnapshot,
    desired_tx: watch::Sender<Option<Mode>>,
    ops_tx: mpsc::Sender<LightOp>,
}

/// A clonable interface to a running registry actor.
#[derive(Clone)]
pub struct RegistryHandle {
    cmd_tx: mpsc::Sender<RegistryCmd>,
    world_rx: watch::Receiver<Arc<WorldSnapshot>>,
    events_tx: broadcast::Sender<SeqEvent>,
    ring_rx: watch::Receiver<Arc<VecDeque<SeqEvent>>>,
    done: CancellationToken,
}

impl RegistryHandle {
    /// Starts a registry using the default actor configuration.
    pub fn spawn<T>(transport: T) -> Self
    where
        T: Transport,
    {
        Self::spawn_with_config(transport, RegistryConfig::default())
    }

    /// Starts a registry using an explicit actor configuration.
    pub fn spawn_with_config<T>(transport: T, config: RegistryConfig) -> Self
    where
        T: Transport,
    {
        let transport: Arc<dyn Transport> = Arc::new(transport);
        let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        let (input_tx, input_rx) = mpsc::channel(REGISTRY_CHANNEL_CAPACITY);
        let initial_world = Arc::new(WorldSnapshot {
            seq: 0,
            lights: Vec::new(),
        });
        let (world_tx, world_rx) = watch::channel(initial_world);
        let (events_tx, _) = broadcast::channel(EVENT_RING_CAPACITY);
        let (ring_tx, ring_rx) = watch::channel(Arc::new(VecDeque::new()));
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let tracker = TaskTracker::new();
        let registry = Registry {
            transport,
            config,
            cmd_rx,
            input_tx,
            input_rx,
            world_tx,
            events_tx: events_tx.clone(),
            ring_tx,
            cancel,
            done: done.clone(),
            tracker,
            write_permits: Arc::new(Semaphore::new(GLOBAL_WRITE_PERMITS)),
            lights: Vec::new(),
            seq: 0,
            ring: VecDeque::with_capacity(EVENT_RING_CAPACITY),
        };
        tokio::spawn(registry.run());

        Self {
            cmd_tx,
            world_rx,
            events_tx,
            ring_rx,
            done,
        }
    }

    /// Starts a discovery scan and returns once the registry accepts it.
    pub async fn discover(&self, duration: Duration) -> Result<(), String> {
        self.cmd_tx
            .send(RegistryCmd::Discover { duration })
            .await
            .map_err(|_| "registry is stopped".to_owned())
    }

    /// Requests a connection and returns the light actor's result.
    pub async fn connect(&self, id: LightId) -> Result<(), String> {
        let (reply, result) = oneshot::channel();
        self.cmd_tx
            .send(RegistryCmd::Connect { id, reply })
            .await
            .map_err(|_| "registry is stopped".to_owned())?;
        result
            .await
            .map_err(|_| "light actor stopped before replying".to_owned())?
    }

    /// Requests a disconnection and returns the light actor's result.
    pub async fn disconnect(&self, id: LightId) -> Result<(), String> {
        let (reply, result) = oneshot::channel();
        self.cmd_tx
            .send(RegistryCmd::Disconnect { id, reply })
            .await
            .map_err(|_| "registry is stopped".to_owned())?;
        result
            .await
            .map_err(|_| "light actor stopped before replying".to_owned())?
    }

    /// Changes a light label and queues it for persistence.
    pub async fn set_label(&self, id: LightId, label: String) -> Result<LightSnapshot, String> {
        let (reply, result) = oneshot::channel();
        self.cmd_tx
            .send(RegistryCmd::SetLabel { id, label, reply })
            .await
            .map_err(|_| "registry is stopped".to_owned())?;
        result
            .await
            .map_err(|_| "registry stopped before replying".to_owned())?
    }

    /// Applies a mode to the selected lights.
    pub async fn set_mode(
        &self,
        selector: Selector,
        mode: Mode,
        wait: bool,
    ) -> Result<Vec<PerLightResult>, String> {
        let (reply, result) = oneshot::channel();
        self.cmd_tx
            .send(RegistryCmd::SetMode {
                selector,
                mode,
                wait,
                reply,
            })
            .await
            .map_err(|_| "registry is stopped".to_owned())?;
        result
            .await
            .map_err(|_| "registry stopped before replying".to_owned())
    }

    /// Subscribes to complete world snapshots.
    pub fn world(&self) -> watch::Receiver<Arc<WorldSnapshot>> {
        self.world_rx.clone()
    }

    /// Subscribes to sequenced world events.
    pub fn events(&self) -> broadcast::Receiver<SeqEvent> {
        self.events_tx.subscribe()
    }

    /// Replays events newer than `seq`, or returns `None` if the gap was evicted.
    pub fn events_since(&self, seq: u64) -> Option<Vec<SeqEvent>> {
        let ring = self.ring_rx.borrow();
        let Some(newest) = ring.back().map(|event| event.seq) else {
            return Some(Vec::new());
        };
        if seq >= newest {
            return Some(Vec::new());
        }
        let oldest = ring.front()?.seq;
        if seq.saturating_add(1) < oldest {
            return None;
        }
        Some(
            ring.iter()
                .filter(|event| event.seq > seq)
                .cloned()
                .collect(),
        )
    }

    /// Stops scans and actors, disconnects links, and waits for tracked tasks.
    pub async fn shutdown(&self) {
        if !self.done.is_cancelled() {
            let _ = self.cmd_tx.send(RegistryCmd::Shutdown).await;
            self.done.cancelled().await;
        }
    }
}

/// Starts a registry using the default actor configuration.
pub fn spawn_registry<T>(transport: T) -> RegistryHandle
where
    T: Transport,
{
    RegistryHandle::spawn(transport)
}

struct Registry {
    transport: Arc<dyn Transport>,
    config: RegistryConfig,
    cmd_rx: mpsc::Receiver<RegistryCmd>,
    input_tx: mpsc::Sender<RegistryInput>,
    input_rx: mpsc::Receiver<RegistryInput>,
    world_tx: watch::Sender<Arc<WorldSnapshot>>,
    events_tx: broadcast::Sender<SeqEvent>,
    ring_tx: watch::Sender<Arc<VecDeque<SeqEvent>>>,
    cancel: CancellationToken,
    done: CancellationToken,
    tracker: TaskTracker,
    write_permits: Arc<Semaphore>,
    lights: Vec<LightHandle>,
    seq: u64,
    ring: VecDeque<SeqEvent>,
}

impl Registry {
    async fn run(mut self) {
        loop {
            tokio::select! {
                biased;
                command = self.cmd_rx.recv() => match command {
                    Some(RegistryCmd::Shutdown) | None => break,
                    Some(command) => self.handle_command(command).await,
                },
                input = self.input_rx.recv() => {
                    if let Some(input) = input {
                        self.handle_input(input).await;
                    }
                }
            }
        }

        self.cancel.cancel();
        let tracker = self.tracker.clone();
        tracker.close();
        let wait = tracker.wait();
        tokio::pin!(wait);
        loop {
            tokio::select! {
                biased;
                input = self.input_rx.recv() => {
                    if let Some(input) = input {
                        self.handle_input(input).await;
                    }
                }
                () = &mut wait => break,
            }
        }
        self.done.cancel();
    }

    async fn handle_command(&mut self, command: RegistryCmd) {
        match command {
            RegistryCmd::Discover { duration } => self.start_scan(duration),
            RegistryCmd::Connect { id, reply } => {
                match self.lights.iter().find(|light| light.snapshot.id == id) {
                    Some(light) => {
                        if let Err(error) = light.ops_tx.send(LightOp::Connect { reply }).await
                            && let LightOp::Connect { reply } = error.0
                        {
                            let _ = reply.send(Err("light actor stopped".to_owned()));
                        }
                    }
                    None => {
                        let _ = reply.send(Err(format!("light {id} was not found")));
                    }
                }
            }
            RegistryCmd::Disconnect { id, reply } => {
                match self.lights.iter().find(|light| light.snapshot.id == id) {
                    Some(light) => {
                        if let Err(error) = light.ops_tx.send(LightOp::Disconnect { reply }).await
                            && let LightOp::Disconnect { reply } = error.0
                        {
                            let _ = reply.send(Err("light actor stopped".to_owned()));
                        }
                    }
                    None => {
                        let _ = reply.send(Err(format!("light {id} was not found")));
                    }
                }
            }
            RegistryCmd::SetLabel { id, label, reply } => {
                let Some(index) = self.index_of(&id) else {
                    let _ = reply.send(Err(format!("light {id} was not found")));
                    return;
                };
                self.lights[index].snapshot.label.clone_from(&label);
                self.config.labels.insert(id.clone(), label.clone());
                self.emit(index);
                if let Some(store_updates) = &self.config.store_updates {
                    let _ = store_updates.send(StoreUpdate { id, label }).await;
                }
                let _ = reply.send(Ok(self.lights[index].snapshot.clone()));
            }
            RegistryCmd::SetMode {
                selector,
                mode,
                wait,
                reply,
            } => self.set_mode(selector, mode, wait, reply).await,
            RegistryCmd::Shutdown => {}
        }
    }

    fn start_scan(&self, duration: Duration) {
        let transport = Arc::clone(&self.transport);
        let input_tx = self.input_tx.clone();
        let cancel = self.cancel.clone();
        self.tracker.spawn(async move {
            let Ok(mut scan) = transport.scan(ScanFilter::default()).await else {
                return;
            };
            let deadline = tokio::time::sleep(duration);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = &mut deadline => break,
                    discovered = scan.events.next() => match discovered {
                        Some(discovered) => {
                            if input_tx.send(RegistryInput::Discovered(discovered)).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        });
    }

    async fn set_mode(
        &mut self,
        selector: Selector,
        mode: Mode,
        wait: bool,
        reply: oneshot::Sender<Vec<PerLightResult>>,
    ) {
        let targets = self.target_indices(&selector);
        for &index in &targets {
            self.lights[index].snapshot.desired = Some(mode);
            self.emit(index);
        }

        if !wait {
            for index in targets {
                self.lights[index].desired_tx.send_replace(Some(mode));
            }
            let _ = reply.send(Vec::new());
            return;
        }

        let mut pending = Vec::with_capacity(targets.len());
        for index in targets {
            // Keep the desired watch in sync so a later reconnect replays this mode,
            // not an older one. The actor dedupes the echo via last_written.
            self.lights[index].desired_tx.send_replace(Some(mode));
            let id = self.lights[index].snapshot.id.clone();
            let (actor_reply, actor_result) = oneshot::channel();
            let result = match self.lights[index]
                .ops_tx
                .send(LightOp::ApplyNow {
                    mode,
                    reply: actor_reply,
                })
                .await
            {
                Ok(()) => PendingResult::Actor { id, actor_result },
                Err(_) => PendingResult::Failed {
                    id,
                    error: "light actor stopped".to_owned(),
                },
            };
            pending.push(result);
        }
        self.tracker.spawn(async move {
            let results = join_all(pending.into_iter().map(PendingResult::resolve)).await;
            let _ = reply.send(results);
        });
    }

    fn target_indices(&self, selector: &Selector) -> Vec<usize> {
        self.lights
            .iter()
            .enumerate()
            .filter(|(_, light)| match selector {
                Selector::All => true,
                Selector::Ids { ids } => ids.contains(&light.snapshot.id),
            })
            .map(|(index, _)| index)
            .collect()
    }

    async fn handle_input(&mut self, input: RegistryInput) {
        match input {
            RegistryInput::Discovered(discovered) => self.discovered(discovered).await,
            RegistryInput::Connection {
                id,
                conn,
                error,
                ack,
            } => {
                if let Some(index) = self.index_of(&id) {
                    self.lights[index].snapshot.conn = conn;
                    if error.is_some() || self.lights[index].snapshot.conn == ConnState::Connected {
                        self.lights[index].snapshot.last_error = error;
                    }
                    self.emit(index);
                }
                let _ = ack.send(());
            }
            RegistryInput::WriteOutcome { result, ack } => {
                self.apply_result(&result);
                let _ = ack.send(());
            }
        }
    }

    async fn discovered(&mut self, discovered: Discovered) {
        if let Some(index) = self.index_of(&discovered.id) {
            if self.lights[index].snapshot.rssi != discovered.rssi {
                self.lights[index].snapshot.rssi = discovered.rssi;
                self.emit(index);
            }
            return;
        }

        let model = discovered.name.unwrap_or_else(|| discovered.id.to_string());
        let caps = ModelTable::builtin().resolve(&model);
        let label = self
            .config
            .labels
            .get(&discovered.id)
            .cloned()
            .unwrap_or_else(|| model.clone());
        let snapshot = LightSnapshot {
            id: discovered.id.clone(),
            model: model.clone(),
            label,
            caps: caps.clone(),
            conn: ConnState::Discovered,
            rssi: discovered.rssi,
            desired: None,
            confirmed: None,
            power: None,
            last_error: None,
        };
        let (desired_tx, desired_rx) = watch::channel(None);
        let (ops_tx, ops_rx) = mpsc::channel(LIGHT_OP_CAPACITY);
        let actor = LightActor::new(LightActorArgs {
            id: discovered.id,
            caps,
            transport: Arc::clone(&self.transport),
            registry_tx: self.input_tx.clone(),
            desired_rx,
            ops_rx,
            write_permits: Arc::clone(&self.write_permits),
            min_interval: self.config.min_write_interval,
            connect_timeout: self.config.connect_timeout,
            cancel: self.cancel.clone(),
        });
        self.tracker.spawn(actor.run());
        self.lights.push(LightHandle {
            snapshot,
            desired_tx,
            ops_tx: ops_tx.clone(),
        });
        self.emit(self.lights.len() - 1);

        let (reply, _) = oneshot::channel();
        let _ = ops_tx.send(LightOp::Connect { reply }).await;
    }

    fn apply_result(&mut self, result: &PerLightResult) {
        let id = result_id(result);
        let Some(index) = self.index_of(id) else {
            return;
        };
        let snapshot = &mut self.lights[index].snapshot;
        match result {
            PerLightResult::Applied { mode, .. } => {
                snapshot.confirmed = Some(*mode);
                update_power(snapshot, *mode);
                snapshot.last_error = None;
            }
            PerLightResult::Adapted { applied, .. } => {
                snapshot.confirmed = Some(*applied);
                update_power(snapshot, *applied);
                snapshot.last_error = None;
            }
            PerLightResult::Coalesced { .. } => {}
            PerLightResult::Skipped { reason, .. } => {
                snapshot.last_error = Some(
                    match reason {
                        SkipReason::NotConnected => "not connected",
                        SkipReason::UnsupportedMode => "unsupported mode",
                    }
                    .to_owned(),
                );
            }
            PerLightResult::Failed { error, .. } => {
                snapshot.last_error = Some(error.clone());
            }
        }
        self.emit(index);
    }

    fn index_of(&self, id: &LightId) -> Option<usize> {
        self.lights
            .iter()
            .position(|light| &light.snapshot.id == id)
    }

    fn emit(&mut self, index: usize) {
        self.seq += 1;
        let event = SeqEvent {
            seq: self.seq,
            event: Event::Light {
                light: self.lights[index].snapshot.clone(),
            },
        };
        if self.ring.len() == EVENT_RING_CAPACITY {
            self.ring.pop_front();
        }
        self.ring.push_back(event.clone());

        let world = Arc::new(WorldSnapshot {
            seq: self.seq,
            lights: self
                .lights
                .iter()
                .map(|light| light.snapshot.clone())
                .collect(),
        });
        self.ring_tx.send_replace(Arc::new(self.ring.clone()));
        self.world_tx.send_replace(world);
        let _ = self.events_tx.send(event);
    }
}

enum PendingResult {
    Actor {
        id: LightId,
        actor_result: oneshot::Receiver<PerLightResult>,
    },
    Failed {
        id: LightId,
        error: String,
    },
}

impl PendingResult {
    async fn resolve(self) -> PerLightResult {
        match self {
            Self::Actor { id, actor_result } => {
                actor_result
                    .await
                    .unwrap_or_else(|_| PerLightResult::Failed {
                        id,
                        error: "light actor stopped before replying".to_owned(),
                    })
            }
            Self::Failed { id, error } => PerLightResult::Failed { id, error },
        }
    }
}

fn result_id(result: &PerLightResult) -> &LightId {
    match result {
        PerLightResult::Applied { id, .. }
        | PerLightResult::Coalesced { id }
        | PerLightResult::Adapted { id, .. }
        | PerLightResult::Skipped { id, .. }
        | PerLightResult::Failed { id, .. } => id,
    }
}

fn update_power(snapshot: &mut LightSnapshot, mode: Mode) {
    match mode {
        Mode::On => snapshot.power = Some(true),
        Mode::Off => snapshot.power = Some(false),
        Mode::Cct { .. } | Mode::Hsi { .. } | Mode::Scene { .. } => {}
    }
}
