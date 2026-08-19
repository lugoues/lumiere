use dioxus::prelude::*;
use lumiere_proto::{Preset, PresetId, PresetTarget, Selector};

use super::format::mode_summary;
use crate::{
    api::{ApiClient, ApiError},
    state::AppState,
};

#[component]
pub fn PresetsPanel() -> Element {
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
    let lights = state.world.read().lights.clone();
    rsx! {
        section { class: "card presets-panel", aria_label: "Presets",
            div { class: "card-header presets-header",
                "Presets"
                div { class: "spacer" }
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
                        class: "btn compact preset-add",
                        onclick: move |_| {
                            confirming_delete.set(None);
                            saving.set(true);
                        },
                        "+ Save"
                    }
                }
            }
            div { class: "preset-list",
                if items.is_empty() {
                    p { class: "preset-empty", "check lights above, set their drawers, then + Save" }
                }
                for preset in items {
                    {
                        let rename_id = preset.id.clone();
                        let delete_id = preset.id.clone();
                        let recall_id = preset.id.clone();
                        let deleting = confirming_delete.read().as_ref() == Some(&preset.id);
                        let is_editing = editing.read().as_ref() == Some(&preset.id);
                        rsx! {
                            article { class: "preset-card", key: "{preset.id}",
                                div { class: "preset-card-heading",
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
                                        h3 { "{preset.name}" }
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
                                ul { class: "preset-entries",
                                    for entry in preset.entries {
                                        {
                                            let (target, unknown) = target_label(&entry.target, &lights);
                                            let target_class = if unknown { "preset-target unknown" } else { "preset-target" };
                                            let summary = mode_summary(entry.mode);
                                            rsx! {
                                                li {
                                                    span { class: "{target_class}", "{target}" }
                                                    span { class: "preset-mode", "{summary}" }
                                                }
                                            }
                                        }
                                    }
                                }
                                div { class: "preset-card-footer",
                                    button {
                                        class: "btn primary compact",
                                        onclick: move |_| {
                                            let id = recall_id.clone();
                                            spawn(async move {
                                                if let Err(error) = ApiClient::new(state.token).recall_preset(&id).await {
                                                    handle_error(state, error);
                                                }
                                            });
                                        },
                                        "Activate"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn target_label(target: &PresetTarget, lights: &[lumiere_proto::LightSnapshot]) -> (String, bool) {
    match target {
        PresetTarget::Everything => ("All lights".into(), false),
        PresetTarget::Light { id } => lights.iter().find(|light| light.id == *id).map_or_else(
            || (id.to_string(), true),
            |light| (light.label.clone(), false),
        ),
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

fn handle_error(state: AppState, error: ApiError) {
    match error {
        ApiError::Auth(_) => state.logout(),
        error => state.report_error(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use lumiere_proto::{Capabilities, ConnState, Kelvin, LightId, LightSnapshot};

    use super::*;

    fn light(id: &str, label: &str) -> LightSnapshot {
        LightSnapshot {
            id: LightId::sim(id),
            model: String::new(),
            label: label.into(),
            caps: Capabilities {
                cct_min: Kelvin::new(2_500).unwrap(),
                cct_max: Kelvin::new(10_000).unwrap(),
                rgb: true,
                scenes: true,
                cct_split_packets: false,
                reports_status: true,
            },
            conn: ConnState::Connected,
            rssi: None,
            desired: None,
            confirmed: None,
            power: None,
            last_error: None,
        }
    }

    #[test]
    fn resolves_known_and_missing_targets() {
        let lights = vec![light("key", "Key light")];
        assert_eq!(
            target_label(&PresetTarget::Everything, &lights),
            ("All lights".into(), false)
        );
        assert_eq!(
            target_label(
                &PresetTarget::Light {
                    id: LightId::sim("key")
                },
                &lights
            ),
            ("Key light".into(), false)
        );
        assert_eq!(
            target_label(
                &PresetTarget::Light {
                    id: LightId::sim("fill")
                },
                &lights
            ),
            (LightId::sim("fill").to_string(), true)
        );
    }
}
