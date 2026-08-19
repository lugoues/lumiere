use dioxus::prelude::*;
use lumiere_proto::{CommandRequest, Hue, Kelvin, Mode, Percent, Selector};

use super::sliders::GradientSlider;
use crate::{
    api::{ApiClient, ApiError},
    state::AppState,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Cct,
    Hsi,
}

#[component]
pub fn ControlPanel() -> Element {
    let state = use_context::<AppState>();
    let mut tab = use_signal(|| Tab::Cct);
    let mut cct_temp = use_signal(|| 5_600_i32);
    let mut cct_bri = use_signal(|| 100_i32);
    let mut hsi_hue = use_signal(|| 240_i32);
    let mut hsi_sat = use_signal(|| 100_i32);
    let mut hsi_bri = use_signal(|| 100_i32);
    let disabled = state.selection.read().is_empty();

    // Capability envelope of the current selection: HSI only when at least one
    // selected light does color, CCT bounded to the range every light accepts.
    let selection = state.selection.read();
    let world = state.world.read();
    let selected = world
        .lights
        .iter()
        .filter(|light| selection.contains(&light.id))
        .collect::<Vec<_>>();
    let any_rgb = selected.is_empty() || selected.iter().any(|light| light.caps.rgb);
    let cct_min = selected
        .iter()
        .map(|light| i32::from(light.caps.cct_min.get()))
        .max()
        .unwrap_or(2500);
    let cct_max = selected
        .iter()
        .map(|light| i32::from(light.caps.cct_max.get()))
        .min()
        .unwrap_or(10000);
    let (cct_min, cct_max) = if cct_min < cct_max {
        (cct_min, cct_max)
    } else {
        (2500, 10000)
    };
    drop(selection);
    drop(world);
    if !any_rgb && tab() == Tab::Hsi {
        tab.set(Tab::Cct);
    }
    let shown_temp = cct_temp().clamp(cct_min, cct_max);

    rsx! {
        section { class: "card control-card",
            div { class: "card-header",
                "Light control"
                if disabled { span { class: "selection-note", "Select at least one light" } }
            }
            fieldset { class: "control-fieldset", disabled,
                div { class: "mode-tabs", role: "tablist",
                    button {
                        class: if tab() == Tab::Cct { "mode-tab active" } else { "mode-tab" },
                        role: "tab",
                        aria_selected: tab() == Tab::Cct,
                        onclick: move |_| tab.set(Tab::Cct),
                        "CCT"
                    }
                    if any_rgb {
                        button {
                            class: if tab() == Tab::Hsi { "mode-tab active" } else { "mode-tab" },
                            role: "tab",
                            aria_selected: tab() == Tab::Hsi,
                            onclick: move |_| tab.set(Tab::Hsi),
                            "HSI"
                        }
                    }
                }
                if tab() == Tab::Cct {
                    div { class: "mode-pane",
                        GradientSlider {
                            label: "Color temperature",
                            min: cct_min,
                            max: cct_max,
                            value: shown_temp,
                            suffix: " K",
                            gradient: "linear-gradient(90deg, #ff9329 0%, #fff4dc 48%, #c9e2ff 100%)",
                            onchange: move |value| {
                                cct_temp.set(value);
                                send_mode(state, Mode::Cct {
                                    temp: Kelvin::new(value.clamp(2500, 10000) as u16).expect("clamped to the valid range"),
                                    bri: Percent::new(cct_bri() as u8).expect("slider is in range"),
                                });
                            },
                        }
                        GradientSlider {
                            label: "Brightness",
                            min: 0,
                            max: 100,
                            value: cct_bri(),
                            suffix: "%",
                            gradient: "linear-gradient(90deg, #101018 0%, #ffffff 100%)",
                            onchange: move |value| {
                                cct_bri.set(value);
                                send_mode(state, Mode::Cct {
                                    temp: Kelvin::new(cct_temp().clamp(2500, 10000) as u16).expect("clamped to the valid range"),
                                    bri: Percent::new(value as u8).expect("slider is in range"),
                                });
                            },
                        }
                        p { class: "range-note", "Range {cct_min} K to {cct_max} K for the selected lights." }
                    }
                } else {
                    div { class: "mode-pane",
                        GradientSlider {
                            label: "Hue",
                            min: 0,
                            max: 359,
                            value: hsi_hue(),
                            suffix: "°",
                            gradient: "linear-gradient(90deg, #f33 0%, #ff3 17%, #3f3 33%, #3ff 50%, #33f 67%, #f3f 83%, #f33 100%)",
                            onchange: move |value| {
                                hsi_hue.set(value);
                                send_mode(state, Mode::Hsi {
                                    hue: Hue::new(value as u16).expect("slider is in range"),
                                    sat: Percent::new(hsi_sat() as u8).expect("slider is in range"),
                                    bri: Percent::new(hsi_bri() as u8).expect("slider is in range"),
                                });
                            },
                        }
                        GradientSlider {
                            label: "Saturation",
                            min: 0,
                            max: 100,
                            value: hsi_sat(),
                            suffix: "%",
                            gradient: "linear-gradient(90deg, #ffffff 0%, #ef4444 100%)",
                            onchange: move |value| {
                                hsi_sat.set(value);
                                send_mode(state, Mode::Hsi {
                                    hue: Hue::new(hsi_hue() as u16).expect("slider is in range"),
                                    sat: Percent::new(value as u8).expect("slider is in range"),
                                    bri: Percent::new(hsi_bri() as u8).expect("slider is in range"),
                                });
                            },
                        }
                        GradientSlider {
                            label: "Brightness",
                            min: 0,
                            max: 100,
                            value: hsi_bri(),
                            suffix: "%",
                            gradient: "linear-gradient(90deg, #101018 0%, #ffffff 100%)",
                            onchange: move |value| {
                                hsi_bri.set(value);
                                send_mode(state, Mode::Hsi {
                                    hue: Hue::new(hsi_hue() as u16).expect("slider is in range"),
                                    sat: Percent::new(hsi_sat() as u8).expect("slider is in range"),
                                    bri: Percent::new(value as u8).expect("slider is in range"),
                                });
                            },
                        }
                    }
                }
                div { class: "power-row",
                    span { "Power" }
                    div { class: "spacer" }
                    button {
                        class: "btn success",
                        onclick: move |_| send_mode(state, Mode::On),
                        "On"
                    }
                    button {
                        class: "btn danger",
                        onclick: move |_| send_mode(state, Mode::Off),
                        "Off"
                    }
                }
            }
        }
    }
}

fn send_mode(state: AppState, mode: Mode) {
    let mut ids = state.selection.peek().iter().cloned().collect::<Vec<_>>();
    ids.sort();
    if ids.is_empty() {
        return;
    }
    spawn(async move {
        let request = CommandRequest {
            selector: Selector::Ids { ids },
            mode,
            wait: false,
        };
        match ApiClient::new(state.token).post_command(request).await {
            Ok(_) => {}
            Err(ApiError::Auth(_)) => state.logout(),
            Err(error) => state.report_error(error.to_string()),
        }
    });
}
