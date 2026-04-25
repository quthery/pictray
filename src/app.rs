use std::{
    cell::RefCell,
    path::Path,
    rc::Rc,
    sync::{Arc, Mutex},
};

use anyhow::Context;
use crossbeam_channel::unbounded;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{CloseRequestResponse, ComponentHandle};

use crate::{
    MainWindow, clipboard,
    events::AppEvent,
    hotkeys, icon,
    storage::ImageStore,
    tray,
    ui::{install_preview_driver, install_ui_callbacks, start_event_bridge},
};

pub fn run() -> anyhow::Result<()> {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .select()
        .context("failed to select winit backend")?;

    configure_platform_app_mode()?;

    let (event_tx, event_rx) = unbounded::<AppEvent>();
    let store = Arc::new(Mutex::new(ImageStore::open()?));
    let ui = MainWindow::new().context("failed to create Slint window")?;
    install_window_icon(&ui);

    ui.window()
        .on_close_requested(|| CloseRequestResponse::HideWindow);

    install_ui_callbacks(&ui, Arc::clone(&store), event_tx.clone());
    install_native_window_callbacks(&ui, Arc::clone(&store), event_tx.clone());
    let _preview_driver = install_preview_driver(&ui, Arc::clone(&store));

    start_event_dispatcher(&ui, Arc::clone(&store), event_rx);

    let _tray = tray::SystemTray::new(event_tx.clone())?;
    let _hotkeys = hotkeys::Hotkeys::register(event_tx.clone())?;
    let _clipboard_worker = clipboard::spawn_watcher(Arc::clone(&store), event_tx.clone());

    let _ = event_tx.send(AppEvent::StorageChanged);

    slint::run_event_loop_until_quit()?;

    Ok(())
}

#[cfg(target_os = "macos")]
fn configure_platform_app_mode() -> anyhow::Result<()> {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::MainThreadMarker;

    let mtm =
        MainThreadMarker::new().context("macOS app mode must be configured on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    anyhow::ensure!(
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory),
        "failed to switch Pictray to tray-only mode",
    );
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn configure_platform_app_mode() -> anyhow::Result<()> {
    Ok(())
}

fn install_window_icon(ui: &MainWindow) {
    let rgba = icon::app_icon_rgba();
    let _ = ui
        .window()
        .with_winit_window(|window: &winit::window::Window| {
            if let Ok(icon) = winit::window::Icon::from_rgba(
                rgba,
                icon::APP_ICON_SIZE as u32,
                icon::APP_ICON_SIZE as u32,
            ) {
                window.set_window_icon(Some(icon));
            }
        });
}

fn start_event_dispatcher(
    ui: &MainWindow,
    store: Arc<Mutex<ImageStore>>,
    event_rx: crossbeam_channel::Receiver<AppEvent>,
) {
    start_event_bridge(ui, store, event_rx);
}

fn install_native_window_callbacks(
    ui: &MainWindow,
    store: Arc<Mutex<ImageStore>>,
    event_tx: crossbeam_channel::Sender<AppEvent>,
) {
    let ui_weak = ui.as_weak();
    let modifiers = Rc::new(RefCell::new(winit::keyboard::ModifiersState::default()));
    let modifiers_for_events = Rc::clone(&modifiers);

    ui.window().on_winit_window_event(move |_slint_window, event| {
        match event {
            winit::event::WindowEvent::ModifiersChanged(new_modifiers) => {
                *modifiers_for_events.borrow_mut() = new_modifiers.state();
            }
            winit::event::WindowEvent::KeyboardInput { event, is_synthetic: false, .. }
                if event.state.is_pressed() =>
            {
                let is_paste = matches!(event.logical_key.as_ref(), winit::keyboard::Key::Character(ch) if ch.eq_ignore_ascii_case("v"))
                    && {
                        let modifiers = *modifiers_for_events.borrow();
                        modifiers.super_key() || modifiers.control_key()
                    };

                if is_paste {
                    let paste_result = if let Ok(mut store) = store.lock() {
                        store.add_current_clipboard_image()
                    } else {
                        Ok(false)
                    };

                    if let Some(ui) = ui_weak.upgrade() {
                        let status = match paste_result {
                            Ok(true) => "Added image from clipboard.".to_owned(),
                            Ok(false) => "Clipboard image already buffered or unavailable.".to_owned(),
                            Err(err) => format!("Paste failed: {err:#}"),
                        };
                        ui.set_status_text(status.into());
                    }

                    let _ = event_tx.send(AppEvent::StorageChanged);
                    return EventResult::PreventDefault;
                }
            }
            winit::event::WindowEvent::HoveredFile(path) => {
                if is_supported_import_path(path) {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_drop_active(true);
                    }
                }
            }
            winit::event::WindowEvent::HoveredFileCancelled => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_drop_active(false);
                }
            }
            winit::event::WindowEvent::DroppedFile(path) => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_drop_active(false);
                }

                if is_supported_import_path(path) {
                    let import_result = if let Ok(mut store) = store.lock() {
                        store.add_image_file(path)
                    } else {
                        Ok(false)
                    };

                    if let Some(ui) = ui_weak.upgrade() {
                        let status = match import_result {
                            Ok(true) => format!("Added {}.", path.display()),
                            Ok(false) => "Image is already buffered.".to_owned(),
                            Err(err) => format!("Import failed: {err:#}"),
                        };
                        ui.set_status_text(status.into());
                    }

                    let _ = event_tx.send(AppEvent::StorageChanged);
                    return EventResult::PreventDefault;
                }
            }
            _ => {}
        }

        EventResult::Propagate
    });
}

fn is_supported_import_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase()),
        Some(ext)
            if matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff"
            )
    )
}
