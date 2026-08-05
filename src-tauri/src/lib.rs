pub mod activation;
pub mod ai;
pub mod api;
pub mod app_lifecycle;
pub mod app_support;
pub mod database;
pub mod domain;
pub mod preview;
pub mod room_resolution;
pub mod runtime_resource_state;
pub mod streamer_service;
pub mod supervisor;
pub mod tauri_browser;
pub mod thumbnail;
pub mod transition_assets;
pub mod transition_matching;
pub mod transition_materials;

pub fn run() {
    app::run();
}

mod app;
