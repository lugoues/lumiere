use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use crate::{
    api::{ApiClient, ApiError},
    state::{AppState, ConnStatus},
};

#[component]
pub fn Header() -> Element {
    let state = use_context::<AppState>();
    let mut scanning = use_signal(|| false);
    let connection = *state.conn.read();
    let (status_class, status_text) = match connection {
        ConnStatus::Connecting => ("connecting", "Connecting".to_owned()),
        ConnStatus::Live => ("live", "Live".to_owned()),
        ConnStatus::Reconnecting { attempt } => {
            ("reconnecting", format!("Reconnecting ({attempt})"))
        }
    };

    rsx! {
        header { class: "header",
            div { class: "brand", span { "Lumi" } "ère" }
            div { class: "spacer" }
            span { class: "status-pill {status_class}",
                span { class: "status-dot" }
                "{status_text}"
            }
            button {
                class: "btn scan-btn",
                disabled: scanning(),
                onclick: move |_| {
                    if scanning() {
                        return;
                    }
                    scanning.set(true);
                    spawn(async move {
                        match ApiClient::new(state.token).post_scan().await {
                            Ok(()) => TimeoutFuture::new(10_000).await,
                            Err(ApiError::Auth(_)) => state.logout(),
                            Err(error) => state.report_error(error.to_string()),
                        }
                        scanning.set(false);
                    });
                },
                if scanning() { "Scanning…" } else { "Scan" }
            }
        }
    }
}
