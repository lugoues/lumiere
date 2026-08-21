use dioxus::prelude::*;
use lumiere_proto::{CommandRequest, LightId, Mode, Selector};

use crate::{
    api::{ApiClient, ApiError},
    state::AppState,
};

/// Sends a live command to exactly one light.
pub fn send_mode(state: AppState, id: LightId, mode: Mode, wake: bool) {
    spawn(async move {
        let request = CommandRequest {
            selector: Selector::Ids { ids: vec![id] },
            mode,
            wake,
            wait: false,
        };
        match ApiClient::new(state.token).post_command(request).await {
            Ok(_) => {}
            Err(ApiError::Auth(_)) => state.logout(),
            Err(error) => state.report_error(error.to_string()),
        }
    });
}
