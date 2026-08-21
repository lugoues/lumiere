use dioxus::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct GradientSliderProps {
    pub label: String,
    pub min: i32,
    pub max: i32,
    pub value: i32,
    pub suffix: String,
    pub gradient: String,
    pub onchange: EventHandler<(i32, bool)>,
}

/// A range control with a gradient track.
///
/// Fires on every input event: dioxus-web does not reliably deliver the native
/// change event for range inputs, and the daemon coalesces bursts per light
/// anyway (latest write wins), so live preview is free.
#[component]
pub fn GradientSlider(props: GradientSliderProps) -> Element {
    let mut armed = use_signal(|| true);
    let min_label = format!("{}{}", props.min, props.suffix);
    let max_label = format!("{}{}", props.max, props.suffix);
    let value_label = format!("{}{}", props.value, props.suffix);
    let gradient = props.gradient.clone();

    rsx! {
        div { class: "control-group",
            div { class: "slider-heading",
                label { "{props.label}" }
                output { "{value_label}" }
            }
            div { class: "slider-row",
                span { class: "range-label", "{min_label}" }
                input {
                    class: "gradient-slider",
                    r#type: "range",
                    min: props.min,
                    max: props.max,
                    value: props.value,
                    style: "background: {gradient}",
                    oninput: move |event| {
                        if let Ok(value) = event.value().parse() {
                            props.onchange.call((value, armed()));
                            armed.set(false);
                        }
                    },
                    onpointerup: move |_| armed.set(true),
                    onpointercancel: move |_| armed.set(true),
                    onkeyup: move |_| armed.set(true),
                }
                span { class: "range-label", "{max_label}" }
            }
        }
    }
}
