use dioxus::prelude::*;
use lumiere_proto::{AnimationId, AnimationSummary, PlaybackOptions, Selector, TargetBinding};

use crate::{
    api::{ApiClient, ApiError},
    state::AppState,
};

#[component]
pub fn AnimationsPanel() -> Element {
    let state = use_context::<AppState>();
    let mut animations = use_signal(Vec::<AnimationSummary>::new);
    let mut search = use_signal(String::new);
    let mut selected = use_signal(|| None::<AnimationId>);
    let mut speed = use_signal(|| 1.0_f32);
    let mut fps = use_signal(|| 5_u8);
    let mut brightness = use_signal(|| 100_u8);
    let mut looping = use_signal(|| false);

    use_effect(move || {
        spawn(async move {
            match ApiClient::new(state.token).get_animations().await {
                Ok(items) => animations.set(items),
                Err(error) => handle_error(state, error),
            }
        });
    });

    let items = filtered_animations(&animations.read(), &search.read());
    let selected_id = selected.read().clone();
    let chosen = animations
        .read()
        .iter()
        .find(|animation| Some(&animation.id) == selected_id.as_ref())
        .cloned();
    let playback = state.world.read().playback.clone();

    rsx! {
        section { class: "card animations-card",
            div { class: "card-header", "Animations" }
            if let Some(playback) = playback {
                div { class: "now-playing",
                    span { "Now playing: " strong { "{playback.name}" } }
                    button {
                        class: "btn danger compact",
                        onclick: move |_| {
                            spawn(async move {
                                if let Err(error) = ApiClient::new(state.token).stop_playback().await {
                                    handle_error(state, error);
                                }
                            });
                        },
                        "Stop"
                    }
                }
            }
            div { class: "animation-layout",
                div { class: "animation-browser",
                    input {
                        class: "search-input",
                        r#type: "search",
                        placeholder: "Search animations",
                        aria_label: "Search animations",
                        value: "{search}",
                        oninput: move |event| search.set(event.value()),
                    }
                    div { class: "animation-list",
                        if items.is_empty() {
                            p { class: "empty-list", "No animations match." }
                        }
                        for animation in items {
                            {
                                let id = animation.id.clone();
                                let is_selected = selected_id.as_ref() == Some(&animation.id);
                                let loop_default = animation.loop_default;
                                rsx! {
                                    button {
                                        key: "{animation.id}",
                                        class: if is_selected { "animation-item selected" } else { "animation-item" },
                                        onclick: move |_| {
                                            selected.set(Some(id.clone()));
                                            looping.set(loop_default);
                                        },
                                        span { class: "animation-name", "{animation.name}" }
                                        span { class: "animation-meta",
                                            "{animation.keyframes} keyframes"
                                            if animation.loop_default {
                                                span { class: "loop-badge", "Loop" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "play-controls",
                    if let Some(animation) = chosen {
                        h3 { "{animation.name}" }
                        p { class: "animation-description", "{animation.description}" }
                        label {
                            "Speed"
                            select {
                                value: "{speed}",
                                onchange: move |event| {
                                    if let Ok(value) = event.value().parse() { speed.set(value); }
                                },
                                option { value: "0.25", "0.25×" }
                                option { value: "0.5", "0.5×" }
                                option { value: "1", "1×" }
                                option { value: "2", "2×" }
                                option { value: "4", "4×" }
                            }
                        }
                        label {
                            "Rate"
                            input {
                                r#type: "number",
                                min: "1",
                                max: "30",
                                value: "{fps}",
                                oninput: move |event| {
                                    if let Ok(value) = event.value().parse::<u8>() {
                                        fps.set(value.clamp(1, 30));
                                    }
                                },
                            }
                            span { class: "field-suffix", "fps" }
                        }
                        label {
                            "Brightness"
                            input {
                                r#type: "number",
                                min: "5",
                                max: "100",
                                step: "5",
                                value: "{brightness}",
                                oninput: move |event| {
                                    if let Ok(value) = event.value().parse::<u8>() {
                                        brightness.set(value.clamp(5, 100));
                                    }
                                },
                            }
                            span { class: "field-suffix", "%" }
                        }
                        label { class: "checkbox-label",
                            input {
                                r#type: "checkbox",
                                checked: looping(),
                                onchange: move |event| looping.set(event.checked()),
                            }
                            "Loop"
                        }
                        button {
                            class: "btn primary play-button",
                            onclick: move |_| {
                                let id = animation.id.clone();
                                let options = PlaybackOptions {
                                    speed: speed(),
                                    fps: fps(),
                                    bri_scale: f32::from(brightness()) / 100.0,
                                    loop_override: Some(looping()),
                                    ..PlaybackOptions::default()
                                };
                                spawn(async move {
                                    let binding = TargetBinding { all: Selector::All, slots: Vec::new() };
                                    if let Err(error) = ApiClient::new(state.token)
                                        .play_animation(&id, options, binding)
                                        .await
                                    {
                                        handle_error(state, error);
                                    }
                                });
                            },
                            "Play"
                        }
                    } else {
                        p { class: "empty-list", "Select an animation to configure playback." }
                    }
                }
            }
        }
    }
}

fn filtered_animations(items: &[AnimationSummary], query: &str) -> Vec<AnimationSummary> {
    let query = query.trim().to_ascii_lowercase();
    items
        .iter()
        .filter(|item| query.is_empty() || item.name.to_ascii_lowercase().contains(&query))
        .cloned()
        .collect()
}

fn handle_error(state: AppState, error: ApiError) {
    match error {
        ApiError::Auth(_) => state.logout(),
        error => state.report_error(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str, name: &str) -> AnimationSummary {
        AnimationSummary {
            id: AnimationId::parse(id).unwrap(),
            name: name.to_owned(),
            description: String::new(),
            keyframes: 1,
            loop_default: false,
            slot_count: 0,
        }
    }

    #[test]
    fn filtering_is_case_insensitive_and_preserves_order() {
        let items = vec![summary("warm-wave", "Warm Wave"), summary("rain", "Rain")];
        assert_eq!(
            filtered_animations(&items, "WA")
                .into_iter()
                .map(|item| item.name)
                .collect::<Vec<_>>(),
            vec!["Warm Wave"]
        );
        assert_eq!(filtered_animations(&items, "").len(), 2);
    }
}
