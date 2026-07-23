pub mod ai;
pub mod app_support;
pub mod database;
pub mod domain;
pub mod preview;
pub mod streamer_service;
pub mod supervisor;
pub mod thumbnail;

pub fn run() {
    app::run();
}

mod app;
