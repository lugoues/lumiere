mod api;
mod components;
mod platform;
mod state;
mod ws;

fn main() {
    dioxus::launch(components::App);
}
