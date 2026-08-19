use dioxus::prelude::*;

use super::{
    animations_panel::AnimationsPanel, header::Header, light_table::LightTable,
    presets_panel::PresetsPanel, token_gate::TokenGate,
};
use crate::{platform, state::AppState, ws};

const MAIN_CSS: Asset = asset!("/assets/main.css");

#[component]
pub fn App() -> Element {
    let mut state = use_context_provider(|| {
        let fragment_token = platform::token_from_url_fragment();
        let token = if let Some(token) = fragment_token {
            platform::store_token(&token);
            platform::strip_url_fragment();
            Some(token)
        } else {
            platform::stored_token()
        };
        AppState::new(token)
    });
    let error = state.error.read().clone();
    let authenticated = state.token.read().is_some();

    // With no saved token the daemon may be running --disable-authentication:
    // probe once, and adopt tokenless mode (empty token) if the API is open.
    use_future(move || async move {
        if state.token.peek().is_none() && crate::api::server_is_open().await {
            state.token.set(Some(String::new()));
        }
    });

    rsx! {
        document::Title { "Lumière Control Panel" }
        document::Stylesheet { href: MAIN_CSS }
        if let Some(message) = error {
            div {
                class: "error-banner",
                role: "alert",
                button {
                    class: "error-dismiss",
                    aria_label: "Dismiss error",
                    onclick: move |_| state.error.set(None),
                    "×"
                }
                "{message}"
            }
        }
        if authenticated {
            LiveApp {}
        } else {
            TokenGate {}
        }
    }
}

#[component]
fn LiveApp() -> Element {
    let state = use_context::<AppState>();
    let _connection = use_future(move || ws::run(state));

    rsx! {
        Header {}
        main { class: "container",
            section { class: "card full-width lights-card",
                div { class: "card-header", "Lights" }
                LightTable {}
            }
            PresetsPanel {}
            AnimationsPanel {}
        }
    }
}
