use std::{
    cell::RefCell,
    collections::HashMap,
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
    ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, Timer,
    TimerMode, VecModel,
};

use crate::{
    ImageTile, MainWindow,
    events::{AppEvent, WindowAnchor},
    storage::{ImageStore, StoredImage, StoredImageKind},
};

pub struct PreviewDriver {
    _timer: Timer,
    _model: Rc<VecModel<ImageTile>>,
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
        crate::window_effects::apply_to_window(window);
    }

    #[cfg(target_os = "macos")]
    {
        crate::window_effects::apply_to_window(window);
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
    has_thumbnail: bool,
    image: CachedTileImage,
}

impl Clone for CachedTile {
    fn clone(&self) -> Self {
        Self {
            title: self.title.clone(),
            subtitle: self.subtitle.clone(),
            badge: self.badge.clone(),
            preview: self.preview.clone(),
            line_numbers: self.line_numbers.clone(),
            is_text: self.is_text,
            is_file: self.is_file,
            has_thumbnail: self.has_thumbnail,
            image: self.image.clone(),
        }
    }
}

enum CachedTileImage {
    Static(Image),
    Animated(AnimatedPreview),
}

impl Clone for CachedTileImage {
    fn clone(&self) -> Self {
        match self {
            Self::Static(image) => Self::Static(image.clone()),
            Self::Animated(animated) => Self::Animated(animated.clone()),
        }
    }
}

struct AnimatedPreview {
    frames: Vec<AnimatedFrame>,
    current_frame: usize,
    next_frame_at: Instant,
}

impl Clone for AnimatedPreview {
    fn clone(&self) -> Self {
        Self {
            frames: self.frames.clone(),
            current_frame: self.current_frame,
            next_frame_at: self.next_frame_at,
        }
    }
}

struct AnimatedFrame {
    image: Image,
    delay: Duration,
}

impl Clone for AnimatedFrame {
    fn clone(&self) -> Self {
        Self {
            image: self.image.clone(),
            delay: self.delay,
        }
    }
}

const TILE_HEIGHT: f32 = 168.0;
const TILE_HEIGHT_SELECTED: f32 = 214.0;
const TILE_SPACING: f32 = 8.0;

impl PreviewState {
    fn sync(&mut self, records: Vec<StoredImage>) {
        let cached_tiles: HashMap<_, _> = self
            .records
            .drain(..)
            .zip(self.tiles.drain(..))
            .map(|(record, tile)| (record.hash.clone(), (record, tile)))
            .collect();

        self.tiles = records
            .iter()
            .map(|record| match cached_tiles.get(&record.hash) {
                Some((cached_record, cached_tile)) if cached_record == record => {
                    cached_tile.clone()
                }
                _ => load_tile_preview(record),
            })
            .collect();
        self.records = records;
    }

    fn advance_animations(&mut self, now: Instant) -> Vec<usize> {
        let mut changed_rows = Vec::new();

        for (index, tile) in self.tiles.iter_mut().enumerate() {
            let CachedTileImage::Animated(animated) = &mut tile.image else {
                continue;
            };

            let mut row_changed = false;
            while now >= animated.next_frame_at {
                animated.current_frame = (animated.current_frame + 1) % animated.frames.len();
                animated.next_frame_at += animated.frames[animated.current_frame].delay;
                row_changed = true;
            }

            if row_changed {
                changed_rows.push(index);
            }
        }

        changed_rows
    }

    fn render_tiles(&self) -> Vec<ImageTile> {
        self.tiles.iter().map(CachedTile::render).collect()
    }

    fn render_tile(&self, index: usize) -> Option<ImageTile> {
        self.tiles.get(index).map(CachedTile::render)
    }
}

impl CachedTile {
    fn render(&self) -> ImageTile {
        ImageTile {
            title: self.title.clone(),
            subtitle: self.subtitle.clone(),
            badge: self.badge.clone(),
            preview: self.preview.clone(),
            line_numbers: self.line_numbers.clone(),
            is_text: self.is_text,
            is_file: self.is_file,
            has_thumbnail: self.has_thumbnail,
            thumbnail: self.current_image(),
        }
    }

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

    let open_store = Arc::clone(&store);
    let open_ui_weak = ui.as_weak();
    ui.on_open_image(move |index| {
        let status = match open_store.lock() {
            Ok(store) => match store.open_in_associated_app(index as usize) {
                Ok(()) => "Opened item in its app.".to_owned(),
                Err(err) => format!("Open failed: {err:#}"),
            },
            Err(_) => "Open failed: storage lock is poisoned.".to_owned(),
        };

        if let Some(ui) = open_ui_weak.upgrade() {
            ui.set_selected_index(-1);
            ui.set_status_text(status.into());
        }
    });

    let delete_store = Arc::clone(&store);
    let delete_tx = event_tx.clone();
    let delete_ui_weak = ui.as_weak();
    ui.on_delete_image(move |index| {
        let (status, changed) = match delete_store.lock() {
            Ok(mut store) => {
                let had_record = (index as usize) < store.records().len();
                if !had_record {
                    ("No buffered item is available to remove.".to_owned(), false)
                } else {
                    match store.delete(index as usize) {
                        Ok(()) => ("Removed item from the buffer.".to_owned(), true),
                        Err(err) => (
                            format!("Removed item from this session, but cleanup failed: {err:#}"),
                            true,
                        ),
                    }
                }
            }
            Err(_) => ("Remove failed: storage lock is poisoned.".to_owned(), false),
        };

        if let Some(ui) = delete_ui_weak.upgrade() {
            if changed {
                ui.set_selected_index(-1);
            }
            ui.set_status_text(status.into());
            ui.set_show_clear_confirmation(false);
        }
        if changed {
            let _ = delete_tx.send(AppEvent::StorageChanged);
        }
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

    let reorder_store = Arc::clone(&store);
    let reorder_tx = event_tx.clone();
    let reorder_ui_weak = ui.as_weak();
    ui.on_reorder_image(move |from_index, pointer_y| {
        let selected_index = reorder_ui_weak
            .upgrade()
            .map(|ui| ui.get_selected_index())
            .unwrap_or(-1);

        let (status, changed) = match reorder_store.lock() {
            Ok(mut store) => {
                let item_count = store.records().len();
                if item_count < 2 {
                    (
                        "Need at least two buffered items to reorder.".to_owned(),
                        false,
                    )
                } else if from_index < 0 || from_index as usize >= item_count {
                    (
                        "Reorder failed: dragged item is no longer available.".to_owned(),
                        false,
                    )
                } else {
                    let target_index = reorder_target_index(pointer_y, item_count, selected_index);
                    match store.move_record(from_index as usize, target_index) {
                        Ok(true) => (
                            format!("Moved item to position {}.", target_index + 1),
                            true,
                        ),
                        Ok(false) => ("Item stayed in the same position.".to_owned(), false),
                        Err(err) => (format!("Reorder failed: {err:#}"), false),
                    }
                }
            }
            Err(_) => (
                "Reorder failed: storage lock is poisoned.".to_owned(),
                false,
            ),
        };

        if let Some(ui) = reorder_ui_weak.upgrade() {
            ui.set_status_text(status.into());
        }
        if changed {
            let _ = reorder_tx.send(AppEvent::StorageChanged);
        }
    });

    let move_up_store = Arc::clone(&store);
    let move_up_tx = event_tx.clone();
    let move_up_ui_weak = ui.as_weak();
    ui.on_move_image_up(move |index| {
        move_item_by_offset(&move_up_store, &move_up_ui_weak, &move_up_tx, index, -1);
    });

    let move_down_store = Arc::clone(&store);
    let move_down_tx = event_tx.clone();
    let move_down_ui_weak = ui.as_weak();
    ui.on_move_image_down(move |index| {
        move_item_by_offset(
            &move_down_store,
            &move_down_ui_weak,
            &move_down_tx,
            index,
            1,
        );
    });

    let clear_ui_weak = ui.as_weak();
    ui.on_clear_all(move || {
        if let Some(ui) = clear_ui_weak.upgrade() {
            ui.set_show_clear_confirmation(true);
        }
    });

    let confirm_clear_store = Arc::clone(&store);
    let confirm_clear_tx = event_tx.clone();
    let confirm_clear_ui_weak = ui.as_weak();
    ui.on_confirm_clear_all(move || {
        let (status, changed) = match confirm_clear_store.lock() {
            Ok(mut store) => {
                let had_records = !store.records().is_empty();
                if !had_records {
                    ("Buffer is already empty.".to_owned(), false)
                } else {
                    match store.clear() {
                        Ok(()) => ("Buffer cleared.".to_owned(), true),
                        Err(err) => (
                            format!(
                                "Buffer cleared from this session, but cleanup failed: {err:#}"
                            ),
                            true,
                        ),
                    }
                }
            }
            Err(_) => ("Clear failed: storage lock is poisoned.".to_owned(), false),
        };

        if let Some(ui) = confirm_clear_ui_weak.upgrade() {
            ui.set_show_clear_confirmation(false);
            if changed {
                ui.set_selected_index(-1);
            }
            ui.set_status_text(status.into());
        }
        if changed {
            let _ = confirm_clear_tx.send(AppEvent::StorageChanged);
        }
    });

    let cancel_clear_ui_weak = ui.as_weak();
    ui.on_cancel_clear_all(move || {
        if let Some(ui) = cancel_clear_ui_weak.upgrade() {
            ui.set_show_clear_confirmation(false);
        }
    });
}

pub fn install_preview_driver(ui: &MainWindow, store: Arc<Mutex<ImageStore>>) -> PreviewDriver {
    let timer = Timer::default();
    let model = Rc::new(VecModel::from(Vec::<ImageTile>::new()));
    let state = Rc::new(RefCell::new(PreviewState::default()));
    let ui_weak = ui.as_weak();
    let state_for_timer = Rc::clone(&state);
    let store_for_timer = Arc::clone(&store);
    let model_for_timer = Rc::clone(&model);

    ui.set_images(ModelRc::from(model.clone()));

    refresh_previews(&ui_weak, &store, &state, &model);

    timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
        refresh_previews(
            &ui_weak,
            &store_for_timer,
            &state_for_timer,
            &model_for_timer,
        )
    });

    PreviewDriver {
        _timer: timer,
        _model: model,
    }
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
                    let ui_weak = ui_weak.clone();
                    let _ =
                        slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_weak.upgrade() {
                                if !ui.window().is_visible() {
                                    let _ = ui.show();
                                }
                                ensure_window_stays_on_top(&ui);
                                apply_native_background_effects(&ui);
                                ui.set_show_clear_confirmation(true);
                                let _ = ui.window().with_winit_window(
                                    |window: &winit::window::Window| window.focus_window(),
                                );
                                ui.window().request_redraw();
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
    model: &Rc<VecModel<ImageTile>>,
) {
    let records = match store.lock() {
        Ok(store) => store.records().to_vec(),
        Err(_) => Vec::new(),
    };

    let now = Instant::now();
    let mut state = state.borrow_mut();
    let records_changed = records != state.records;

    if records_changed {
        state.sync(records);
    }

    let changed_rows = if records_changed {
        Vec::new()
    } else {
        state.advance_animations(now)
    };

    if records_changed {
        model.set_vec(state.render_tiles());
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_selected_index(-1);
        }
        return;
    }

    for row in changed_rows {
        if let Some(tile) = state.render_tile(row) {
            model.set_row_data(row, tile);
        }
    }
}

fn reorder_target_index(pointer_y: f32, item_count: usize, selected_index: i32) -> usize {
    if item_count == 0 {
        return 0;
    }

    let mut current_y = 0.0;
    for index in 0..item_count {
        let height = if selected_index == index as i32 {
            TILE_HEIGHT_SELECTED
        } else {
            TILE_HEIGHT
        };
        let midpoint = current_y + (height / 2.0);
        if pointer_y <= midpoint {
            return index;
        }
        current_y += height + TILE_SPACING;
    }

    item_count.saturating_sub(1)
}

fn move_item_by_offset(
    store: &Arc<Mutex<ImageStore>>,
    ui_weak: &slint::Weak<MainWindow>,
    event_tx: &crossbeam_channel::Sender<AppEvent>,
    index: i32,
    offset: isize,
) {
    let (status, changed) = match store.lock() {
        Ok(mut store) => {
            let item_count = store.records().len();
            if item_count < 2 {
                (
                    "Need at least two buffered items to reorder.".to_owned(),
                    false,
                )
            } else if index < 0 || index as usize >= item_count {
                (
                    "Move failed: selected item is no longer available.".to_owned(),
                    false,
                )
            } else {
                let from_index = index as usize;
                let target_index = if offset.is_negative() {
                    from_index.saturating_sub(offset.unsigned_abs())
                } else {
                    from_index
                        .saturating_add(offset as usize)
                        .min(item_count.saturating_sub(1))
                };

                match store.move_record(from_index, target_index) {
                    Ok(true) => (
                        format!("Moved item to position {}.", target_index + 1),
                        true,
                    ),
                    Ok(false) => ("Item stayed in the same position.".to_owned(), false),
                    Err(err) => (format!("Move failed: {err:#}"), false),
                }
            }
        }
        Err(_) => ("Move failed: storage lock is poisoned.".to_owned(), false),
    };

    if let Some(ui) = ui_weak.upgrade() {
        ui.set_status_text(status.into());
    }
    if changed {
        let _ = event_tx.send(AppEvent::StorageChanged);
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
    let has_thumbnail = record
        .thumbnail_path
        .as_ref()
        .is_some_and(|path| path.is_file());

    let image = match record.kind {
        StoredImageKind::Gif => load_animated_preview(record)
            .map(CachedTileImage::Animated)
            .unwrap_or_else(|| CachedTileImage::Static(load_static_preview_image(record))),
        StoredImageKind::Raster => CachedTileImage::Static(load_static_preview_image(record)),
        StoredImageKind::File if has_thumbnail => {
            CachedTileImage::Static(load_static_preview_image(record))
        }
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
        has_thumbnail,
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
    if let Some(thumbnail_path) = record.thumbnail_path.as_ref().filter(|path| path.is_file()) {
        if let Ok(rgba) = image::open(thumbnail_path).map(|image| image.to_rgba8()) {
            return image_from_rgba((rgba.width(), rgba.height(), rgba.into_raw()));
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_record(hash: &str, display_name: &str) -> StoredImage {
        StoredImage {
            hash: hash.to_owned(),
            display_name: display_name.to_owned(),
            width: 10,
            height: 10,
            kind: StoredImageKind::Raster,
            original_path: PathBuf::from(format!("/tmp/{hash}.png")),
            thumbnail_path: None,
            reference_path: None,
            text_preview: String::new(),
            line_count: 0,
            byte_len: 4,
            file_extension: "png".to_owned(),
        }
    }

    #[test]
    fn sync_reuses_cached_tiles_when_records_are_reordered() {
        let first = sample_record("first", "First");
        let second = sample_record("second", "Second");
        let original_next_frame_at = Instant::now() + Duration::from_secs(5);

        let mut state = PreviewState {
            records: vec![first.clone(), second.clone()],
            tiles: vec![
                CachedTile {
                    title: "First".into(),
                    subtitle: "10x10".into(),
                    badge: "PNG".into(),
                    preview: SharedString::default(),
                    line_numbers: SharedString::default(),
                    is_text: false,
                    is_file: false,
                    has_thumbnail: true,
                    image: CachedTileImage::Static(Image::default()),
                },
                CachedTile {
                    title: "Second".into(),
                    subtitle: "10x10".into(),
                    badge: "GIF".into(),
                    preview: SharedString::default(),
                    line_numbers: SharedString::default(),
                    is_text: false,
                    is_file: false,
                    has_thumbnail: true,
                    image: CachedTileImage::Animated(AnimatedPreview {
                        frames: vec![AnimatedFrame {
                            image: Image::default(),
                            delay: Duration::from_millis(100),
                        }],
                        current_frame: 0,
                        next_frame_at: original_next_frame_at,
                    }),
                },
            ],
        };

        state.sync(vec![second, first]);

        match &state.tiles[0].image {
            CachedTileImage::Animated(animated) => {
                assert_eq!(animated.next_frame_at, original_next_frame_at);
            }
            CachedTileImage::Static(_) => panic!("expected animated tile to be preserved"),
        }
    }
}
