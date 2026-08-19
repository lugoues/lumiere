use dioxus::prelude::*;
use gloo_net::http::{Request, Response};
use lumiere_proto::{
    AnimationId, AnimationSummary, CommandRequest, CommandResponse, LightId, LightSnapshot,
    PlaybackOptions, PlaybackStatus, Preset, PresetId, Selector, TargetBinding, WorldSnapshot,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fmt;

use crate::platform;

/// An API response indicating that the saved token is not valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthError;

impl fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("authentication required")
    }
}

/// A typed REST client failure.
#[derive(Debug)]
pub enum ApiError {
    Auth(AuthError),
    Request(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auth(error) => error.fmt(formatter),
            Self::Request(message) => formatter.write_str(message),
        }
    }
}

/// Thin client for the daemon's authenticated REST endpoints.
#[derive(Clone, Copy)]
pub struct ApiClient {
    token: Signal<Option<String>>,
}

impl ApiClient {
    pub fn new(token: Signal<Option<String>>) -> Self {
        Self { token }
    }

    pub async fn get_lights(self) -> Result<WorldSnapshot, ApiError> {
        let response = self
            .request("/api/v1/lights", Request::get)?
            .send()
            .await
            .map_err(network)?;
        decode(response).await
    }

    pub async fn set_label(self, id: &LightId, label: String) -> Result<LightSnapshot, ApiError> {
        #[derive(Serialize)]
        struct LabelRequest {
            label: String,
        }

        let path = format!("/api/v1/lights/{id}");
        let request = self
            .request(&path, Request::patch)?
            .json(&LabelRequest { label })
            .map_err(network)?;
        decode(request.send().await.map_err(network)?).await
    }

    pub async fn post_scan(self) -> Result<(), ApiError> {
        #[derive(Serialize)]
        struct ScanRequest {
            duration_ms: u64,
        }

        let request = self
            .request("/api/v1/scan", Request::post)?
            .json(&ScanRequest {
                duration_ms: 10_000,
            })
            .map_err(network)?;
        ensure_success(request.send().await.map_err(network)?).await
    }

    pub async fn post_command(self, command: CommandRequest) -> Result<CommandResponse, ApiError> {
        let request = self
            .request("/api/v1/command", Request::post)?
            .json(&command)
            .map_err(network)?;
        decode(request.send().await.map_err(network)?).await
    }

    pub async fn get_animations(self) -> Result<Vec<AnimationSummary>, ApiError> {
        let response = self
            .request("/api/v1/animations", Request::get)?
            .send()
            .await
            .map_err(network)?;
        decode(response).await
    }

    pub async fn play_animation(
        self,
        id: &AnimationId,
        options: PlaybackOptions,
        binding: TargetBinding,
    ) -> Result<PlaybackStatus, ApiError> {
        #[derive(Serialize)]
        struct PlayRequest {
            options: PlaybackOptions,
            binding: TargetBinding,
        }

        let path = format!("/api/v1/animations/{id}/play");
        let request = self
            .request(&path, Request::post)?
            .json(&PlayRequest { options, binding })
            .map_err(network)?;
        decode(request.send().await.map_err(network)?).await
    }

    pub async fn stop_playback(self) -> Result<(), ApiError> {
        ensure_success(
            self.request("/api/v1/playback/stop", Request::post)?
                .send()
                .await
                .map_err(network)?,
        )
        .await
    }

    pub async fn get_presets(self) -> Result<Vec<Preset>, ApiError> {
        let response = self
            .request("/api/v1/presets", Request::get)?
            .send()
            .await
            .map_err(network)?;
        decode(response).await
    }

    pub async fn capture_preset(
        self,
        name: String,
        selector: Selector,
    ) -> Result<Preset, ApiError> {
        #[derive(Serialize)]
        struct CaptureRequest {
            name: String,
            selector: Selector,
        }

        let request = self
            .request("/api/v1/presets", Request::post)?
            .json(&CaptureRequest { name, selector })
            .map_err(network)?;
        decode(request.send().await.map_err(network)?).await
    }

    pub async fn recall_preset(self, id: &PresetId) -> Result<CommandResponse, ApiError> {
        #[derive(Serialize)]
        struct RecallRequest {
            wait: bool,
        }

        let path = format!("/api/v1/presets/{id}/recall");
        let request = self
            .request(&path, Request::post)?
            .json(&RecallRequest { wait: false })
            .map_err(network)?;
        decode(request.send().await.map_err(network)?).await
    }

    pub async fn recapture_preset(
        self,
        id: &PresetId,
        selector: Option<Selector>,
    ) -> Result<Preset, ApiError> {
        #[derive(Serialize)]
        struct RecaptureRequest {
            #[serde(skip_serializing_if = "Option::is_none")]
            selector: Option<Selector>,
        }

        let path = format!("/api/v1/presets/{id}/capture");
        let request = self
            .request(&path, Request::post)?
            .json(&RecaptureRequest { selector })
            .map_err(network)?;
        decode(request.send().await.map_err(network)?).await
    }

    pub async fn rename_preset(self, id: &PresetId, name: String) -> Result<Preset, ApiError> {
        #[derive(Serialize)]
        struct RenameRequest {
            name: String,
        }

        let path = format!("/api/v1/presets/{id}");
        let request = self
            .request(&path, Request::patch)?
            .json(&RenameRequest { name })
            .map_err(network)?;
        decode(request.send().await.map_err(network)?).await
    }

    pub async fn delete_preset(self, id: &PresetId) -> Result<(), ApiError> {
        let path = format!("/api/v1/presets/{id}");
        ensure_success(
            self.request(&path, Request::delete)?
                .send()
                .await
                .map_err(network)?,
        )
        .await
    }

    pub async fn ws_ticket(self) -> Result<String, ApiError> {
        #[derive(Deserialize)]
        struct TicketResponse {
            ticket: String,
        }

        let response: TicketResponse = decode(
            self.request("/api/v1/ws-ticket", Request::post)?
                .send()
                .await
                .map_err(network)?,
        )
        .await?;
        Ok(response.ticket)
    }

    fn request(
        self,
        path: &str,
        method: fn(&str) -> gloo_net::http::RequestBuilder,
    ) -> Result<gloo_net::http::RequestBuilder, ApiError> {
        let token = self.token.peek().clone().ok_or(ApiError::Auth(AuthError))?;
        let url = format!("{}{path}", platform::current_origin());
        // An empty token means the daemon runs with --disable-authentication.
        if token.is_empty() {
            return Ok(method(&url));
        }
        Ok(method(&url).header("Authorization", &format!("Bearer {token}")))
    }
}

/// True when the daemon serves the API without authentication
/// (--disable-authentication): a tokenless request succeeds.
pub async fn server_is_open() -> bool {
    gloo_net::http::Request::get(&format!("{}/api/v1/lights", platform::current_origin()))
        .send()
        .await
        .is_ok_and(|response| response.status() == 200)
}

async fn decode<T: DeserializeOwned>(response: Response) -> Result<T, ApiError> {
    if response.status() == 401 {
        return Err(ApiError::Auth(AuthError));
    }
    if !response.ok() {
        return Err(ApiError::Request(response_message(response).await));
    }
    response
        .json()
        .await
        .map_err(|error| ApiError::Request(format!("invalid daemon response: {error}")))
}

async fn ensure_success(response: Response) -> Result<(), ApiError> {
    if response.status() == 401 {
        return Err(ApiError::Auth(AuthError));
    }
    if response.ok() {
        Ok(())
    } else {
        Err(ApiError::Request(response_message(response).await))
    }
}

async fn response_message(response: Response) -> String {
    let status = response.status();
    response
        .text()
        .await
        .ok()
        .filter(|body| !body.is_empty())
        .map_or_else(
            || format!("daemon returned HTTP {status}"),
            |body| format!("daemon returned HTTP {status}: {body}"),
        )
}

fn network(error: gloo_net::Error) -> ApiError {
    ApiError::Request(format!("request failed: {error}"))
}
