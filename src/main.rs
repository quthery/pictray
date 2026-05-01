#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod clipboard;
mod events;
mod hotkeys;
mod icon;
mod storage;
mod tray;
mod ui;

slint::include_modules!();

fn main() -> anyhow::Result<()> {
    app::run()
}
