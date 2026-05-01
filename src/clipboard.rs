use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use arboard::Clipboard;

use crate::{events::AppEvent, storage, storage::ImageStore};

pub struct ClipboardWorker {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

pub fn copy_text(text: &str) -> anyhow::Result<()> {
    let mut clipboard = Clipboard::new()?;
    clipboard.set_text(text.to_owned())?;
    Ok(())
}

impl Drop for ClipboardWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn spawn_watcher(
    store: Arc<Mutex<ImageStore>>,
    event_tx: crossbeam_channel::Sender<AppEvent>,
) -> ClipboardWorker {
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);

    let handle = thread::spawn(move || {
        let mut last_fingerprint = String::new();

        while !worker_stop.load(Ordering::Relaxed) {
            if let Ok(mut clipboard) = Clipboard::new() {
                if let Ok(image) = clipboard.get_image() {
                    let fingerprint =
                        storage::image_fingerprint(image.width, image.height, image.bytes.as_ref());

                    if fingerprint != last_fingerprint {
                        last_fingerprint = fingerprint;

                        if let Ok(mut store) = store.lock() {
                            if matches!(store.add_clipboard_image(image), Ok(true)) {
                                let _ = event_tx.send(AppEvent::StorageChanged);
                            }
                        }
                    }
                }
            }

            thread::sleep(Duration::from_millis(750));
        }
    });

    ClipboardWorker {
        stop,
        handle: Some(handle),
    }
}
