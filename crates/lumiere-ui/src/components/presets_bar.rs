use dioxus::prelude::*;
use lumiere_proto::{Preset, PresetId, Selector};

use crate::{
    api::{ApiClient, ApiError},
    state::AppState,
};

#[component]
pub fn PresetsBar() -> Element {
    let state = use_context::<AppState>();
    let mut presets = use_signal(Vec::<Preset>::new);
    let mut refresh = use_signal(|| 0_u32);
    let mut saving = use_signal(|| false);
    let mut save_name = use_signal(String::new);
    let mut editing = use_signal(|| None::<PresetId>);
    let mut edit_name = use_signal(String::new);
    let mut confirming_delete = use_signal(|| None::<PresetId>);

    use_effect(move || {
        let _generation = refresh();
        spawn(async move {
            match ApiClient::new(state.token).get_presets().await {
                Ok(items) => presets.set(items),
                Err(error) => handle_error(state, error),
            }
        });
    });

    let items = presets.read().clone();
    rsx! {
        section { class: "presets-bar full-width", aria_label: "Presets",
            div { class: "presets-title", "Presets" }
            div { class: "preset-strip",
                for preset in items {
                    {
                        let recall_id = preset.id.clone();
                        let rename_id = preset.id.clone();
                        let delete_id = preset.id.clone();
                        let deleting = confirming_delete.read().as_ref() == Some(&preset.id);
                        let is_editing = editing.read().as_ref() == Some(&preset.id);
                        rsx! {
                            div { class: "preset-item", key: "{preset.id}",
                                if is_editing {
                                    form {
                                        class: "preset-edit",
                                        onsubmit: move |event| {
                                            event.prevent_default();
                                            let name = edit_name.read().trim().to_owned();
                                            if name.is_empty() { return; }
                                            let id = rename_id.clone();
                                            spawn(async move {
                                                match ApiClient::new(state.token).rename_preset(&id, name).await {
                                                    Ok(_) => {
                                                        editing.set(None);
                                                        refresh += 1;
                                                    }
                                                    Err(error) => handle_error(state, error),
                                                }
                                            });
                                        },
                                        input {
                                            aria_label: "Rename preset",
                                            value: "{edit_name}",
                                            oninput: move |event| edit_name.set(event.value()),
                                        }
                                        button { class: "btn compact", r#type: "submit", "Save" }
                                        button {
                                            class: "btn compact",
                                            r#type: "button",
                                            onclick: move |_| editing.set(None),
                                            "Cancel"
                                        }
                                    }
                                } else {
                                    button {
                                        class: "preset-recall",
                                        title: "Preset {preset_slug_display(&preset.id)}",
                                        onclick: move |_| {
                                            let id = recall_id.clone();
                                            spawn(async move {
                                                if let Err(error) = ApiClient::new(state.token).recall_preset(&id).await {
                                                    handle_error(state, error);
                                                }
                                            });
                                        },
                                        span { "{preset.name}" }
                                        small { "{preset_slug_display(&preset.id)}" }
                                    }
                                    div { class: "preset-actions",
                                        button {
                                            aria_label: "Rename {preset.name}",
                                            title: "Rename",
                                            onclick: move |_| {
                                                edit_name.set(preset.name.clone());
                                                confirming_delete.set(None);
                                                editing.set(Some(preset.id.clone()));
                                            },
                                            "✎"
                                        }
                                        button {
                                            class: if deleting { "confirm" } else { "" },
                                            aria_label: if deleting { "Confirm delete" } else { "Delete preset" },
                                            title: if deleting { "Click again to confirm" } else { "Delete" },
                                            onclick: move |_| {
                                                if confirming_delete.peek().as_ref() != Some(&delete_id) {
                                                    confirming_delete.set(Some(delete_id.clone()));
                                                    return;
                                                }
                                                let id = delete_id.clone();
                                                spawn(async move {
                                                    match ApiClient::new(state.token).delete_preset(&id).await {
                                                        Ok(()) => {
                                                            confirming_delete.set(None);
                                                            refresh += 1;
                                                        }
                                                        Err(error) => handle_error(state, error),
                                                    }
                                                });
                                            },
                                            if deleting { "✓" } else { "×" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if saving() {
                    form {
                        class: "preset-save",
                        onsubmit: move |event| {
                            event.prevent_default();
                            let name = save_name.read().trim().to_owned();
                            if name.is_empty() { return; }
                            let selector = current_selector(state);
                            spawn(async move {
                                match ApiClient::new(state.token).capture_preset(name, selector).await {
                                    Ok(_) => {
                                        save_name.set(String::new());
                                        saving.set(false);
                                        refresh += 1;
                                    }
                                    Err(error) => handle_error(state, error),
                                }
                            });
                        },
                        input {
                            aria_label: "Preset name",
                            placeholder: "Preset name",
                            value: "{save_name}",
                            oninput: move |event| save_name.set(event.value()),
                        }
                        button { class: "btn primary compact", r#type: "submit", "Save" }
                        button {
                            class: "btn compact",
                            r#type: "button",
                            onclick: move |_| saving.set(false),
                            "Cancel"
                        }
                    }
                } else {
                    button {
                        class: "btn preset-add",
                        onclick: move |_| {
                            confirming_delete.set(None);
                            saving.set(true);
                        },
                        "+ Save"
                    }
                }
            }
        }
    }
}

fn current_selector(state: AppState) -> Selector {
    let mut ids = state.selection.peek().iter().cloned().collect::<Vec<_>>();
    ids.sort();
    if ids.is_empty() {
        Selector::All
    } else {
        Selector::Ids { ids }
    }
}

fn preset_slug_display(id: &PresetId) -> &str {
    id.as_str()
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

    #[test]
    fn slug_display_is_the_stable_identifier() {
        let id = PresetId::parse("warm-light-2").unwrap();
        assert_eq!(preset_slug_display(&id), "warm-light-2");
    }
}
