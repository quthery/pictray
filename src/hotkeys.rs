use std::thread;
use std::time::Duration;

use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};

use crate::events::AppEvent;

pub struct Hotkeys {
    _manager: GlobalHotKeyManager,
    _hotkey: HotKey,
}

impl Hotkeys {
    pub fn register(event_tx: crossbeam_channel::Sender<AppEvent>) -> anyhow::Result<Self> {
        let manager = GlobalHotKeyManager::new()?;
        let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyV);
        manager.register(hotkey)?;

        thread::spawn(move || {
            let receiver = GlobalHotKeyEvent::receiver();
            loop {
                if let Ok(event) = receiver.try_recv() {
                    if event.id == hotkey.id() && event.state == HotKeyState::Pressed {
                        let _ = event_tx.send(AppEvent::CopyLatest);
                    }
                }
                thread::sleep(Duration::from_millis(50));
            }
        });

        Ok(Self {
            _manager: manager,
            _hotkey: hotkey,
        })
    }
}
