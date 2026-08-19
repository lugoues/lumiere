//! Phase 0 spike: prove the Dioxus 0.7 toolchain builds a working WASM bundle
//! that can share types with lumiere-proto and open a WebSocket.

use dioxus::prelude::*;
use lumiere_proto::{Kelvin, Mode, Percent};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let mut clicks = use_signal(|| 0u32);
    let mut ws_status = use_signal(|| String::from("not connected"));

    // Shared-type sanity check: serialize a Mode with proto's validated newtypes.
    let mode = Mode::Cct {
        temp: Kelvin::new(5600).expect("valid"),
        bri: Percent::new(50).expect("valid"),
    };
    let mode_json = serde_json::to_string(&mode).unwrap_or_default();

    rsx! {
        h1 { "Lumière UI spike" }
        p { "Shared proto type over the boundary: {mode_json}" }
        button {
            onclick: move |_| clicks += 1,
            "Clicked {clicks} times"
        }
        button {
            onclick: move |_| {
                match web_sys::WebSocket::new("ws://127.0.0.1:8080/api/v1/events") {
                    Ok(ws) => {
                        ws_status.set("connecting".into());
                        let mut on_open_status = ws_status;
                        let on_open = Closure::<dyn FnMut()>::new(move || {
                            on_open_status.set("open".into());
                        });
                        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
                        on_open.forget();
                        let mut on_err_status = ws_status;
                        let on_error = Closure::<dyn FnMut()>::new(move || {
                            on_err_status.set("error (is the daemon running?)".into());
                        });
                        ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));
                        on_error.forget();
                    }
                    Err(_) => ws_status.set("failed to create socket".into()),
                }
            },
            "Connect WebSocket"
        }
        p { "WebSocket: {ws_status}" }
    }
}
