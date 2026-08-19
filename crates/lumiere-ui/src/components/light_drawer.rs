use dioxus::prelude::*;
use lumiere_proto::{Hue, Kelvin, LightSnapshot, Mode, Percent};

use super::{command::send_mode, sliders::GradientSlider};
use crate::{
    api::{ApiClient, ApiError},
    state::AppState,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Cct,
    Hsi,
}

/// Fields the light is known to hold right now, derived from the newest
/// snapshot on every render.
#[derive(Clone, Copy)]
struct Known {
    cct_temp: Option<i32>,
    cct_bri: Option<i32>,
    hsi_hue: Option<i32>,
    hsi_sat: Option<i32>,
    hsi_bri: Option<i32>,
}

fn known_from_mode(mode: Option<Mode>) -> Known {
    let mut known = Known {
        cct_temp: None,
        cct_bri: None,
        hsi_hue: None,
        hsi_sat: None,
        hsi_bri: None,
    };
    match mode {
        Some(Mode::Cct { temp, bri }) => {
            known.cct_temp = Some(i32::from(temp.get()));
            known.cct_bri = Some(i32::from(bri.get()));
        }
        Some(Mode::Hsi { hue, sat, bri }) => {
            known.hsi_hue = Some(i32::from(hue.get()));
            known.hsi_sat = Some(i32::from(sat.get()));
            known.hsi_bri = Some(i32::from(bri.get()));
        }
        _ => {}
    }
    known
}

/// Per-light control drawer.
///
/// Every field the user has NOT touched tracks the live snapshot, so a preset
/// or another client changing the light updates the sliders, and a later drag
/// of one slider never resurrects a stale value for the others. Touched
/// fields belong to the user until the drawer is closed.
#[component]
pub fn LightDrawer(light: LightSnapshot) -> Element {
    let state = use_context::<AppState>();
    let cct_min = i32::from(light.caps.cct_min.get());
    let cct_max = i32::from(light.caps.cct_max.get());
    let rgb = light.caps.rgb;
    let known = known_from_mode(light.confirmed.or(light.desired));

    let initial_tab = if rgb && known.hsi_hue.is_some() {
        Tab::Hsi
    } else {
        Tab::Cct
    };
    let mut tab = use_signal(|| initial_tab);
    // None = untouched: display and send the live value (or the default).
    let mut cct_temp = use_signal(|| None::<i32>);
    let mut cct_bri = use_signal(|| None::<i32>);
    let mut hsi_hue = use_signal(|| None::<i32>);
    let mut hsi_sat = use_signal(|| None::<i32>);
    let mut hsi_bri = use_signal(|| None::<i32>);
    let mut editing_label = use_signal(|| false);
    let mut edit_label = use_signal(String::new);

    let eff_temp = cct_temp()
        .or(known.cct_temp)
        .unwrap_or(5_600)
        .clamp(cct_min, cct_max);
    let eff_cct_bri = cct_bri().or(known.cct_bri).unwrap_or(100);
    let eff_hue = hsi_hue().or(known.hsi_hue).unwrap_or(240);
    let eff_sat = hsi_sat().or(known.hsi_sat).unwrap_or(100);
    let eff_hsi_bri = hsi_bri().or(known.hsi_bri).unwrap_or(100);

    let cct_mode = move |temp: i32, bri: i32| Mode::Cct {
        temp: Kelvin::new(temp.clamp(2_500, 10_000) as u16).expect("clamped to the valid range"),
        bri: Percent::new(bri.clamp(0, 100) as u8).expect("clamped to the valid range"),
    };
    let hsi_mode = move |hue: i32, sat: i32, bri: i32| Mode::Hsi {
        hue: Hue::new(hue.rem_euclid(360) as u16).expect("wrapped into range"),
        sat: Percent::new(sat.clamp(0, 100) as u8).expect("clamped to the valid range"),
        bri: Percent::new(bri.clamp(0, 100) as u8).expect("clamped to the valid range"),
    };

    let cct_id = light.id.clone();
    let cct_bri_id = light.id.clone();
    let hue_id = light.id.clone();
    let sat_id = light.id.clone();
    let hsi_bri_id = light.id.clone();
    let on_id = light.id.clone();
    let off_id = light.id.clone();
    let rename_id = light.id.clone();

    rsx! {
        div { class: "light-drawer",
            div { class: "light-name-row",
                if editing_label() {
                    form {
                        class: "light-name-edit",
                        onsubmit: move |event| {
                            event.prevent_default();
                            let label = edit_label.read().trim().to_owned();
                            if label.is_empty() {
                                state.report_error("Light name cannot be empty.");
                                return;
                            }
                            let id = rename_id.clone();
                            spawn(async move {
                                match ApiClient::new(state.token).set_label(&id, label).await {
                                    Ok(_) => editing_label.set(false),
                                    Err(error) => handle_error(state, error),
                                }
                            });
                        },
                        input {
                            aria_label: "Rename light",
                            value: "{edit_label}",
                            oninput: move |event| edit_label.set(event.value()),
                        }
                        button { class: "btn compact", r#type: "submit", "Save" }
                        button {
                            class: "btn compact",
                            r#type: "button",
                            onclick: move |_| editing_label.set(false),
                            "Cancel"
                        }
                    }
                } else {
                    strong { "{light.label}" }
                    button {
                        class: "light-name-edit-button",
                        aria_label: "Rename {light.label}",
                        title: "Rename",
                        onclick: move |_| {
                            edit_label.set(light.label.clone());
                            editing_label.set(true);
                        },
                        "✎"
                    }
                }
                div { class: "spacer" }
                button {
                    class: "btn success compact",
                    onclick: move |_| send_mode(state, on_id.clone(), Mode::On),
                    "On"
                }
                button {
                    class: "btn danger compact",
                    onclick: move |_| send_mode(state, off_id.clone(), Mode::Off),
                    "Off"
                }
            }
            div { class: "mode-tabs drawer-tabs", role: "tablist",
                button {
                    class: if tab() == Tab::Cct { "mode-tab active" } else { "mode-tab" },
                    role: "tab",
                    aria_selected: tab() == Tab::Cct,
                    onclick: move |_| tab.set(Tab::Cct),
                    "CCT"
                }
                if rgb {
                    button {
                        class: if tab() == Tab::Hsi { "mode-tab active" } else { "mode-tab" },
                        role: "tab",
                        aria_selected: tab() == Tab::Hsi,
                        onclick: move |_| tab.set(Tab::Hsi),
                        "HSI"
                    }
                }
            }
            if tab() == Tab::Cct || !rgb {
                div { class: "mode-pane drawer-mode-pane",
                    GradientSlider {
                        label: "Color temperature",
                        min: cct_min,
                        max: cct_max,
                        value: eff_temp,
                        suffix: " K",
                        gradient: "linear-gradient(90deg, #ff9329 0%, #fff4dc 48%, #c9e2ff 100%)",
                        onchange: move |value: i32| {
                            let value = value.clamp(cct_min, cct_max);
                            cct_temp.set(Some(value));
                            send_mode(state, cct_id.clone(), cct_mode(value, eff_cct_bri));
                        },
                    }
                    GradientSlider {
                        label: "Brightness",
                        min: 0,
                        max: 100,
                        value: eff_cct_bri,
                        suffix: "%",
                        gradient: "linear-gradient(90deg, #101018 0%, #ffffff 100%)",
                        onchange: move |value| {
                            cct_bri.set(Some(value));
                            send_mode(state, cct_bri_id.clone(), cct_mode(eff_temp, value));
                        },
                    }
                    p { class: "range-note", "Range {cct_min} K to {cct_max} K for this light." }
                }
            } else {
                div { class: "mode-pane drawer-mode-pane",
                    GradientSlider {
                        label: "Hue",
                        min: 0,
                        max: 359,
                        value: eff_hue,
                        suffix: "°",
                        gradient: "linear-gradient(90deg, #f33 0%, #ff3 17%, #3f3 33%, #3ff 50%, #33f 67%, #f3f 83%, #f33 100%)",
                        onchange: move |value| {
                            hsi_hue.set(Some(value));
                            send_mode(state, hue_id.clone(), hsi_mode(value, eff_sat, eff_hsi_bri));
                        },
                    }
                    GradientSlider {
                        label: "Saturation",
                        min: 0,
                        max: 100,
                        value: eff_sat,
                        suffix: "%",
                        gradient: "linear-gradient(90deg, #ffffff 0%, #ef4444 100%)",
                        onchange: move |value| {
                            hsi_sat.set(Some(value));
                            send_mode(state, sat_id.clone(), hsi_mode(eff_hue, value, eff_hsi_bri));
                        },
                    }
                    GradientSlider {
                        label: "Brightness",
                        min: 0,
                        max: 100,
                        value: eff_hsi_bri,
                        suffix: "%",
                        gradient: "linear-gradient(90deg, #101018 0%, #ffffff 100%)",
                        onchange: move |value| {
                            hsi_bri.set(Some(value));
                            send_mode(state, hsi_bri_id.clone(), hsi_mode(eff_hue, eff_sat, value));
                        },
                    }
                }
            }
        }
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
    use super::*;

    #[test]
    fn known_fields_follow_the_mode() {
        let cct = known_from_mode(Some(Mode::Cct {
            temp: Kelvin::new(4200).unwrap(),
            bri: Percent::new(20).unwrap(),
        }));
        assert_eq!(cct.cct_temp, Some(4200));
        assert_eq!(cct.cct_bri, Some(20));
        assert_eq!(cct.hsi_hue, None);

        let none = known_from_mode(Some(Mode::On));
        assert_eq!(none.cct_temp, None);
    }
}
