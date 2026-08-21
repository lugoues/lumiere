use std::{fmt, sync::Arc, time::Duration};

use lumiere_core::wire::{clamp_to_device, encode, hsi_to_cct};
use lumiere_proto::{Capabilities, ConnState, LightId, Mode, PerLightResult, SkipReason};
use lumiere_transport::{Link, Transport, WriteKind};
use tokio::sync::{Semaphore, mpsc, oneshot, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::registry::RegistryInput;

/// A discrete operation sent to one light actor.
pub enum LightOp {
    Connect {
        reply: oneshot::Sender<Result<(), String>>,
    },
    Disconnect {
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Writes this mode immediately and replies after the write finishes.
    ApplyNow {
        mode: Mode,
        reply: oneshot::Sender<PerLightResult>,
    },
}

pub(crate) struct LightActor {
    id: LightId,
    caps: Capabilities,
    transport: Arc<dyn Transport>,
    registry_tx: mpsc::Sender<RegistryInput>,
    desired_rx: watch::Receiver<Option<Mode>>,
    ops_rx: mpsc::Receiver<LightOp>,
    write_permits: Arc<Semaphore>,
    min_interval: Duration,
    connect_timeout: Duration,
    cancel: CancellationToken,
    link: Option<Box<dyn Link>>,
    next_write_at: Instant,
    desired_open: bool,
    ops_open: bool,
    retry: Option<Retry>,
    /// The last requested mode that reached the light (or was knowingly skipped).
    /// Used to drop duplicate desired-watch echoes; cleared on every (re)connect
    /// so a fresh link always gets a full rewrite.
    last_written: Option<Mode>,
    /// True only after this link has carried a power-on (alone or prepended).
    /// Mode writes do not wake a Neewer light, and the light may be off for
    /// reasons this daemon never saw (a previous session, the physical switch),
    /// so the first lit write on every link prepends a power-on.
    power_known_on: bool,
}

#[derive(Clone, Copy)]
struct Retry {
    attempt: u32,
    at: Instant,
}

enum Next {
    Shutdown,
    Op(Option<LightOp>),
    Desired(Result<(), watch::error::RecvError>),
    Closed,
    Retry,
}

pub(crate) struct LightActorArgs {
    pub id: LightId,
    pub caps: Capabilities,
    pub transport: Arc<dyn Transport>,
    pub registry_tx: mpsc::Sender<RegistryInput>,
    pub desired_rx: watch::Receiver<Option<Mode>>,
    pub ops_rx: mpsc::Receiver<LightOp>,
    pub write_permits: Arc<Semaphore>,
    pub min_interval: Duration,
    pub connect_timeout: Duration,
    pub cancel: CancellationToken,
}

impl LightActor {
    pub(crate) fn new(args: LightActorArgs) -> Self {
        Self {
            id: args.id,
            caps: args.caps,
            transport: args.transport,
            registry_tx: args.registry_tx,
            desired_rx: args.desired_rx,
            ops_rx: args.ops_rx,
            write_permits: args.write_permits,
            min_interval: args.min_interval,
            connect_timeout: args.connect_timeout,
            cancel: args.cancel,
            link: None,
            next_write_at: Instant::now(),
            desired_open: true,
            ops_open: true,
            retry: None,
            last_written: None,
            power_known_on: false,
        }
    }

    pub(crate) async fn run(mut self) {
        loop {
            let next = self.next().await;
            match next {
                Next::Shutdown => break,
                Next::Op(Some(op)) => self.handle_op(op).await,
                Next::Op(None) => self.ops_open = false,
                Next::Desired(Ok(())) => {
                    let mode = *self.desired_rx.borrow_and_update();
                    if let Some(mode) = mode
                        && self.last_written != Some(mode)
                    {
                        let result = self.apply_desired(mode).await;
                        self.report_result(result).await;
                    }
                }
                Next::Desired(Err(_)) => self.desired_open = false,
                Next::Closed => self.link_closed().await,
                Next::Retry => self.retry_connect().await,
            }

            if !self.ops_open && !self.desired_open && self.link.is_none() && self.retry.is_none() {
                break;
            }
        }

        self.retry = None;
        if let Some(link) = self.link.take() {
            let _ = link.disconnect().await;
        }
    }

    async fn next(&mut self) -> Next {
        if let Some(link) = self.link.as_ref() {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => Next::Shutdown,
                op = self.ops_rx.recv(), if self.ops_open => Next::Op(op),
                changed = self.desired_rx.changed(), if self.desired_open => Next::Desired(changed),
                _ = link.closed() => Next::Closed,
            }
        } else if let Some(retry) = self.retry {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => Next::Shutdown,
                op = self.ops_rx.recv(), if self.ops_open => Next::Op(op),
                changed = self.desired_rx.changed(), if self.desired_open => Next::Desired(changed),
                _ = tokio::time::sleep_until(retry.at) => Next::Retry,
            }
        } else {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => Next::Shutdown,
                op = self.ops_rx.recv(), if self.ops_open => Next::Op(op),
                changed = self.desired_rx.changed(), if self.desired_open => Next::Desired(changed),
            }
        }
    }

    async fn handle_op(&mut self, op: LightOp) {
        match op {
            LightOp::Connect { reply } => {
                self.retry = None;
                let result = self
                    .connect_once(ConnState::Connecting { attempt: 1 })
                    .await;
                let _ = reply.send(result);
            }
            LightOp::Disconnect { reply } => {
                self.retry = None;
                let result = if let Some(link) = self.link.take() {
                    link.disconnect().await.map_err(|error| error.to_string())
                } else {
                    Ok(())
                };
                self.report_connection(ConnState::Discovered, result.as_ref().err().cloned())
                    .await;
                let _ = reply.send(result);
            }
            LightOp::ApplyNow { mode, reply } => {
                let result = self.apply(mode).await;
                self.report_result(result.clone()).await;
                let _ = reply.send(result);
            }
        }
    }

    async fn connect_once(&mut self, state: ConnState) -> Result<(), String> {
        if self.link.as_ref().is_some_and(|link| link.is_connected()) {
            self.report_connection(ConnState::Connected, None).await;
            return Ok(());
        }
        self.link = None;
        tracing::info!(light_id = %self.id, ?state, "connecting to light");
        self.report_connection(state, None).await;
        match self.transport.connect(&self.id, self.connect_timeout).await {
            Ok(link) => {
                self.link = Some(link);
                self.last_written = None;
                self.power_known_on = false;
                tracing::info!(light_id = %self.id, "connected to light");
                self.report_connection(ConnState::Connected, None).await;
                Ok(())
            }
            Err(error) => {
                // A transient failure here (adapter settling, light mid-boot) must
                // not strand the light: arm the same backoff the closed-link path uses.
                let error = error.to_string();
                let attempt = 1;
                self.retry = Some(Retry {
                    attempt,
                    at: Instant::now() + retry_delay(attempt),
                });
                tracing::warn!(light_id = %self.id, %error, attempt, "light connection failed; retrying");
                self.report_connection(ConnState::Reconnecting { attempt }, Some(error.clone()))
                    .await;
                Err(error)
            }
        }
    }

    async fn link_closed(&mut self) {
        self.link = None;
        let attempt = 1;
        self.retry = Some(Retry {
            attempt,
            at: Instant::now() + retry_delay(attempt),
        });
        tracing::warn!(light_id = %self.id, attempt, "light connection closed; retrying");
        self.report_connection(ConnState::Reconnecting { attempt }, None)
            .await;
    }

    async fn retry_connect(&mut self) {
        let Some(retry) = self.retry.take() else {
            return;
        };
        tracing::info!(light_id = %self.id, attempt = retry.attempt, "retrying light connection");
        match self.transport.connect(&self.id, self.connect_timeout).await {
            Ok(link) => {
                self.link = Some(link);
                self.last_written = None;
                self.power_known_on = false;
                tracing::info!(light_id = %self.id, attempt = retry.attempt, "reconnected to light");
                self.report_connection(ConnState::Connected, None).await;
                self.apply_current_desired().await;
            }
            Err(error) if retry.attempt < 5 => {
                let attempt = retry.attempt + 1;
                self.retry = Some(Retry {
                    attempt,
                    at: Instant::now() + retry_delay(attempt),
                });
                tracing::warn!(light_id = %self.id, %error, attempt, "light reconnect failed; retrying");
                self.report_connection(
                    ConnState::Reconnecting { attempt },
                    Some(error.to_string()),
                )
                .await;
            }
            Err(error) => {
                tracing::warn!(light_id = %self.id, %error, attempt = retry.attempt, "light reconnect failed; giving up");
                self.report_connection(ConnState::Lost, Some(error.to_string()))
                    .await;
            }
        }
    }

    async fn apply(&mut self, requested: Mode) -> PerLightResult {
        if !self.link.as_ref().is_some_and(|link| link.is_connected()) {
            tracing::debug!(light_id = %self.id, ?requested, reason = ?SkipReason::NotConnected, "skipping light mode");
            return PerLightResult::Skipped {
                id: self.id.clone(),
                reason: SkipReason::NotConnected,
            };
        }
        if matches!(requested, Mode::Scene { .. }) && !self.caps.scenes {
            self.last_written = Some(requested);
            tracing::debug!(light_id = %self.id, ?requested, reason = ?SkipReason::UnsupportedMode, "skipping light mode");
            return PerLightResult::Skipped {
                id: self.id.clone(),
                reason: SkipReason::UnsupportedMode,
            };
        }

        // A color command to a bi-color light approximates the hue as a
        // temperature, matching the reference's default convert fallback.
        // Without this, most of the animation library is silent on CCT rigs.
        let (requested_for_device, converted) = match requested {
            Mode::Hsi { hue, bri, .. } if !self.caps.rgb => {
                (hsi_to_cct(hue, bri, &self.caps), true)
            }
            other => (other, false),
        };
        let (applied, clamped) = clamp_to_device(requested_for_device, &self.caps);
        let adapted = converted || clamped;
        tracing::debug!(light_id = %self.id, ?requested, ?applied, adapted, "resolved light mode");
        if !self.power_known_on && !matches!(applied, Mode::Off) {
            tracing::debug!(light_id = %self.id, ?applied, "sending wake packet because power state is not known on");
            for packet in encode(Mode::On, &self.caps) {
                if let Err(error) = self.write_packet(packet.as_bytes()).await {
                    self.last_written = None;
                    return PerLightResult::Failed {
                        id: self.id.clone(),
                        error,
                    };
                }
            }
            self.power_known_on = true;
        }
        for packet in encode(applied, &self.caps) {
            if let Err(error) = self.write_packet(packet.as_bytes()).await {
                self.last_written = None;
                return PerLightResult::Failed {
                    id: self.id.clone(),
                    error,
                };
            }
        }

        self.last_written = Some(requested);
        self.power_known_on = !matches!(applied, Mode::Off);
        if adapted {
            PerLightResult::Adapted {
                id: self.id.clone(),
                requested,
                applied,
            }
        } else {
            PerLightResult::Applied {
                id: self.id.clone(),
                mode: applied,
            }
        }
    }

    async fn apply_desired(&mut self, mut requested: Mode) -> PerLightResult {
        if Instant::now() < self.next_write_at {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => {
                    return PerLightResult::Failed {
                        id: self.id.clone(),
                        error: "daemon is shutting down".into(),
                    };
                }
                _ = tokio::time::sleep_until(self.next_write_at) => {}
            }
            if self.desired_rx.has_changed().unwrap_or(false)
                && let Some(newest) = *self.desired_rx.borrow_and_update()
            {
                requested = newest;
            }
        }
        self.apply(requested).await
    }

    async fn apply_current_desired(&mut self) {
        let mode = *self.desired_rx.borrow();
        if let Some(mode) = mode {
            let result = self.apply_desired(mode).await;
            self.report_result(result).await;
        }
    }

    async fn write_packet(&mut self, packet: &[u8]) -> Result<(), String> {
        if Instant::now() < self.next_write_at {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Err("daemon is shutting down".into()),
                _ = tokio::time::sleep_until(self.next_write_at) => {}
            }
        }
        if self.cancel.is_cancelled() {
            return Err("daemon is shutting down".into());
        }

        let permit = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => return Err("daemon is shutting down".into()),
            permit = self.write_permits.acquire() => permit.map_err(|_| "write limiter closed".to_owned())?,
        };
        tracing::trace!(light_id = %self.id, packet = %Hex(packet), "writing light packet");
        let result = match self.link.as_ref() {
            Some(link) => link
                .write(packet, WriteKind::WithoutResponse)
                .await
                .map_err(|error| error.to_string()),
            None => Err(format!("light {} is disconnected", self.id)),
        };
        drop(permit);
        self.next_write_at = Instant::now() + self.min_interval;
        if let Err(error) = &result {
            tracing::warn!(light_id = %self.id, %error, packet = %Hex(packet), "light packet write failed");
        }
        result
    }

    async fn report_connection(&self, conn: ConnState, error: Option<String>) {
        let (ack_tx, ack_rx) = oneshot::channel();
        if self
            .registry_tx
            .send(RegistryInput::Connection {
                id: self.id.clone(),
                conn,
                error,
                ack: ack_tx,
            })
            .await
            .is_ok()
        {
            let _ = ack_rx.await;
        }
    }

    async fn report_result(&self, result: PerLightResult) {
        let (ack_tx, ack_rx) = oneshot::channel();
        if self
            .registry_tx
            .send(RegistryInput::WriteOutcome {
                result,
                ack: ack_tx,
            })
            .await
            .is_ok()
        {
            let _ = ack_rx.await;
        }
    }
}

struct Hex<'a>(&'a [u8]);

impl fmt::Display for Hex<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

fn retry_delay(attempt: u32) -> Duration {
    let factor = 1_u32 << attempt.saturating_sub(1).min(7);
    Duration::from_millis(250 * u64::from(factor)).min(Duration::from_secs(30))
}
