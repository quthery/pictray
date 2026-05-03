use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
};

use anyhow::Context;
use crossbeam_channel::unbounded;
#[cfg(target_os = "macos")]
use slint::winit_030::winit::platform::macos::EventLoopBuilderExtMacOS;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
#[cfg(target_os = "macos")]
use slint::winit_030::{SlintEvent, winit::platform::macos::ActivationPolicy};
use slint::{CloseRequestResponse, ComponentHandle};

use crate::{
    MainWindow, clipboard,
    events::AppEvent,
    hotkeys, icon,
    storage::ImageStore,
    tray,
    ui::{
        ensure_window_stays_on_top, install_preview_driver, install_ui_callbacks,
        start_event_bridge,
    },
};

pub fn run() -> anyhow::Result<()> {
    select_backend()?;

    let (event_tx, event_rx) = unbounded::<AppEvent>();
    let store = Arc::new(Mutex::new(ImageStore::open()?));
    let ui = MainWindow::new().context("failed to create Slint window")?;
    install_window_icon(&ui);
    ensure_window_stays_on_top(&ui);

    ui.window()
        .on_close_requested(|| CloseRequestResponse::HideWindow);

    install_ui_callbacks(&ui, Arc::clone(&store), event_tx.clone());
    install_native_window_callbacks(&ui, Arc::clone(&store), event_tx.clone());
    let _preview_driver = install_preview_driver(&ui, Arc::clone(&store));

    start_event_dispatcher(&ui, Arc::clone(&store), event_rx);

    let _tray = tray::SystemTray::new(event_tx.clone())?;
    let _hotkeys = hotkeys::Hotkeys::register(event_tx.clone())?;

    let _ = event_tx.send(AppEvent::StorageChanged);

    slint::run_event_loop_until_quit()?;

    Ok(())
}

fn select_backend() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let mut event_loop_builder = winit::event_loop::EventLoop::<SlintEvent>::with_user_event();
        // Winit treats unbundled macOS executables as regular apps by default, so
        // force the accessory policy to keep `cargo run` tray-only as well.
        event_loop_builder.with_activation_policy(ActivationPolicy::Accessory);

        slint::BackendSelector::new()
            .backend_name("winit".into())
            .with_winit_event_loop_builder(event_loop_builder)
            .select()
            .context("failed to select winit backend")?;
    }

    #[cfg(not(target_os = "macos"))]
    {
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .select()
            .context("failed to select winit backend")?;
    }

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
                let modifiers = *modifiers_for_events.borrow();
                let has_shortcut_modifier = modifiers.super_key() || modifiers.control_key();
                let is_paste = has_shortcut_modifier
                    && matches!(event.logical_key.as_ref(), winit::keyboard::Key::Character(ch) if ch.eq_ignore_ascii_case("v"));
                let is_copy = has_shortcut_modifier
                    && matches!(event.logical_key.as_ref(), winit::keyboard::Key::Character(ch) if ch.eq_ignore_ascii_case("c"));

                if is_paste {
                    let paste_result = if let Ok(mut store) = store.lock() {
                        store.add_current_clipboard_item()
                    } else {
                        Ok(false)
                    };
                    let pasted = matches!(&paste_result, Ok(true));

                    if let Some(ui) = ui_weak.upgrade() {
                        let status = match &paste_result {
                            Ok(true) => "Buffered clipboard item.".to_owned(),
                            Ok(false) => {
                                "Clipboard does not contain a supported image or file.".to_owned()
                            }
                            Err(err) => format!("Paste failed: {err:#}"),
                        };
                        ui.set_status_text(status.into());
                    }

                    if pasted {
                        let _ = event_tx.send(AppEvent::StorageChanged);
                    }
                    return EventResult::PreventDefault;
                }

                if is_copy {
                    let selected_index = ui_weak
                        .upgrade()
                        .map(|ui| ui.get_selected_index())
                        .unwrap_or(-1);
                    let copy_result = if let Ok(store) = store.lock() {
                        if selected_index >= 0 {
                            store.copy_to_clipboard(selected_index as usize).map(|_| true)
                        } else {
                            store.copy_latest_to_clipboard()
                        }
                    } else {
                        Err(anyhow::anyhow!("storage lock is poisoned"))
                    };

                    if let Some(ui) = ui_weak.upgrade() {
                        let status = match copy_result {
                            Ok(true) if selected_index >= 0 => {
                                "Copied selected item to the clipboard.".to_owned()
                            }
                            Ok(true) => "Copied latest item to the clipboard.".to_owned(),
                            Ok(false) => "No buffered item is available to copy.".to_owned(),
                            Err(err) => format!("Copy failed: {err:#}"),
                        };
                        ui.set_status_text(status.into());
                    }

                    return EventResult::PreventDefault;
                }
            }
            winit::event::WindowEvent::HoveredFile(path) => {
                if path.is_file() {
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

                if path.is_file() {
                    let copy_result = clipboard::copy_text(&path.to_string_lossy());
                    let store_result = if let Ok(mut store) = store.lock() {
                        store.add_file_reference(path)
                    } else {
                        Ok(false)
                    };
                    if let Some(ui) = ui_weak.upgrade() {
                        let status = match (copy_result, store_result) {
                            (Ok(()), Ok(true)) => {
                                format!("Buffered file path and copied it: {}", path.display())
                            }
                            (Ok(()), Ok(false)) => {
                                format!("Copied existing file path: {}", path.display())
                            }
                            (Ok(()), Err(err)) => format!(
                                "Copied path, but buffering failed: {err:#}"
                            ),
                            (Err(err), _) => format!("Copy failed: {err:#}"),
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
