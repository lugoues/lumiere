use dioxus::prelude::*;
use gloo_net::http::{Request, Response};
use lumiere_proto::{CommandRequest, CommandResponse, WorldSnapshot};
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
        Ok(method(&url).header("Authorization", &format!("Bearer {token}")))
    }
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
