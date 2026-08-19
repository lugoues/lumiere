use dioxus::prelude::*;
use futures::{SinkExt, StreamExt};
use gloo_net::websocket::{Message, futures::WebSocket};
use gloo_timers::future::TimeoutFuture;
use lumiere_proto::{ClientMsg, Event, ServerMsg, WS_PROTOCOL_VERSION, WorldSnapshot};

use crate::{
    api::{ApiClient, ApiError},
    platform,
    state::{AppState, ConnStatus},
};

/// Runs the authenticated event stream until the component is unmounted.
pub async fn run(mut state: AppState) {
    let api = ApiClient::new(state.token);
    match api.get_lights().await {
        Ok(snapshot) => state.world.set(snapshot),
        Err(ApiError::Auth(_)) => {
            state.logout();
            return;
        }
        Err(error) => state.report_error(error.to_string()),
    }

    let mut last_seq = state.world.peek().seq;
    let mut attempt = 0;
    loop {
        if state.token.peek().is_none() {
            return;
        }
        state.conn.set(if attempt == 0 {
            ConnStatus::Connecting
        } else {
            ConnStatus::Reconnecting { attempt }
        });

        let ticket = match api.ws_ticket().await {
            Ok(ticket) => ticket,
            Err(ApiError::Auth(_)) => {
                state.logout();
                return;
            }
            Err(error) => {
                state.report_error(error.to_string());
                wait_to_reconnect(&mut state, &mut attempt).await;
                continue;
            }
        };

        let url = platform::ws_url(&platform::current_origin(), "/api/v1/events");
        match WebSocket::open(&url) {
            Ok(mut socket) => {
                let hello = ClientMsg::Hello {
                    protocol_version: WS_PROTOCOL_VERSION,
                    ticket,
                    last_seq: Some(last_seq),
                };
                let encoded = serde_json::to_string(&hello).expect("client hello serializes");
                if socket.send(Message::Text(encoded)).await.is_ok() {
                    while let Some(message) = socket.next().await {
                        let text = match message {
                            Ok(Message::Text(text)) => text,
                            Ok(Message::Bytes(_)) => continue,
                            Err(_) => break,
                        };
                        let Ok(message) = serde_json::from_str::<ServerMsg>(&text) else {
                            state.report_error("the daemon sent an invalid event");
                            continue;
                        };
                        if matches!(message, ServerMsg::Welcome { .. }) {
                            attempt = 0;
                            state.conn.set(ConnStatus::Live);
                        }
                        if let ServerMsg::Error { message } = &message {
                            state.report_error(message.clone());
                            break;
                        }
                        let mut world = state.world.peek().clone();
                        apply_server_message(&mut world, &message);
                        last_seq = world.seq;
                        state.world.set(world);
                    }
                }
            }
            Err(error) => state.report_error(format!("WebSocket failed: {error}")),
        }

        wait_to_reconnect(&mut state, &mut attempt).await;
    }
}

async fn wait_to_reconnect(state: &mut AppState, attempt: &mut u32) {
    *attempt = attempt.saturating_add(1);
    state
        .conn
        .set(ConnStatus::Reconnecting { attempt: *attempt });
    let base = reconnect_backoff_ms(*attempt);
    let jitter = platform::reconnect_jitter_ms(*attempt, base / 4 + 1);
    TimeoutFuture::new(base.saturating_add(jitter).min(10_000)).await;
}

/// Returns the capped exponential delay before a reconnect attempt.
pub fn reconnect_backoff_ms(attempt: u32) -> u32 {
    let exponent = attempt.saturating_sub(1).min(31);
    250_u32.saturating_mul(1_u32 << exponent).min(10_000)
}

/// Folds a protocol message into the current world snapshot.
pub fn apply_server_message(world: &mut WorldSnapshot, message: &ServerMsg) {
    match message {
        ServerMsg::Welcome {
            snapshot: Some(snapshot),
            ..
        }
        | ServerMsg::Resync { snapshot, .. } => *world = snapshot.clone(),
        ServerMsg::Patch { events } => {
            for sequence_event in events {
                match &sequence_event.event {
                    Event::Light { light } => {
                        if let Some(current) = world
                            .lights
                            .iter_mut()
                            .find(|current| current.id == light.id)
                        {
                            *current = light.clone();
                        } else {
                            world.lights.push(light.clone());
                        }
                    }
                    Event::LightRemoved { id } => {
                        world.lights.retain(|light| light.id != *id);
                    }
                    Event::Playback { playback } => world.playback.clone_from(playback),
                }
                world.seq = sequence_event.seq;
            }
        }
        ServerMsg::Welcome { snapshot: None, .. }
        | ServerMsg::Pong { .. }
        | ServerMsg::Error { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use lumiere_proto::{
        Capabilities, ConnState, Kelvin, LightId, LightSnapshot, ResyncReason, SeqEvent,
    };

    use super::*;

    fn light(name: &str, label: &str) -> LightSnapshot {
        LightSnapshot {
            id: LightId::sim(name),
            model: "RGB660".into(),
            label: label.into(),
            caps: Capabilities {
                cct_min: Kelvin::new(2500).unwrap(),
                cct_max: Kelvin::new(10_000).unwrap(),
                rgb: true,
                scenes: true,
                cct_split_packets: false,
                reports_status: true,
            },
            conn: ConnState::Connected,
            rssi: Some(-42),
            desired: None,
            confirmed: None,
            power: Some(true),
            last_error: None,
        }
    }

    #[test]
    fn folds_welcome_patch_and_resync() {
        let mut world = WorldSnapshot {
            seq: 0,
            lights: Vec::new(),
            playback: None,
        };
        apply_server_message(
            &mut world,
            &ServerMsg::Welcome {
                protocol_version: WS_PROTOCOL_VERSION,
                server_version: "test".into(),
                session: "one".into(),
                seq: 1,
                snapshot: Some(WorldSnapshot {
                    seq: 1,
                    lights: vec![light("key", "Key")],
                    playback: None,
                }),
            },
        );
        assert_eq!(world.lights[0].label, "Key");

        apply_server_message(
            &mut world,
            &ServerMsg::Patch {
                events: vec![
                    SeqEvent {
                        seq: 2,
                        event: Event::Light {
                            light: light("key", "Key light"),
                        },
                    },
                    SeqEvent {
                        seq: 3,
                        event: Event::Light {
                            light: light("fill", "Fill"),
                        },
                    },
                    SeqEvent {
                        seq: 4,
                        event: Event::LightRemoved {
                            id: LightId::sim("key"),
                        },
                    },
                ],
            },
        );
        assert_eq!(world.seq, 4);
        assert_eq!(world.lights, vec![light("fill", "Fill")]);

        let replacement = WorldSnapshot {
            seq: 9,
            lights: vec![light("rim", "Rim")],
            playback: None,
        };
        apply_server_message(
            &mut world,
            &ServerMsg::Resync {
                snapshot: replacement.clone(),
                reason: ResyncReason::SessionChanged,
            },
        );
        assert_eq!(world, replacement);
    }

    #[test]
    fn reconnect_delays_are_exponential_and_capped() {
        assert_eq!(
            (1..=8).map(reconnect_backoff_ms).collect::<Vec<_>>(),
            vec![250, 500, 1_000, 2_000, 4_000, 8_000, 10_000, 10_000]
        );
    }
}
