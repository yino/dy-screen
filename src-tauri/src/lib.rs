pub mod ai;
pub mod app_lifecycle;
pub mod app_support;
pub mod database;
pub mod domain;
pub mod preview;
pub mod runtime_resource_state;
pub mod room_resolution;
pub mod streamer_service;
pub mod supervisor;
pub mod tauri_browser;
pub mod thumbnail;

pub fn run() {
    app::run();
}

mod app;
