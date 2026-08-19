use dioxus::prelude::*;
use lumiere_proto::{Hue, Kelvin, LightSnapshot, Mode, Percent};

use super::{command::send_mode, sliders::GradientSlider};
use crate::state::AppState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Cct,
    Hsi,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DrawerSeed {
    tab: Tab,
    cct_temp: i32,
    cct_bri: i32,
    hsi_hue: i32,
    hsi_sat: i32,
    hsi_bri: i32,
}

#[component]
pub fn LightDrawer(light: LightSnapshot) -> Element {
    let state = use_context::<AppState>();
    let cct_min = i32::from(light.caps.cct_min.get());
    let cct_max = i32::from(light.caps.cct_max.get());
    let rgb = light.caps.rgb;
    let seed = seed_from_mode(light.confirmed.or(light.desired), cct_min, cct_max, rgb);
    let mut tab = use_signal(|| seed.tab);
    let mut cct_temp = use_signal(|| seed.cct_temp);
    let mut cct_bri = use_signal(|| seed.cct_bri);
    let mut hsi_hue = use_signal(|| seed.hsi_hue);
    let mut hsi_sat = use_signal(|| seed.hsi_sat);
    let mut hsi_bri = use_signal(|| seed.hsi_bri);
    let cct_id = light.id.clone();
    let cct_bri_id = light.id.clone();
    let hue_id = light.id.clone();
    let sat_id = light.id.clone();
    let hsi_bri_id = light.id.clone();
    let on_id = light.id.clone();
    let off_id = light.id.clone();

    rsx! {
        div { class: "light-drawer",
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
                        value: cct_temp(),
                        suffix: " K",
                        gradient: "linear-gradient(90deg, #ff9329 0%, #fff4dc 48%, #c9e2ff 100%)",
                        onchange: move |value: i32| {
                            let value = value.clamp(cct_min, cct_max);
                            cct_temp.set(value);
                            send_mode(state, cct_id.clone(), Mode::Cct {
                                temp: Kelvin::new(value as u16).expect("slider is in range"),
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
                            send_mode(state, cct_bri_id.clone(), Mode::Cct {
                                temp: Kelvin::new(cct_temp() as u16).expect("slider is in range"),
                                bri: Percent::new(value as u8).expect("slider is in range"),
                            });
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
                        value: hsi_hue(),
                        suffix: "°",
                        gradient: "linear-gradient(90deg, #f33 0%, #ff3 17%, #3f3 33%, #3ff 50%, #33f 67%, #f3f 83%, #f33 100%)",
                        onchange: move |value| {
                            hsi_hue.set(value);
                            send_mode(state, hue_id.clone(), Mode::Hsi {
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
                            send_mode(state, sat_id.clone(), Mode::Hsi {
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
                            send_mode(state, hsi_bri_id.clone(), Mode::Hsi {
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
        }
    }
}

fn seed_from_mode(mode: Option<Mode>, cct_min: i32, cct_max: i32, rgb: bool) -> DrawerSeed {
    let mut seed = DrawerSeed {
        tab: Tab::Cct,
        cct_temp: 5_600_i32.clamp(cct_min, cct_max),
        cct_bri: 100,
        hsi_hue: 240,
        hsi_sat: 100,
        hsi_bri: 100,
    };
    match mode {
        Some(Mode::Cct { temp, bri }) => {
            seed.cct_temp = i32::from(temp.get()).clamp(cct_min, cct_max);
            seed.cct_bri = i32::from(bri.get());
        }
        Some(Mode::Hsi { hue, sat, bri }) if rgb => {
            seed.tab = Tab::Hsi;
            seed.hsi_hue = i32::from(hue.get());
            seed.hsi_sat = i32::from(sat.get());
            seed.hsi_bri = i32::from(bri.get());
        }
        _ => {}
    }
    seed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_defaults_and_clamps_temperature() {
        assert_eq!(
            seed_from_mode(None, 6_000, 8_000, true),
            DrawerSeed {
                tab: Tab::Cct,
                cct_temp: 6_000,
                cct_bri: 100,
                hsi_hue: 240,
                hsi_sat: 100,
                hsi_bri: 100,
            }
        );
    }

    #[test]
    fn seeds_cct_and_hsi_modes() {
        let cct = seed_from_mode(
            Some(Mode::Cct {
                temp: Kelvin::new(4_500).unwrap(),
                bri: Percent::new(65).unwrap(),
            }),
            3_200,
            6_500,
            true,
        );
        assert_eq!((cct.tab, cct.cct_temp, cct.cct_bri), (Tab::Cct, 4_500, 65));

        let hsi = seed_from_mode(
            Some(Mode::Hsi {
                hue: Hue::new(120).unwrap(),
                sat: Percent::new(70).unwrap(),
                bri: Percent::new(45).unwrap(),
            }),
            3_200,
            6_500,
            true,
        );
        assert_eq!(
            (hsi.tab, hsi.hsi_hue, hsi.hsi_sat, hsi.hsi_bri),
            (Tab::Hsi, 120, 70, 45)
        );
    }
}
