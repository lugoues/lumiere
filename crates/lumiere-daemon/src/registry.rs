use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::{StreamExt, future::join_all};
use lumiere_core::{caps::ModelTable, playback_duration, schedule};
use lumiere_proto::{
    AnimTarget, Animation, AnimationId, AnimationSummary, ConnState, Event, LightId, LightSnapshot,
    Mode, PerLightResult, PlaybackOptions, PlaybackStatus, Preset, PresetEntry, PresetId,
    PresetTarget, Selector, SeqEvent, SkipReason, TargetBinding, WorldSnapshot,
};
use lumiere_transport::{Discovered, ScanFilter, Transport};
use tokio::sync::{Semaphore, broadcast, mpsc, oneshot, watch};
use tokio::time::Instant;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing::warn;

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
    pub animations_dir: Option<PathBuf>,
    pub presets: Vec<Preset>,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            min_write_interval: Duration::from_millis(50),
            connect_timeout: Duration::from_secs(10),
            labels: HashMap::new(),
            store_updates: None,
            animations_dir: None,
            presets: Vec::new(),
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
    ListAnimations {
        reply: oneshot::Sender<Vec<AnimationSummary>>,
    },
    GetAnimation {
        id: AnimationId,
        reply: oneshot::Sender<Option<Animation>>,
    },
    Play {
        id: AnimationId,
        options: PlaybackOptions,
        binding: TargetBinding,
        reply: oneshot::Sender<Result<PlaybackStatus, String>>,
    },
    StopPlayback {
        reply: oneshot::Sender<bool>,
    },
    ListPresets {
        reply: oneshot::Sender<Vec<Preset>>,
    },
    SavePreset {
        name: String,
        selector: Selector,
        reply: oneshot::Sender<Result<Preset, String>>,
    },
    RecallPreset {
        id: PresetId,
        wait: bool,
        reply: oneshot::Sender<Result<Vec<PerLightResult>, String>>,
    },
    RenamePreset {
        id: PresetId,
        name: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    DeletePreset {
        id: PresetId,
        reply: oneshot::Sender<Result<(), String>>,
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
    PlaybackFinished {
        playback_id: u64,
        reason: PlaybackFinishReason,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum PlaybackFinishReason {
    Natural,
    Cancelled,
    Chained,
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
            playback: None,
        });
        let (world_tx, world_rx) = watch::channel(initial_world);
        let (events_tx, _) = broadcast::channel(EVENT_RING_CAPACITY);
        let (ring_tx, ring_rx) = watch::channel(Arc::new(VecDeque::new()));
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let tracker = TaskTracker::new();
        let animations = config
            .animations_dir
            .as_deref()
            .map(load_animations)
            .unwrap_or_default();
        let presets = config.presets.clone();
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
            animations,
            presets,
            active: None,
            next_playback_id: 1,
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

    /// Lists all loaded animations in identifier order.
    pub async fn list_animations(&self) -> Result<Vec<AnimationSummary>, String> {
        let (reply, result) = oneshot::channel();
        self.cmd_tx
            .send(RegistryCmd::ListAnimations { reply })
            .await
            .map_err(|_| "registry is stopped".to_owned())?;
        result
            .await
            .map_err(|_| "registry stopped before replying".to_owned())
    }

    /// Returns one loaded animation definition.
    pub async fn animation(&self, id: AnimationId) -> Result<Option<Animation>, String> {
        let (reply, result) = oneshot::channel();
        self.cmd_tx
            .send(RegistryCmd::GetAnimation { id, reply })
            .await
            .map_err(|_| "registry is stopped".to_owned())?;
        result
            .await
            .map_err(|_| "registry stopped before replying".to_owned())
    }

    /// Starts an animation after resolving its target binding once.
    pub async fn play(
        &self,
        id: AnimationId,
        options: PlaybackOptions,
        binding: TargetBinding,
    ) -> Result<PlaybackStatus, String> {
        let (reply, result) = oneshot::channel();
        self.cmd_tx
            .send(RegistryCmd::Play {
                id,
                options,
                binding,
                reply,
            })
            .await
            .map_err(|_| "registry is stopped".to_owned())?;
        result
            .await
            .map_err(|_| "registry stopped before replying".to_owned())?
    }

    /// Stops the active playback, returning false when none was running.
    pub async fn stop_playback(&self) -> Result<bool, String> {
        let (reply, result) = oneshot::channel();
        self.cmd_tx
            .send(RegistryCmd::StopPlayback { reply })
            .await
            .map_err(|_| "registry is stopped".to_owned())?;
        result
            .await
            .map_err(|_| "registry stopped before replying".to_owned())
    }

    /// Lists presets in their user-defined order.
    pub async fn list_presets(&self) -> Result<Vec<Preset>, String> {
        let (reply, result) = oneshot::channel();
        self.cmd_tx
            .send(RegistryCmd::ListPresets { reply })
            .await
            .map_err(|_| "registry is stopped".to_owned())?;
        result
            .await
            .map_err(|_| "registry stopped before replying".to_owned())
    }

    /// Captures the current modes of selected connected lights.
    pub async fn save_preset(&self, name: String, selector: Selector) -> Result<Preset, String> {
        let (reply, result) = oneshot::channel();
        self.cmd_tx
            .send(RegistryCmd::SavePreset {
                name,
                selector,
                reply,
            })
            .await
            .map_err(|_| "registry is stopped".to_owned())?;
        result
            .await
            .map_err(|_| "registry stopped before replying".to_owned())?
    }

    /// Recalls a preset, optionally waiting for per-light results.
    pub async fn recall_preset(
        &self,
        id: PresetId,
        wait: bool,
    ) -> Result<Vec<PerLightResult>, String> {
        let (reply, result) = oneshot::channel();
        self.cmd_tx
            .send(RegistryCmd::RecallPreset { id, wait, reply })
            .await
            .map_err(|_| "registry is stopped".to_owned())?;
        result
            .await
            .map_err(|_| "registry stopped before replying".to_owned())?
    }

    /// Renames a preset without changing its identifier.
    pub async fn rename_preset(&self, id: PresetId, name: String) -> Result<(), String> {
        let (reply, result) = oneshot::channel();
        self.cmd_tx
            .send(RegistryCmd::RenamePreset { id, name, reply })
            .await
            .map_err(|_| "registry is stopped".to_owned())?;
        result
            .await
            .map_err(|_| "registry stopped before replying".to_owned())?
    }

    /// Deletes a preset.
    pub async fn delete_preset(&self, id: PresetId) -> Result<(), String> {
        let (reply, result) = oneshot::channel();
        self.cmd_tx
            .send(RegistryCmd::DeletePreset { id, reply })
            .await
            .map_err(|_| "registry is stopped".to_owned())?;
        result
            .await
            .map_err(|_| "registry stopped before replying".to_owned())?
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
    animations: BTreeMap<AnimationId, Animation>,
    presets: Vec<Preset>,
    active: Option<ActivePlayback>,
    next_playback_id: u64,
}

struct ActivePlayback {
    playback_id: u64,
    cancel: CancellationToken,
    cancel_reason: Arc<AtomicU8>,
    leased: HashSet<LightId>,
    status: PlaybackStatus,
    done: oneshot::Receiver<()>,
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
                if self.active_leases(&id) {
                    self.cancel_active(PlaybackFinishReason::Cancelled, true)
                        .await;
                }
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
                    let _ = store_updates.send(StoreUpdate::Label { id, label }).await;
                }
                let _ = reply.send(Ok(self.lights[index].snapshot.clone()));
            }
            RegistryCmd::SetMode {
                selector,
                mode,
                wait,
                reply,
            } => self.set_mode(selector, mode, wait, reply).await,
            RegistryCmd::ListAnimations { reply } => {
                let summaries = self.animations.values().map(Animation::summary).collect();
                let _ = reply.send(summaries);
            }
            RegistryCmd::GetAnimation { id, reply } => {
                let _ = reply.send(self.animations.get(&id).cloned());
            }
            RegistryCmd::Play {
                id,
                options,
                binding,
                reply,
            } => {
                let result = self.play(id, options, binding).await;
                let _ = reply.send(result);
            }
            RegistryCmd::StopPlayback { reply } => {
                let stopped = if self.active.is_some() {
                    self.cancel_active(PlaybackFinishReason::Cancelled, true)
                        .await;
                    true
                } else {
                    false
                };
                let _ = reply.send(stopped);
            }
            RegistryCmd::ListPresets { reply } => {
                let _ = reply.send(self.presets.clone());
            }
            RegistryCmd::SavePreset {
                name,
                selector,
                reply,
            } => {
                let result = self.save_preset(name, selector).await;
                let _ = reply.send(result);
            }
            RegistryCmd::RecallPreset { id, wait, reply } => {
                self.recall_preset(id, wait, reply).await;
            }
            RegistryCmd::RenamePreset { id, name, reply } => {
                let result = self.rename_preset(&id, name).await;
                let _ = reply.send(result);
            }
            RegistryCmd::DeletePreset { id, reply } => {
                let result = self.delete_preset(&id).await;
                let _ = reply.send(result);
            }
            RegistryCmd::Shutdown => {}
        }
    }

    async fn save_preset(&mut self, name: String, selector: Selector) -> Result<Preset, String> {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err("preset name may not be empty".to_owned());
        }
        let entries = self
            .target_indices(&selector)
            .into_iter()
            .filter_map(|index| {
                let light = &self.lights[index].snapshot;
                (light.conn == ConnState::Connected)
                    .then(|| {
                        light.confirmed.or(light.desired).map(|mode| PresetEntry {
                            target: PresetTarget::Light {
                                id: light.id.clone(),
                            },
                            mode,
                        })
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return Err("no connected light modes could be captured".to_owned());
        }

        let base = preset_slug(&name);
        let mut candidate = base.clone();
        let mut suffix = 2;
        while self
            .presets
            .iter()
            .any(|preset| preset.id.as_str() == candidate)
        {
            candidate = format!("{base}-{suffix}");
            suffix += 1;
        }
        let preset = Preset {
            id: PresetId::parse(&candidate)?,
            name,
            entries,
        };
        self.presets.push(preset.clone());
        self.persist_presets().await;
        Ok(preset)
    }

    async fn recall_preset(
        &mut self,
        id: PresetId,
        wait: bool,
        reply: oneshot::Sender<Result<Vec<PerLightResult>, String>>,
    ) {
        let Some(preset) = self.presets.iter().find(|preset| preset.id == id).cloned() else {
            let _ = reply.send(Err(format!("preset {id} was not found")));
            return;
        };
        let mut assignments = BTreeMap::new();
        for entry in preset.entries {
            match entry.target {
                PresetTarget::Everything => {
                    for (index, light) in self.lights.iter().enumerate() {
                        if light.snapshot.conn == ConnState::Connected {
                            assignments.insert(index, entry.mode);
                        }
                    }
                }
                PresetTarget::Light { id } => {
                    if let Some(index) = self.index_of(&id) {
                        assignments.insert(index, entry.mode);
                    }
                }
            }
        }
        self.apply_modes(assignments.into_iter().collect(), wait, move |results| {
            let _ = reply.send(Ok(results));
        })
        .await;
    }

    async fn rename_preset(&mut self, id: &PresetId, name: String) -> Result<(), String> {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err("preset name may not be empty".to_owned());
        }
        let preset = self
            .presets
            .iter_mut()
            .find(|preset| &preset.id == id)
            .ok_or_else(|| format!("preset {id} was not found"))?;
        preset.name = name;
        self.persist_presets().await;
        Ok(())
    }

    async fn delete_preset(&mut self, id: &PresetId) -> Result<(), String> {
        let Some(index) = self.presets.iter().position(|preset| &preset.id == id) else {
            return Err(format!("preset {id} was not found"));
        };
        self.presets.remove(index);
        self.persist_presets().await;
        Ok(())
    }

    async fn persist_presets(&self) {
        if let Some(store_updates) = &self.config.store_updates {
            let _ = store_updates
                .send(StoreUpdate::Presets(self.presets.clone()))
                .await;
        }
    }

    fn start_scan(&self, duration: Duration) {
        let transport = Arc::clone(&self.transport);
        let input_tx = self.input_tx.clone();
        let cancel = self.cancel.clone();
        self.tracker.spawn(async move {
            // Everything else on the air (headphones, watches, beacons) is noise.
            let filter = ScanFilter {
                name_prefix: Some("NEEWER".to_owned()),
            };
            let Ok(mut scan) = transport.scan(filter).await else {
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
        let targets = self
            .target_indices(&selector)
            .into_iter()
            .map(|index| (index, mode))
            .collect();
        self.apply_modes(targets, wait, move |results| {
            let _ = reply.send(results);
        })
        .await;
    }

    async fn apply_modes<F>(&mut self, targets: Vec<(usize, Mode)>, wait: bool, finish: F)
    where
        F: FnOnce(Vec<PerLightResult>) + Send + 'static,
    {
        if targets
            .iter()
            .any(|(index, _)| self.active_leases(&self.lights[*index].snapshot.id))
        {
            self.cancel_active(PlaybackFinishReason::Cancelled, true)
                .await;
        }
        for &(index, mode) in &targets {
            self.lights[index].snapshot.desired = Some(mode);
            self.emit(index);
        }

        if !wait {
            for (index, mode) in targets {
                self.lights[index].desired_tx.send_replace(Some(mode));
            }
            finish(Vec::new());
            return;
        }

        let mut pending = Vec::with_capacity(targets.len());
        for (index, mode) in targets {
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
            finish(results);
        });
    }

    async fn play(
        &mut self,
        id: AnimationId,
        options: PlaybackOptions,
        binding: TargetBinding,
    ) -> Result<PlaybackStatus, String> {
        options.validate()?;
        let animation = self
            .animations
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("animation {id} was not found"))?;
        let target_indices = self.resolve_animation_targets(&animation, &binding)?;

        if self.active.is_some() {
            self.cancel_active(PlaybackFinishReason::Chained, false)
                .await;
        }

        let mut targets: BTreeMap<AnimTarget, Vec<watch::Sender<Option<Mode>>>> = BTreeMap::new();
        let mut leased = HashSet::new();
        for (target, indices) in target_indices {
            let senders = indices
                .into_iter()
                .map(|index| {
                    let light = &self.lights[index];
                    leased.insert(light.snapshot.id.clone());
                    light.desired_tx.clone()
                })
                .collect();
            targets.insert(target, senders);
        }
        let revert = self
            .lights
            .iter()
            .filter(|light| leased.contains(&light.snapshot.id))
            .filter_map(|light| {
                light
                    .snapshot
                    .confirmed
                    .or(light.snapshot.desired)
                    .map(|mode| (light.desired_tx.clone(), mode))
            })
            .collect::<Vec<_>>();
        let looping = options.loop_override.unwrap_or(animation.loop_default);
        let status = PlaybackStatus {
            animation: animation.id.clone(),
            name: animation.name.clone(),
            started_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            looping,
        };
        let playback_id = self.next_playback_id;
        self.next_playback_id = self.next_playback_id.wrapping_add(1);
        let cancel = self.cancel.child_token();
        let cancel_reason = Arc::new(AtomicU8::new(0));
        let (done_tx, done) = oneshot::channel();
        let input_tx = self.input_tx.clone();
        let task_cancel = cancel.clone();
        let task_reason = Arc::clone(&cancel_reason);
        self.tracker.spawn(async move {
            let start = Instant::now();
            let mut reason = PlaybackFinishReason::Natural;
            let duration = playback_duration(&animation, &options);
            for frame in schedule(&animation, &options) {
                let deadline = start + Duration::from_millis(frame.at_ms);
                let cancelled = tokio::select! {
                    biased;
                    _ = task_cancel.cancelled() => true,
                    _ = tokio::time::sleep_until(deadline) => false,
                };
                if cancelled {
                    reason = decode_cancel_reason(task_reason.load(Ordering::Acquire));
                    break;
                }
                for (target, mode) in frame.ops {
                    if let Some(senders) = targets.get(&target) {
                        for sender in senders {
                            sender.send_replace(Some(mode));
                        }
                    }
                }
            }
            if matches!(reason, PlaybackFinishReason::Natural)
                && let Some(duration) = duration
            {
                let cancelled = tokio::select! {
                    biased;
                    _ = task_cancel.cancelled() => true,
                    _ = tokio::time::sleep_until(start + Duration::from_millis(duration)) => false,
                };
                if cancelled {
                    reason = decode_cancel_reason(task_reason.load(Ordering::Acquire));
                }
            }
            if options.revert_on_finish && !matches!(reason, PlaybackFinishReason::Chained) {
                for (sender, mode) in revert {
                    sender.send_replace(Some(mode));
                }
            }
            let _ = done_tx.send(());
            let _ = input_tx
                .send(RegistryInput::PlaybackFinished {
                    playback_id,
                    reason,
                })
                .await;
        });
        self.active = Some(ActivePlayback {
            playback_id,
            cancel,
            cancel_reason,
            leased,
            status: status.clone(),
            done,
        });
        self.emit_event(Event::Playback {
            playback: Some(status.clone()),
        });
        Ok(status)
    }

    fn resolve_animation_targets(
        &self,
        animation: &Animation,
        binding: &TargetBinding,
    ) -> Result<BTreeMap<AnimTarget, Vec<usize>>, String> {
        let connected_all = self
            .target_indices(&binding.all)
            .into_iter()
            .filter(|index| self.lights[*index].snapshot.conn == ConnState::Connected)
            .collect::<Vec<_>>();
        let referenced = animation
            .keyframes
            .iter()
            .flat_map(|keyframe| keyframe.lights.keys().copied())
            .collect::<BTreeSet<_>>();
        if referenced.is_empty() {
            return Err("animation has no resolvable light targets".to_owned());
        }
        referenced
            .into_iter()
            .map(|target| {
                let indices = match target {
                    AnimTarget::All => connected_all.clone(),
                    AnimTarget::Slot(slot) => {
                        let slot_index = usize::from(slot.get() - 1);
                        if let Some(id) = binding.slots.get(slot_index) {
                            let index = self
                                .index_of(id)
                                .filter(|index| {
                                    self.lights[*index].snapshot.conn == ConnState::Connected
                                })
                                .ok_or_else(|| {
                                    format!("slot {} light {id} is not connected", slot.get())
                                })?;
                            vec![index]
                        } else if connected_all.is_empty() {
                            return Err(format!(
                                "slot {} cannot resolve because no lights are connected",
                                slot.get()
                            ));
                        } else {
                            vec![connected_all[slot_index % connected_all.len()]]
                        }
                    }
                };
                if indices.is_empty() {
                    Err(format!("target {target:?} resolved to no connected lights"))
                } else {
                    Ok((target, indices))
                }
            })
            .collect()
    }

    fn active_leases(&self, id: &LightId) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.leased.contains(id))
    }

    async fn cancel_active(&mut self, reason: PlaybackFinishReason, emit: bool) {
        let Some(active) = self.active.take() else {
            return;
        };
        active
            .cancel_reason
            .store(encode_cancel_reason(reason), Ordering::Release);
        active.cancel.cancel();
        let _ = active.done.await;
        if emit {
            self.emit_event(Event::Playback { playback: None });
        }
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
            RegistryInput::PlaybackFinished {
                playback_id,
                reason,
            } => {
                if self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.playback_id == playback_id)
                    && matches!(
                        reason,
                        PlaybackFinishReason::Natural | PlaybackFinishReason::Cancelled
                    )
                {
                    self.active = None;
                    self.emit_event(Event::Playback { playback: None });
                }
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
        self.emit_event(Event::Light {
            light: self.lights[index].snapshot.clone(),
        });
    }

    fn emit_event(&mut self, event: Event) {
        self.seq += 1;
        let event = SeqEvent {
            seq: self.seq,
            event,
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
            playback: self.active.as_ref().map(|active| active.status.clone()),
        });
        self.ring_tx.send_replace(Arc::new(self.ring.clone()));
        self.world_tx.send_replace(world);
        let _ = self.events_tx.send(event);
    }
}

fn encode_cancel_reason(reason: PlaybackFinishReason) -> u8 {
    match reason {
        PlaybackFinishReason::Natural => 0,
        PlaybackFinishReason::Cancelled => 1,
        PlaybackFinishReason::Chained => 2,
    }
}

fn decode_cancel_reason(reason: u8) -> PlaybackFinishReason {
    match reason {
        2 => PlaybackFinishReason::Chained,
        _ => PlaybackFinishReason::Cancelled,
    }
}

fn preset_slug(name: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    if slug.is_empty() {
        "preset".to_owned()
    } else {
        slug
    }
}

fn load_animations(directory: &Path) -> BTreeMap<AnimationId, Animation> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(error) => {
            warn!(path = %directory.display(), %error, "could not read animations directory");
            return BTreeMap::new();
        }
    };
    let mut paths = entries
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry.path()),
            Err(error) => {
                warn!(path = %directory.display(), %error, "could not read animation directory entry");
                None
            }
        })
        .filter(|path| path.extension().is_some_and(|extension| extension == "json"))
        .collect::<Vec<_>>();
    paths.sort();

    let mut animations = BTreeMap::new();
    for path in paths {
        let result = fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|encoded| {
                serde_json::from_str::<Animation>(&encoded).map_err(|error| error.to_string())
            })
            .and_then(|animation| {
                animation.validate()?;
                let filename = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("");
                if filename != animation.id.as_str() {
                    return Err(format!(
                        "filename id {filename:?} does not match animation id {}",
                        animation.id
                    ));
                }
                Ok(animation)
            });
        match result {
            Ok(animation) => {
                if animations.contains_key(&animation.id) {
                    warn!(path = %path.display(), "skipping duplicate animation id");
                } else {
                    animations.insert(animation.id.clone(), animation);
                }
            }
            Err(error) => {
                warn!(path = %path.display(), %error, "skipping invalid animation");
            }
        }
    }
    animations
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
