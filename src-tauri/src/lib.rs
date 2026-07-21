pub mod app_support;
pub mod database;
pub mod domain;
pub mod preview;
pub mod streamer_service;
pub mod supervisor;

pub fn run() {
    app::run();
}

mod app;
