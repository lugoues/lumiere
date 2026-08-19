use dioxus::prelude::*;
use lumiere_proto::{ConnState, LightId, LightSnapshot};

use super::{format::mode_summary, light_drawer::LightDrawer};
use crate::state::AppState;

#[component]
pub fn LightTable() -> Element {
    let mut state = use_context::<AppState>();
    let lights = state.world.read().lights.clone();
    let all_ids = lights
        .iter()
        .map(|light| light.id.clone())
        .collect::<Vec<_>>();
    let selected = state.selection.read().clone();
    let expanded = state.expanded.read().clone();
    let all_selected =
        !lights.is_empty() && lights.iter().all(|light| selected.contains(&light.id));

    rsx! {
        div { class: "table-scroll",
            table { class: "light-table",
                thead {
                    tr {
                        th { class: "select-column",
                            input {
                                r#type: "checkbox",
                                aria_label: "Include all lights in preset capture",
                                checked: all_selected,
                                onchange: move |_| {
                                    let mut selection = state.selection.write();
                                    if all_selected {
                                        selection.clear();
                                    } else {
                                        *selection = all_ids.iter().cloned().collect();
                                    }
                                }
                            }
                        }
                        th { "Label" }
                        th { "Model" }
                        th { "Status" }
                        th { "RSSI" }
                        th { "Current" }
                    }
                }
                tbody {
                    if lights.is_empty() {
                        tr {
                            td { class: "empty-state", colspan: 6,
                                "No lights found. Start a scan to discover nearby lights."
                            }
                        }
                    }
                    for light in lights {
                        LightRow {
                            key: "{light.id}",
                            selected: selected.contains(&light.id),
                            expanded: expanded.contains(&light.id),
                            light,
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn LightRow(light: LightSnapshot, selected: bool, expanded: bool) -> Element {
    let mut state = use_context::<AppState>();
    let id = light.id.clone();
    let checkbox_id = id.clone();
    let drawer_light = light.clone();
    let (connection_class, connection_text) = connection_summary(&light.conn);
    let (mode_text, pending) = light_mode_summary(&light);
    let row_class = if selected { "selected" } else { "" };
    let current_class = if pending {
        "mode-summary pending"
    } else {
        "mode-summary"
    };

    rsx! {
        tr {
            class: "light-row {row_class}",
            aria_expanded: expanded,
            onclick: move |_| toggle(&mut state.expanded, &id),
            td {
                class: "select-column",
                onclick: move |event| event.stop_propagation(),
                input {
                    r#type: "checkbox",
                    aria_label: "Include {light.label} in preset capture",
                    checked: selected,
                    onchange: move |_| toggle(&mut state.selection, &checkbox_id),
                }
            }
            td {
                span { class: if expanded { "chevron expanded" } else { "chevron" }, "›" }
                div { class: "light-label", "{light.label}" }
                div { class: "light-id", "{light.id}" }
            }
            td { "{light.model}" }
            td { span { class: "conn-chip {connection_class}", "{connection_text}" } }
            td { class: "rssi", {light.rssi.map_or_else(|| "-".into(), |rssi| format!("{rssi} dBm"))} }
            td { class: "{current_class}",
                "{mode_text}"
                if pending { span { class: "pending-label", "pending" } }
            }
        }
        if expanded {
            tr { class: "drawer-row",
                td { colspan: 6, LightDrawer { light: drawer_light } }
            }
        }
    }
}

fn toggle(selection: &mut Signal<std::collections::HashSet<LightId>>, id: &LightId) {
    let mut selection = selection.write();
    if !selection.remove(id) {
        selection.insert(id.clone());
    }
}

fn connection_summary(connection: &ConnState) -> (&'static str, String) {
    match connection {
        ConnState::Discovered => ("discovered", "Discovered".into()),
        ConnState::Connecting { attempt } => ("reconnecting", format!("Connecting {attempt}")),
        ConnState::Connected => ("connected", "Connected".into()),
        ConnState::Reconnecting { attempt } => ("reconnecting", format!("Reconnecting {attempt}")),
        ConnState::Lost => ("lost", "Lost".into()),
    }
}

fn light_mode_summary(light: &LightSnapshot) -> (String, bool) {
    let (mode, pending) = match (light.confirmed, light.desired) {
        (Some(mode), _) => (Some(mode), false),
        (None, Some(mode)) => (Some(mode), true),
        (None, None) => (None, false),
    };
    let summary = mode.map(mode_summary).unwrap_or_else(|| {
        light.power.map_or_else(
            || "Unknown".into(),
            |power| if power { "On" } else { "Off" }.into(),
        )
    });
    (summary, pending)
}
