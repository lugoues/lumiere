//! Browser access is isolated here so a future desktop build can replace this module.

#[cfg(target_arch = "wasm32")]
const TOKEN_KEY: &str = "lumiere.token";

/// Extracts a token from a URL fragment in the form `#t=TOKEN`.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub fn parse_fragment(hash: &str) -> Option<String> {
    let token = hash.strip_prefix("#t=")?.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

#[cfg(target_arch = "wasm32")]
fn window() -> Option<web_sys::Window> {
    web_sys::window()
}

/// Reads the token from the current URL fragment.
#[cfg(target_arch = "wasm32")]
pub fn token_from_url_fragment() -> Option<String> {
    let hash = window()?.location().hash().ok()?;
    parse_fragment(&hash)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn token_from_url_fragment() -> Option<String> {
    None
}

/// Removes the current URL fragment without navigating.
#[cfg(target_arch = "wasm32")]
pub fn strip_url_fragment() {
    let Some(window) = window() else {
        return;
    };
    let location = window.location();
    let Ok(pathname) = location.pathname() else {
        return;
    };
    let search = location.search().unwrap_or_default();
    let clean_url = format!("{pathname}{search}");
    if let Ok(history) = window.history() {
        let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&clean_url));
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn strip_url_fragment() {}

/// Reads a previously persisted token.
#[cfg(target_arch = "wasm32")]
pub fn stored_token() -> Option<String> {
    window()?.local_storage().ok()??.get_item(TOKEN_KEY).ok()?
}

#[cfg(not(target_arch = "wasm32"))]
pub fn stored_token() -> Option<String> {
    None
}

/// Persists a token for subsequent visits.
#[cfg(target_arch = "wasm32")]
pub fn store_token(token: &str) {
    if let Some(storage) = window().and_then(|window| window.local_storage().ok().flatten()) {
        let _ = storage.set_item(TOKEN_KEY, token);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn store_token(_token: &str) {}

/// Removes the persisted token.
#[cfg(target_arch = "wasm32")]
pub fn clear_stored_token() {
    if let Some(storage) = window().and_then(|window| window.local_storage().ok().flatten()) {
        let _ = storage.remove_item(TOKEN_KEY);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn clear_stored_token() {}

/// Returns the browser origin used by the REST API.
#[cfg(target_arch = "wasm32")]
pub fn current_origin() -> String {
    window()
        .and_then(|window| window.location().origin().ok())
        .unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn current_origin() -> String {
    String::new()
}

/// Converts an HTTP origin and path into a WebSocket URL.
pub fn ws_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let scheme = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_owned()
    };
    format!("{scheme}/{}", path.trim_start_matches('/'))
}

/// Returns per-attempt reconnect jitter from the browser random source.
#[cfg(target_arch = "wasm32")]
pub fn reconnect_jitter_ms(attempt: u32, maximum: u32) -> u32 {
    if maximum == 0 {
        return 0;
    }
    let mut bytes = [0_u8; 4];
    if window()
        .and_then(|window| window.crypto().ok())
        .is_some_and(|crypto| crypto.get_random_values_with_u8_array(&mut bytes).is_ok())
    {
        u32::from_ne_bytes(bytes) % maximum
    } else {
        attempt.wrapping_mul(1_103_515_245).wrapping_add(12_345) % maximum
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn reconnect_jitter_ms(attempt: u32, maximum: u32) -> u32 {
    if maximum == 0 {
        0
    } else {
        attempt.wrapping_mul(1_103_515_245).wrapping_add(12_345) % maximum
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_token_fragment() {
        assert_eq!(parse_fragment("#t=secret"), Some("secret".into()));
        assert_eq!(parse_fragment("#t=  secret  "), Some("secret".into()));
        assert_eq!(parse_fragment("#t="), None);
        assert_eq!(parse_fragment("#other=secret"), None);
        assert_eq!(parse_fragment(""), None);
    }

    #[test]
    fn converts_origins_to_websocket_urls() {
        assert_eq!(
            ws_url("https://lights.example", "/api/v1/events"),
            "wss://lights.example/api/v1/events"
        );
        assert_eq!(
            ws_url("http://localhost:8080/", "api/v1/events"),
            "ws://localhost:8080/api/v1/events"
        );
    }
}
