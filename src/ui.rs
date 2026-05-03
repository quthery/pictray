use std::{
    cell::RefCell,
    fs::File,
    io::BufReader,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crossbeam_channel::Receiver;
use image::{AnimationDecoder, codecs::gif::GifDecoder, imageops};
use slint::winit_030::{WinitWindowAccessor, winit};
use slint::{
    ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, Timer, TimerMode,
    VecModel,
};

use crate::{
    ImageTile, MainWindow,
    events::{AppEvent, WindowAnchor},
    storage::{ImageStore, StoredImage, StoredImageKind},
};

pub struct PreviewDriver {
    _timer: Timer,
}

pub fn ensure_window_stays_on_top(ui: &MainWindow) {
    let _ = ui
        .window()
        .with_winit_window(|window: &winit::window::Window| {
            // Re-assert the native window level when the tray shows the panel again.
            window.set_window_level(winit::window::WindowLevel::AlwaysOnTop);
        });
}

pub fn apply_native_background_effects(ui: &MainWindow) {
    let _ = ui
        .window()
        .with_winit_window(apply_native_background_effects_to_window);
}

fn apply_native_background_effects_to_window(window: &winit::window::Window) {
    #[cfg(target_os = "windows")]
    {
        use slint::winit_030::winit::platform::windows::{BackdropType, WindowExtWindows};

        window.set_system_backdrop(BackdropType::TransientWindow);
    }

    #[cfg(target_os = "macos")]
    {
        window.set_transparent(true);
        window.set_blur(true);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // On Wayland this uses the compositor blur protocol when available.
        // On X11 it gracefully no-ops.
        window.set_blur(true);
    }
}

#[cfg(target_os = "windows")]
fn start_native_file_drag(ui: &MainWindow, path: PathBuf) -> anyhow::Result<bool> {
    let path = path.canonicalize().unwrap_or(path);
    ui.window()
        .with_winit_window(|window: &winit::window::Window| {
            let item = drag::DragItem::Files(vec![path.clone()]);
            let preview = drag::Image::File(path);
            drag::start_drag(
                window,
                item,
                preview,
                |_result, _cursor_position| {},
                Default::default(),
            )
            .map(|_| true)
            .map_err(anyhow::Error::from)
        })
        .unwrap_or(Ok(false))
}

#[cfg(not(target_os = "windows"))]
fn start_native_file_drag(_ui: &MainWindow, _path: PathBuf) -> anyhow::Result<bool> {
    Ok(false)
}

#[derive(Default)]
struct PreviewState {
    records: Vec<StoredImage>,
    tiles: Vec<CachedTile>,
}

struct CachedTile {
    title: SharedString,
    subtitle: SharedString,
    badge: SharedString,
    preview: SharedString,
    line_numbers: SharedString,
    is_text: bool,
    is_file: bool,
    image: CachedTileImage,
}

enum CachedTileImage {
    Static(Image),
    Animated(AnimatedPreview),
}

struct AnimatedPreview {
    frames: Vec<AnimatedFrame>,
    current_frame: usize,
    next_frame_at: Instant,
}

struct AnimatedFrame {
    image: Image,
    delay: Duration,
}

impl PreviewState {
    fn sync(&mut self, records: Vec<StoredImage>) {
        self.tiles = records.iter().map(load_tile_preview).collect();
        self.records = records;
    }

    fn advance_animations(&mut self, now: Instant) -> bool {
        let mut changed = false;

        for tile in &mut self.tiles {
            let CachedTileImage::Animated(animated) = &mut tile.image else {
                continue;
            };

            while now >= animated.next_frame_at {
                animated.current_frame = (animated.current_frame + 1) % animated.frames.len();
                animated.next_frame_at += animated.frames[animated.current_frame].delay;
                changed = true;
            }
        }

        changed
    }

    fn render_tiles(&self) -> Vec<ImageTile> {
        self.tiles
            .iter()
            .map(|tile| ImageTile {
                title: tile.title.clone(),
                subtitle: tile.subtitle.clone(),
                badge: tile.badge.clone(),
                preview: tile.preview.clone(),
                line_numbers: tile.line_numbers.clone(),
                is_text: tile.is_text,
                is_file: tile.is_file,
                thumbnail: tile.current_image(),
            })
            .collect()
    }
}

impl CachedTile {
    fn current_image(&self) -> Image {
        match &self.image {
            CachedTileImage::Static(image) => image.clone(),
            CachedTileImage::Animated(animated) => {
                animated.frames[animated.current_frame].image.clone()
            }
        }
    }
}

pub fn install_ui_callbacks(
    ui: &MainWindow,
    store: Arc<Mutex<ImageStore>>,
    event_tx: crossbeam_channel::Sender<AppEvent>,
) {
    let ui_weak = ui.as_weak();
    ui.on_hide_window(move || {
        if let Some(ui) = ui_weak.upgrade() {
            let _ = ui.hide();
        }
    });

    let drag_ui_weak = ui.as_weak();
    ui.on_begin_window_drag(move || {
        if let Some(ui) = drag_ui_weak.upgrade() {
            let _ = ui
                .window()
                .with_winit_window(|window: &winit::window::Window| {
                    let _ = window.drag_window();
                });
        }
    });

    let copy_store = Arc::clone(&store);
    let copy_ui_weak = ui.as_weak();
    ui.on_copy_image(move |index| {
        let status = match copy_store.lock() {
            Ok(store) => match store.copy_to_clipboard(index as usize) {
                Ok(()) => "Copied item back to clipboard.".to_owned(),
                Err(err) => format!("Copy failed: {err:#}"),
            },
            Err(_) => "Copy failed: storage lock is poisoned.".to_owned(),
        };

        if let Some(ui) = copy_ui_weak.upgrade() {
            ui.set_status_text(status.into());
        }
    });

    let drag_out_store = Arc::clone(&store);
    let drag_out_ui_weak = ui.as_weak();
    ui.on_drag_out_image(move |index| {
        let drag_path = match drag_out_store.lock() {
            Ok(store) => store.drag_path(index as usize),
            Err(_) => Err(anyhow::anyhow!("storage lock is poisoned")),
        };

        let status = match drag_path {
            Ok(path) => {
                let native_drag = if let Some(ui) = drag_out_ui_weak.upgrade() {
                    start_native_file_drag(&ui, path.clone())
                } else {
                    Ok(false)
                };

                match native_drag {
                    Ok(true) => "Started file drag.".to_owned(),
                    Ok(false) => match crate::clipboard::copy_text(&path.to_string_lossy()) {
                        Ok(()) => format!("Copied path to clipboard: {}", path.display()),
                        Err(err) => format!("Drag failed: {err:#}"),
                    },
                    Err(err) => format!("Drag failed: {err:#}"),
                }
            }
            Err(err) => format!("Drag failed: {err:#}"),
        };

        if let Some(ui) = drag_out_ui_weak.upgrade() {
            ui.set_status_text(status.into());
        }
    });

    let inspect_store = Arc::clone(&store);
    let inspect_ui_weak = ui.as_weak();
    ui.on_inspect_image(move |index| {
        let status = match inspect_store.lock() {
            Ok(store) => match store.reveal_in_file_manager(index as usize) {
                Ok(()) => "Revealed item in file browser.".to_owned(),
                Err(err) => format!("Inspect failed: {err:#}"),
            },
            Err(_) => "Inspect failed: storage lock is poisoned.".to_owned(),
        };

        if let Some(ui) = inspect_ui_weak.upgrade() {
            ui.set_status_text(status.into());
        }
    });

    let delete_store = Arc::clone(&store);
    let delete_tx = event_tx.clone();
    let delete_ui_weak = ui.as_weak();
    ui.on_delete_image(move |index| {
        if let Ok(mut store) = delete_store.lock() {
            let _ = store.delete(index as usize);
        }
        if let Some(ui) = delete_ui_weak.upgrade() {
            ui.set_selected_index(-1);
            ui.set_status_text("Removed item from the buffer.".into());
        }
        let _ = delete_tx.send(AppEvent::StorageChanged);
    });

    let select_ui_weak = ui.as_weak();
    ui.on_select_image(move |index| {
        if let Some(ui) = select_ui_weak.upgrade() {
            let next_index = if ui.get_selected_index() == index {
                -1
            } else {
                index
            };
            ui.set_selected_index(next_index);
        }
    });

    let clear_ui_weak = ui.as_weak();
    ui.on_clear_all(move || {
        if let Ok(mut store) = store.lock() {
            let _ = store.clear();
        }
        if let Some(ui) = clear_ui_weak.upgrade() {
            ui.set_selected_index(-1);
            ui.set_status_text("Buffer cleared.".into());
        }
        let _ = event_tx.send(AppEvent::StorageChanged);
    });
}

pub fn install_preview_driver(ui: &MainWindow, store: Arc<Mutex<ImageStore>>) -> PreviewDriver {
    let timer = Timer::default();
    let state = Rc::new(RefCell::new(PreviewState::default()));
    let ui_weak = ui.as_weak();
    let state_for_timer = Rc::clone(&state);
    let store_for_timer = Arc::clone(&store);

    refresh_previews(&ui_weak, &store, &state);

    timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
        refresh_previews(&ui_weak, &store_for_timer, &state_for_timer)
    });

    PreviewDriver { _timer: timer }
}

pub fn start_event_bridge(
    ui: &MainWindow,
    store: Arc<Mutex<ImageStore>>,
    event_rx: Receiver<AppEvent>,
) {
    let ui_weak = ui.as_weak();

    std::thread::spawn(move || {
        while let Ok(event) = event_rx.recv() {
            match event {
                AppEvent::ShowWindow(anchor) => {
                    let ui_weak = ui_weak.clone();
                    let _ =
                        slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_weak.upgrade() {
                                if !ui.window().is_visible() {
                                    let _ = ui.show();
                                }
                                ensure_window_stays_on_top(&ui);
                                apply_native_background_effects(&ui);
                                if let Some(anchor) = anchor {
                                    place_window_near_anchor(&ui, anchor);
                                }
                                let _ = ui.window().with_winit_window(
                                    |window: &winit::window::Window| window.focus_window(),
                                );
                                ui.window().request_redraw();
                            }
                        });
                }
                AppEvent::ToggleWindow(anchor) => {
                    let ui_weak = ui_weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_weak.upgrade() {
                            if ui.window().is_visible() {
                                let _ = ui.hide();
                            } else {
                                let _ = ui.show();
                                ensure_window_stays_on_top(&ui);
                                apply_native_background_effects(&ui);
                                if let Some(anchor) = anchor {
                                    place_window_near_anchor(&ui, anchor);
                                }
                                let _ = ui.window().with_winit_window(
                                    |window: &winit::window::Window| window.focus_window(),
                                );
                                ui.window().request_redraw();
                            }
                        }
                    });
                }
                AppEvent::CopyLatest => {
                    let status = match store.lock() {
                        Ok(store) => match store.copy_latest_to_clipboard() {
                            Ok(true) => "Copied latest item back to clipboard.".to_owned(),
                            Ok(false) => "No stored item is available yet.".to_owned(),
                            Err(err) => format!("Copy failed: {err:#}"),
                        },
                        Err(_) => "Copy failed: storage lock is poisoned.".to_owned(),
                    };

                    let ui_weak = ui_weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_status_text(status.into());
                        }
                    });
                }
                AppEvent::RequestClearHistory => {
                    if let Ok(mut store) = store.lock() {
                        let _ = store.clear();
                    }
                    let ui_weak = ui_weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_selected_index(-1);
                            ui.set_status_text("Buffer cleared.".into());
                        }
                    });
                }
                AppEvent::StorageChanged => {}
                AppEvent::Quit => {
                    let _ = slint::invoke_from_event_loop(|| {
                        let _ = slint::quit_event_loop();
                    });
                    break;
                }
            }
        }
    });
}

fn refresh_previews(
    ui_weak: &slint::Weak<MainWindow>,
    store: &Arc<Mutex<ImageStore>>,
    state: &Rc<RefCell<PreviewState>>,
) {
    let records = match store.lock() {
        Ok(store) => store.records().to_vec(),
        Err(_) => Vec::new(),
    };

    let now = Instant::now();
    let mut state = state.borrow_mut();
    let mut dirty = false;
    let records_changed = records != state.records;

    if records_changed {
        state.sync(records);
        dirty = true;
    }

    dirty |= state.advance_animations(now);

    if dirty {
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_images(ModelRc::new(VecModel::from(state.render_tiles())));
            if records_changed {
                ui.set_selected_index(-1);
            }
        }
    }
}

fn load_tile_preview(record: &StoredImage) -> CachedTile {
    let title = record.display_name.clone().into();
    let badge: SharedString = match record.kind {
        StoredImageKind::Gif => "GIF".into(),
        StoredImageKind::Raster => record.file_extension.to_ascii_uppercase().into(),
        StoredImageKind::Text => text_file_badge(record).into(),
        StoredImageKind::File => file_badge(record).into(),
    };
    let subtitle = match record.kind {
        StoredImageKind::Gif => format!("{}x{} GIF", record.width, record.height).into(),
        StoredImageKind::Raster => format!("{}x{}", record.width, record.height).into(),
        StoredImageKind::Text => text_file_subtitle(record).into(),
        StoredImageKind::File => file_subtitle(record).into(),
    };
    let preview: SharedString = match record.kind {
        StoredImageKind::File => compact_path_preview(&record.original_path).into(),
        _ => record.text_preview.clone().into(),
    };
    let line_numbers: SharedString = if record.kind == StoredImageKind::Text {
        preview_line_numbers(&record.text_preview).into()
    } else {
        SharedString::default()
    };
    let is_text = record.kind == StoredImageKind::Text;
    let is_file = record.kind == StoredImageKind::File;

    let image = match record.kind {
        StoredImageKind::Gif => load_animated_preview(record)
            .map(CachedTileImage::Animated)
            .unwrap_or_else(|| CachedTileImage::Static(load_static_preview_image(record))),
        StoredImageKind::Raster => CachedTileImage::Static(load_static_preview_image(record)),
        StoredImageKind::Text | StoredImageKind::File => CachedTileImage::Static(Image::default()),
    };

    CachedTile {
        title,
        subtitle,
        badge,
        preview,
        line_numbers,
        is_text,
        is_file,
        image,
    }
}

fn text_file_badge(record: &StoredImage) -> String {
    let normalized = match record.file_extension.as_str() {
        "markdown" => "md",
        "text" => "txt",
        extension => extension,
    };
    normalized
        .chars()
        .take(4)
        .collect::<String>()
        .to_ascii_uppercase()
}

fn text_file_subtitle(record: &StoredImage) -> String {
    let file_kind = text_file_badge(record);
    let line_label = if record.line_count == 1 {
        "line"
    } else {
        "lines"
    };
    format!(
        "{file_kind} • {} {line_label} • {}",
        record.line_count,
        format_byte_len(record.byte_len),
    )
}

fn file_badge(record: &StoredImage) -> String {
    let normalized = if record.file_extension == "file" {
        "file"
    } else {
        record.file_extension.as_str()
    };
    normalized
        .chars()
        .take(5)
        .collect::<String>()
        .to_ascii_uppercase()
}

fn file_subtitle(record: &StoredImage) -> String {
    format!(
        "{} file • {}",
        file_badge(record),
        format_byte_len(record.byte_len)
    )
}

fn compact_path_preview(path: &std::path::Path) -> String {
    path.parent()
        .map(|parent| parent.display().to_string())
        .filter(|parent| !parent.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

fn preview_line_numbers(preview: &str) -> String {
    let line_count = preview.lines().count().max(1);

    (1..=line_count)
        .map(|line| format!("{line:02}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_byte_len(byte_len: u64) -> String {
    if byte_len < 1024 {
        format!("{byte_len} B")
    } else if byte_len < 1024 * 1024 {
        format!("{:.1} KB", byte_len as f64 / 1024.0)
    } else {
        format!("{:.1} MB", byte_len as f64 / (1024.0 * 1024.0))
    }
}

fn load_animated_preview(record: &StoredImage) -> Option<AnimatedPreview> {
    let reader = BufReader::new(File::open(&record.original_path).ok()?);
    let decoder = GifDecoder::new(reader).ok()?;
    let frames = decoder.into_frames().collect_frames().ok()?;

    if frames.len() < 2 {
        return None;
    }

    let frames: Vec<_> = frames
        .into_iter()
        .map(|frame| {
            let delay = normalized_gif_delay(frame.delay());
            let preview = imageops::thumbnail(&frame.into_buffer(), 320, 168);
            AnimatedFrame {
                image: image_from_rgba((preview.width(), preview.height(), preview.into_raw())),
                delay,
            }
        })
        .collect();

    let first_delay = frames.first()?.delay;

    Some(AnimatedPreview {
        frames,
        current_frame: 0,
        next_frame_at: Instant::now() + first_delay,
    })
}

fn normalized_gif_delay(delay: image::Delay) -> Duration {
    let (numerator, denominator) = delay.numer_denom_ms();
    let denominator = denominator.max(1);
    let nanos = (u128::from(numerator) * 1_000_000) / u128::from(denominator);
    let duration = Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64);

    if duration < Duration::from_millis(50) {
        Duration::from_millis(50)
    } else {
        duration
    }
}

fn load_static_preview_image(record: &StoredImage) -> Image {
    record
        .preview_rgba()
        .map(image_from_rgba)
        .unwrap_or_else(|_| Image::default())
}

fn place_window_near_anchor(ui: &MainWindow, anchor: WindowAnchor) {
    let _ = ui
        .window()
        .with_winit_window(|window: &winit::window::Window| {
            let size = window.outer_size();
            let width = size.width as i32;
            let height = size.height as i32;
            let tray_gap = 3;
            let monitor_padding = 6;

            let mut x = anchor.x + (anchor.width - width) / 2;
            let mut y = anchor.y + anchor.height + tray_gap;

            if let Some(monitor) = window.current_monitor() {
                let monitor_pos = monitor.position();
                let monitor_size = monitor.size();
                let min_x = monitor_pos.x + monitor_padding;
                let max_x = monitor_pos.x + monitor_size.width as i32 - width - monitor_padding;
                let min_y = monitor_pos.y + monitor_padding;
                let max_y = monitor_pos.y + monitor_size.height as i32 - height - monitor_padding;

                x = x.clamp(min_x, max_x.max(min_x));

                if y > max_y {
                    y = (anchor.y - height - tray_gap).clamp(min_y, max_y.max(min_y));
                } else {
                    y = y.clamp(min_y, max_y.max(min_y));
                }
            }

            window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
        });
}

fn image_from_rgba((width, height, rgba): (u32, u32, Vec<u8>)) -> Image {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    buffer.make_mut_bytes().copy_from_slice(&rgba);
    Image::from_rgba8(buffer)
}
