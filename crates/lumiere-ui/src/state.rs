use std::collections::HashSet;

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use lumiere_proto::{LightId, WorldSnapshot};

use crate::platform;

/// Current connection state of the live event stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnStatus {
    Connecting,
    Live,
    Reconnecting { attempt: u32 },
}

/// Shared reactive state for the control panel.
#[derive(Clone, Copy)]
pub struct AppState {
    pub world: Signal<WorldSnapshot>,
    pub conn: Signal<ConnStatus>,
    pub token: Signal<Option<String>>,
    pub selection: Signal<HashSet<LightId>>,
    pub error: Signal<Option<String>>,
}

impl AppState {
    pub fn new(token: Option<String>) -> Self {
        Self {
            world: Signal::new(WorldSnapshot {
                seq: 0,
                lights: Vec::new(),
                playback: None,
            }),
            conn: Signal::new(ConnStatus::Connecting),
            token: Signal::new(token),
            selection: Signal::new(HashSet::new()),
            error: Signal::new(None),
        }
    }

    pub fn logout(mut self) {
        platform::clear_stored_token();
        self.token.set(None);
        self.selection.write().clear();
    }

    pub fn report_error(mut self, message: impl Into<String>) {
        let message = message.into();
        self.error.set(Some(message.clone()));
        let mut error = self.error;
        spawn(async move {
            TimeoutFuture::new(5_000).await;
            if error.peek().as_ref() == Some(&message) {
                error.set(None);
            }
        });
    }
}
