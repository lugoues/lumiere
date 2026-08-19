use dioxus::prelude::*;

use crate::{platform, state::AppState};

#[component]
pub fn TokenGate() -> Element {
    let mut token = use_signal(String::new);
    let mut state = use_context::<AppState>();

    let mut connect = move || {
        let entered = token.read().trim().to_owned();
        if entered.is_empty() {
            state.report_error("Enter an API token to connect");
            return;
        }
        platform::store_token(&entered);
        state.token.set(Some(entered));
    };

    rsx! {
        main { class: "token-page",
            section { class: "token-card",
                div { class: "brand large", span { "Lumi" } "ère" }
                h1 { "Connect to your lights" }
                p { "Paste the API token configured for the Lumière daemon." }
                form {
                    onsubmit: move |event| {
                        event.prevent_default();
                        connect();
                    },
                    label { r#for: "api-token", "API token" }
                    input {
                        id: "api-token",
                        r#type: "password",
                        autocomplete: "current-password",
                        placeholder: "Paste token",
                        value: "{token}",
                        oninput: move |event| token.set(event.value()),
                    }
                    button { class: "btn primary connect-btn", r#type: "submit", "Connect" }
                }
                p { class: "token-hint",
                    "You can also open this page once with "
                    code { "#t=your-token" }
                    ". The fragment is removed after the token is saved."
                }
            }
        }
    }
}
