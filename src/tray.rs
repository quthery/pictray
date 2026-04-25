use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
};

use crate::{
    events::{AppEvent, WindowAnchor},
    icon,
};

pub struct SystemTray {
    _show_id: MenuId,
    _tray: TrayIcon,
    _toggle_id: MenuId,
    _copy_latest_id: MenuId,
    _clear_history_id: MenuId,
    _quit_id: MenuId,
}

impl SystemTray {
    pub fn new(event_tx: crossbeam_channel::Sender<AppEvent>) -> anyhow::Result<Self> {
        let menu = Menu::new();
        let show = MenuItem::new("Show Pictray", true, None);
        let toggle = MenuItem::new("Toggle Pictray", true, None);
        let copy_latest = MenuItem::new("Copy Latest Image", true, None);
        let clear_history = MenuItem::new("Clear History", true, None);
        let quit = MenuItem::new("Quit", true, None);

        menu.append_items(&[
            &show,
            &toggle,
            &copy_latest,
            &clear_history,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;

        let show_id = show.id().clone();
        let toggle_id = toggle.id().clone();
        let copy_latest_id = copy_latest.id().clone();
        let clear_history_id = clear_history.id().clone();
        let quit_id = quit.id().clone();
        let show_id_for_handler = show_id.clone();
        let toggle_id_for_handler = toggle_id.clone();
        let copy_latest_id_for_handler = copy_latest_id.clone();
        let clear_history_id_for_handler = clear_history_id.clone();
        let quit_id_for_handler = quit_id.clone();
        let menu_tx = event_tx.clone();

        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == show_id_for_handler {
                let _ = menu_tx.send(AppEvent::ShowWindow(None));
            } else if event.id == toggle_id_for_handler {
                let _ = menu_tx.send(AppEvent::ToggleWindow(None));
            } else if event.id == copy_latest_id_for_handler {
                let _ = menu_tx.send(AppEvent::CopyLatest);
            } else if event.id == clear_history_id_for_handler {
                let _ = menu_tx.send(AppEvent::RequestClearHistory);
            } else if event.id == quit_id_for_handler {
                let _ = menu_tx.send(AppEvent::Quit);
            }
        }));

        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button,
                button_state,
                rect,
                ..
            } = event
            {
                if button == MouseButton::Left && button_state == MouseButtonState::Up {
                    let _ = event_tx.send(AppEvent::ToggleWindow(Some(WindowAnchor {
                        x: rect.position.x.round() as i32,
                        y: rect.position.y.round() as i32,
                        width: rect.size.width as i32,
                        height: rect.size.height as i32,
                    })));
                }
            }
        }));

        let tray = TrayIconBuilder::new()
            .with_icon(pictray_icon()?)
            .with_tooltip("Pictray panel - left click toggles the window")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()?;

        Ok(Self {
            _show_id: show_id,
            _tray: tray,
            _toggle_id: toggle_id,
            _copy_latest_id: copy_latest_id,
            _clear_history_id: clear_history_id,
            _quit_id: quit_id,
        })
    }
}

fn pictray_icon() -> anyhow::Result<Icon> {
    let rgba = icon::app_icon_rgba();
    Ok(Icon::from_rgba(
        rgba,
        icon::APP_ICON_SIZE as u32,
        icon::APP_ICON_SIZE as u32,
    )?)
}
