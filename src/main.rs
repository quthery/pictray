#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod clipboard;
mod events;
mod hotkeys;
mod icon;
mod storage;
mod tray;
mod ui;
mod window_effects;

slint::include_modules!();

fn main() -> anyhow::Result<()> {
    app::run()
}
