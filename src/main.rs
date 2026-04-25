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
