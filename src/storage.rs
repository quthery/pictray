use std::{
    borrow::Cow,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

use anyhow::{Context, anyhow};
use arboard::{Clipboard, ImageData};
use image::{RgbaImage, imageops};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredImageKind {
    Raster,
    Gif,
    Text,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    Copy,
    Move,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredImage {
    pub hash: String,
    pub display_name: String,
    pub width: u32,
    pub height: u32,
    pub kind: StoredImageKind,
    pub original_path: PathBuf,
    pub thumbnail_path: Option<PathBuf>,
    pub reference_path: Option<PathBuf>,
    pub text_preview: String,
    pub line_count: usize,
    pub byte_len: u64,
    pub file_extension: String,
}

impl StoredImage {
    pub fn preview_rgba(&self) -> anyhow::Result<(u32, u32, Vec<u8>)> {
        anyhow::ensure!(
            matches!(self.kind, StoredImageKind::Raster | StoredImageKind::Gif),
            "this item does not expose image previews",
        );

        let rgba = image::open(&self.original_path)
            .with_context(|| format!("failed to open preview {}", self.original_path.display()))?
            .to_rgba8();

        Ok((rgba.width(), rgba.height(), rgba.into_raw()))
    }
}

#[derive(Debug)]
pub struct ImageStore {
    root: PathBuf,
    originals_dir: PathBuf,
    thumbnails_dir: PathBuf,
    metadata_dir: PathBuf,
    file_refs_dir: PathBuf,
    records: Vec<StoredImage>,
}

impl ImageStore {
    pub fn open() -> anyhow::Result<Self> {
        let root = dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("pictray");
        Self::open_at(root)
    }

    fn open_at(root: PathBuf) -> anyhow::Result<Self> {
        let originals_dir = root.join("originals");
        let thumbnails_dir = root.join("thumbnails");
        let metadata_dir = root.join("metadata");
        let file_refs_dir = root.join("file-refs");

        fs::create_dir_all(&originals_dir)?;
        fs::create_dir_all(&thumbnails_dir)?;
        fs::create_dir_all(&metadata_dir)?;
        fs::create_dir_all(&file_refs_dir)?;

        let records = load_records(
            &originals_dir,
            &thumbnails_dir,
            &metadata_dir,
            &file_refs_dir,
        )?;

        Ok(Self {
            root,
            originals_dir,
            thumbnails_dir,
            metadata_dir,
            file_refs_dir,
            records,
        })
    }

    pub fn add_clipboard_image(&mut self, image: ImageData<'_>) -> anyhow::Result<bool> {
        let width = image.width as u32;
        let height = image.height as u32;
        let rgba = image.bytes.into_owned();
        let rgba_image = RgbaImage::from_raw(width, height, rgba)
            .ok_or_else(|| anyhow!("clipboard image buffer does not match dimensions"))?;
        self.persist_rgba_image(rgba_image, "Clipboard")
    }

    pub fn add_current_clipboard_item(&mut self) -> anyhow::Result<bool> {
        let mut clipboard = Clipboard::new()?;

        if let Ok(paths) = clipboard.get().file_list() {
            let mut imported_any = false;
            for path in paths {
                imported_any |= self.import_path(&path, ImportMode::Copy)?;
            }

            if imported_any {
                return Ok(true);
            }
        }

        if let Ok(image) = clipboard.get_image() {
            return self.add_clipboard_image(image);
        }

        if let Ok(text) = clipboard.get_text() {
            let mut imported_any = false;
            for path in extract_existing_paths_from_clipboard_text(&text) {
                imported_any |= self.import_path(&path, ImportMode::Copy)?;
            }

            return Ok(imported_any);
        }

        Ok(false)
    }

    pub fn add_image_file(&mut self, path: &Path) -> anyhow::Result<bool> {
        self.add_image_file_with_mode(path, ImportMode::Copy)
    }

    pub fn import_path(&mut self, path: &Path, mode: ImportMode) -> anyhow::Result<bool> {
        if is_supported_image_file(path) {
            self.add_image_file_with_mode(path, mode)
        } else if looks_like_supported_text_path(path) || file_contains_text(path)? {
            self.add_text_file(path, mode)
        } else {
            self.add_file_reference(path)
        }
    }

    pub fn add_file_reference(&mut self, path: &Path) -> anyhow::Result<bool> {
        self.add_path_reference(path)
    }

    fn add_image_file_with_mode(&mut self, path: &Path, mode: ImportMode) -> anyhow::Result<bool> {
        if mode == ImportMode::Copy {
            return self.add_path_reference(path);
        }

        let display_name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("Imported image");

        let stored = if is_gif_file(path) {
            self.persist_gif_image(path, display_name)
        } else {
            let rgba_image = image::open(path)
                .with_context(|| format!("failed to decode {}", path.display()))?
                .to_rgba8();
            self.persist_rgba_image(rgba_image, display_name)
        }?;

        if stored && should_remove_source(path, mode, &self.root) {
            remove_file_if_exists(path.to_path_buf())?;
        }

        Ok(stored)
    }

    fn add_text_file(&mut self, path: &Path, mode: ImportMode) -> anyhow::Result<bool> {
        if mode == ImportMode::Copy {
            return self.add_path_reference(path);
        }

        let encoded =
            fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        anyhow::ensure!(
            looks_like_text_bytes(&encoded),
            "{} does not look like a text file",
            path.display(),
        );

        let text = std::str::from_utf8(&encoded)
            .with_context(|| format!("{} is not valid UTF-8 text", path.display()))?;
        let hash = blake3::hash(&encoded).to_hex().to_string();

        if self.promote_existing(&hash) {
            if should_remove_source(path, mode, &self.root) {
                remove_file_if_exists(path.to_path_buf())?;
            }
            return Ok(true);
        }

        let display_name = normalize_display_name(
            path.file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("Text file"),
        );
        let file_extension = normalized_text_extension(path);
        let original_path = self
            .originals_dir
            .join(format!("{hash}.{}", persisted_text_extension(path)));
        let metadata_path = self.metadata_dir.join(format!("{hash}.txt"));

        if should_remove_source(path, mode, &self.root) {
            move_file(path, &original_path)?;
        } else {
            fs::write(&original_path, &encoded)
                .with_context(|| format!("failed to save {}", original_path.display()))?;
        }

        fs::write(&metadata_path, &display_name)
            .with_context(|| format!("failed to save {}", metadata_path.display()))?;

        self.records.insert(
            0,
            StoredImage {
                hash,
                display_name,
                width: 0,
                height: 0,
                kind: StoredImageKind::Text,
                original_path,
                thumbnail_path: None,
                reference_path: None,
                text_preview: build_text_preview(text),
                line_count: text.lines().count(),
                byte_len: encoded.len() as u64,
                file_extension,
            },
        );

        Ok(true)
    }

    fn add_path_reference(&mut self, path: &Path) -> anyhow::Result<bool> {
        anyhow::ensure!(path.is_file(), "{} is not a regular file", path.display());

        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let path_text = path.to_string_lossy().into_owned();
        let hash = format!("ref-{}", blake3::hash(path_text.as_bytes()).to_hex());
        let reference_path = self.file_refs_dir.join(format!("{hash}.path"));
        let metadata_path = self.metadata_dir.join(format!("{hash}.txt"));

        if let Some(index) = self.records.iter().position(|record| record.hash == hash) {
            if self.records[index].reference_path.is_none() {
                return Ok(self.promote_existing(&hash));
            }

            let stale_record = self.records.remove(index);
            if let Some(thumbnail_path) = stale_record.thumbnail_path {
                remove_file_if_exists(thumbnail_path)?;
            }
        }

        let mut record = self.build_reference_record(path, hash.clone())?;
        fs::write(&reference_path, path_text.as_bytes())
            .with_context(|| format!("failed to save {}", reference_path.display()))?;
        fs::write(&metadata_path, &record.display_name)
            .with_context(|| format!("failed to save {}", metadata_path.display()))?;
        record.reference_path = Some(reference_path);

        self.records.insert(0, record);
        Ok(true)
    }

    fn build_reference_record(&self, path: PathBuf, hash: String) -> anyhow::Result<StoredImage> {
        build_reference_record_from_disk(&path, &self.thumbnails_dir, &hash)
    }

    fn persist_gif_image(&mut self, path: &Path, display_name: &str) -> anyhow::Result<bool> {
        let encoded =
            fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        let hash = blake3::hash(&encoded).to_hex().to_string();

        if self.promote_existing(&hash) {
            return Ok(true);
        }

        let first_frame = image::open(path)
            .with_context(|| format!("failed to decode {}", path.display()))?
            .to_rgba8();
        let width = first_frame.width();
        let height = first_frame.height();
        let display_name = normalize_display_name(display_name);
        let original_path = self.originals_dir.join(format!("{hash}.gif"));
        let thumbnail_path = self.thumbnails_dir.join(format!("{hash}.png"));
        let metadata_path = self.metadata_dir.join(format!("{hash}.txt"));

        fs::copy(path, &original_path)
            .with_context(|| format!("failed to save {}", original_path.display()))?;

        let thumbnail = imageops::thumbnail(&first_frame, 196, 196);
        thumbnail
            .save(&thumbnail_path)
            .with_context(|| format!("failed to save {}", thumbnail_path.display()))?;
        fs::write(&metadata_path, &display_name)
            .with_context(|| format!("failed to save {}", metadata_path.display()))?;

        self.records.insert(
            0,
            StoredImage {
                hash,
                display_name,
                width,
                height,
                kind: StoredImageKind::Gif,
                original_path,
                thumbnail_path: Some(thumbnail_path),
                reference_path: None,
                text_preview: String::new(),
                line_count: 0,
                byte_len: encoded.len() as u64,
                file_extension: "gif".to_owned(),
            },
        );

        Ok(true)
    }

    fn persist_rgba_image(
        &mut self,
        rgba_image: RgbaImage,
        display_name: &str,
    ) -> anyhow::Result<bool> {
        let hash = blake3::hash(rgba_image.as_raw()).to_hex().to_string();

        if self.promote_existing(&hash) {
            return Ok(true);
        }

        let width = rgba_image.width();
        let height = rgba_image.height();
        let display_name = normalize_display_name(display_name);
        let original_path = self.originals_dir.join(format!("{hash}.png"));
        let thumbnail_path = self.thumbnails_dir.join(format!("{hash}.png"));
        let metadata_path = self.metadata_dir.join(format!("{hash}.txt"));
        rgba_image
            .save(&original_path)
            .with_context(|| format!("failed to save {}", original_path.display()))?;

        let thumbnail = imageops::thumbnail(&rgba_image, 196, 196);
        thumbnail
            .save(&thumbnail_path)
            .with_context(|| format!("failed to save {}", thumbnail_path.display()))?;
        fs::write(&metadata_path, &display_name)
            .with_context(|| format!("failed to save {}", metadata_path.display()))?;

        self.records.insert(
            0,
            StoredImage {
                hash,
                display_name,
                width,
                height,
                kind: StoredImageKind::Raster,
                original_path,
                thumbnail_path: Some(thumbnail_path),
                reference_path: None,
                text_preview: String::new(),
                line_count: 0,
                byte_len: rgba_image.as_raw().len() as u64,
                file_extension: "png".to_owned(),
            },
        );

        Ok(true)
    }

    pub fn copy_to_clipboard(&self, index: usize) -> anyhow::Result<()> {
        let record = self
            .records
            .get(index)
            .ok_or_else(|| anyhow!("image index {index} does not exist"))?;
        let mut clipboard = Clipboard::new()?;

        match record.kind {
            StoredImageKind::Text => {
                let text = fs::read_to_string(&record.original_path).with_context(|| {
                    format!("failed to open {}", record.original_path.display())
                })?;
                clipboard.set_text(text)?;
            }
            StoredImageKind::File => {
                if record.reference_path.is_some() {
                    clipboard.set().file_list(&[record.original_path.clone()])?;
                } else {
                    clipboard.set_text(record.original_path.to_string_lossy().into_owned())?;
                }
            }
            StoredImageKind::Gif | StoredImageKind::Raster => {
                if record.reference_path.is_some() {
                    clipboard.set().file_list(&[record.original_path.clone()])?;
                } else {
                    let rgba = image::open(&record.original_path)
                        .with_context(|| {
                            format!("failed to open {}", record.original_path.display())
                        })?
                        .to_rgba8();
                    clipboard.set_image(ImageData {
                        width: rgba.width() as usize,
                        height: rgba.height() as usize,
                        bytes: Cow::Owned(rgba.into_raw()),
                    })?;
                }
            }
        }

        Ok(())
    }

    pub fn copy_latest_to_clipboard(&self) -> anyhow::Result<bool> {
        if self.records.is_empty() {
            return Ok(false);
        }

        self.copy_to_clipboard(0)?;
        Ok(true)
    }

    pub fn drag_path(&self, index: usize) -> anyhow::Result<PathBuf> {
        let record = self
            .records
            .get(index)
            .ok_or_else(|| anyhow!("item index {index} does not exist"))?;
        Ok(record.original_path.clone())
    }

    pub fn reveal_in_file_manager(&self, index: usize) -> anyhow::Result<()> {
        let record = self
            .records
            .get(index)
            .ok_or_else(|| anyhow!("item index {index} does not exist"))?;
        reveal_path_in_file_manager(&record.original_path)
    }

    pub fn delete(&mut self, index: usize) -> anyhow::Result<()> {
        if index >= self.records.len() {
            return Ok(());
        }

        let record = self.records.remove(index);
        if record.reference_path.is_none() {
            remove_file_if_exists(record.original_path)?;
        }
        if let Some(thumbnail_path) = record.thumbnail_path {
            remove_file_if_exists(thumbnail_path)?;
        }
        if let Some(reference_path) = record.reference_path {
            remove_file_if_exists(reference_path)?;
        }
        remove_file_if_exists(self.metadata_dir.join(format!("{}.txt", record.hash)))?;
        Ok(())
    }

    pub fn clear(&mut self) -> anyhow::Result<()> {
        for record in self.records.drain(..) {
            if record.reference_path.is_none() {
                remove_file_if_exists(record.original_path)?;
            }
            if let Some(thumbnail_path) = record.thumbnail_path {
                remove_file_if_exists(thumbnail_path)?;
            }
            if let Some(reference_path) = record.reference_path {
                remove_file_if_exists(reference_path)?;
            }
            remove_file_if_exists(self.metadata_dir.join(format!("{}.txt", record.hash)))?;
        }
        Ok(())
    }

    pub fn records(&self) -> &[StoredImage] {
        &self.records
    }

    fn promote_existing(&mut self, hash: &str) -> bool {
        let Some(index) = self.records.iter().position(|record| record.hash == hash) else {
            return false;
        };

        if index > 0 {
            let record = self.records.remove(index);
            self.records.insert(0, record);
        }

        let reference_path = self.records[0].reference_path.clone();
        let _ = self.touch_record_recency(hash, reference_path.as_deref());

        true
    }

    fn touch_record_recency(
        &self,
        hash: &str,
        reference_path: Option<&Path>,
    ) -> anyhow::Result<()> {
        if let Some(reference_path) = reference_path {
            touch_file(reference_path)?;
        }

        let metadata_path = self.metadata_dir.join(format!("{hash}.txt"));
        if metadata_path.exists() {
            touch_file(&metadata_path)?;
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub fn root(&self) -> &PathBuf {
        &self.root
    }
}

fn remove_file_if_exists(path: PathBuf) -> anyhow::Result<()> {
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn touch_file(path: &Path) -> anyhow::Result<()> {
    let encoded = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    fs::write(path, encoded).with_context(|| format!("failed to save {}", path.display()))?;
    Ok(())
}

fn move_file(from: &Path, to: &Path) -> anyhow::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(from, to).with_context(|| format!("failed to save {}", to.display()))?;
            remove_file_if_exists(from.to_path_buf())?;
            Ok(())
        }
    }
}

fn reveal_path_in_file_manager(path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let status = Command::new("open")
            .arg("-R")
            .arg(path)
            .status()
            .with_context(|| format!("failed to reveal {}", path.display()))?;
        anyhow::ensure!(status.success(), "failed to reveal {}", path.display());
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let status = Command::new("explorer")
            .arg("/select,")
            .arg(path)
            .status()
            .with_context(|| format!("failed to reveal {}", path.display()))?;
        anyhow::ensure!(status.success(), "failed to reveal {}", path.display());
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target_dir = path.parent().unwrap_or(path);
        let status = Command::new("xdg-open")
            .arg(target_dir)
            .status()
            .with_context(|| format!("failed to open {}", target_dir.display()))?;
        anyhow::ensure!(status.success(), "failed to open {}", target_dir.display());
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err(anyhow!("revealing files is not supported on this platform"))
}

fn record_recency(primary: Option<PathBuf>, fallback: PathBuf) -> SystemTime {
    primary
        .as_deref()
        .and_then(file_modified_time)
        .or_else(|| file_modified_time(&fallback))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn file_modified_time(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

fn should_remove_source(path: &Path, mode: ImportMode, store_root: &Path) -> bool {
    mode == ImportMode::Move && !path.starts_with(store_root)
}

fn build_reference_record_from_disk(
    path: &Path,
    thumbnails_dir: &Path,
    hash: &str,
) -> anyhow::Result<StoredImage> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to read metadata for {}", path.display()))?;
    let kind = classify_file_kind(path)?;

    match kind {
        StoredImageKind::Gif | StoredImageKind::Raster => {
            let rgba = image::open(path)
                .with_context(|| format!("failed to decode {}", path.display()))?
                .to_rgba8();
            let thumbnail_path = thumbnails_dir.join(format!("{hash}.png"));
            imageops::thumbnail(&rgba, 196, 196)
                .save(&thumbnail_path)
                .with_context(|| format!("failed to save {}", thumbnail_path.display()))?;

            Ok(StoredImage {
                hash: hash.to_owned(),
                display_name: normalize_display_name(
                    path.file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Imported image"),
                ),
                width: rgba.width(),
                height: rgba.height(),
                kind,
                original_path: path.to_path_buf(),
                thumbnail_path: Some(thumbnail_path),
                reference_path: None,
                text_preview: String::new(),
                line_count: 0,
                byte_len: metadata.len(),
                file_extension: path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.to_ascii_lowercase())
                    .unwrap_or_else(|| "png".to_owned()),
            })
        }
        StoredImageKind::Text => {
            let encoded =
                fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
            anyhow::ensure!(
                looks_like_text_bytes(&encoded),
                "{} does not look like a text file",
                path.display(),
            );
            let text = std::str::from_utf8(&encoded)
                .with_context(|| format!("{} is not valid UTF-8 text", path.display()))?;

            Ok(StoredImage {
                hash: hash.to_owned(),
                display_name: normalize_display_name(
                    path.file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Text file"),
                ),
                width: 0,
                height: 0,
                kind,
                original_path: path.to_path_buf(),
                thumbnail_path: None,
                reference_path: None,
                text_preview: build_text_preview(text),
                line_count: text.lines().count(),
                byte_len: encoded.len() as u64,
                file_extension: normalized_text_extension(path),
            })
        }
        StoredImageKind::File => {
            let display_name = normalize_display_name(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("File"),
            );
            let file_extension = normalized_file_extension(path, &display_name);

            Ok(StoredImage {
                hash: hash.to_owned(),
                display_name,
                width: 0,
                height: 0,
                kind,
                original_path: path.to_path_buf(),
                thumbnail_path: None,
                reference_path: None,
                text_preview: String::new(),
                line_count: 0,
                byte_len: metadata.len(),
                file_extension,
            })
        }
    }
}

fn classify_file_kind(path: &Path) -> anyhow::Result<StoredImageKind> {
    if is_gif_file(path) {
        Ok(StoredImageKind::Gif)
    } else if is_supported_image_file(path) {
        Ok(StoredImageKind::Raster)
    } else if looks_like_supported_text_path(path) || file_contains_text(path)? {
        Ok(StoredImageKind::Text)
    } else {
        Ok(StoredImageKind::File)
    }
}

fn extract_existing_paths_from_clipboard_text(text: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for line in text.lines() {
        let Some(candidate) = parse_clipboard_path_candidate(line) else {
            continue;
        };
        let path = candidate.canonicalize().unwrap_or(candidate);
        if path.is_file() && !paths.contains(&path) {
            paths.push(path);
        }
    }

    paths
}

fn parse_clipboard_path_candidate(text: &str) -> Option<PathBuf> {
    let candidate = text.trim().trim_matches(|ch| matches!(ch, '"' | '\''));
    if candidate.is_empty() {
        return None;
    }

    let candidate = candidate.strip_prefix("file://").unwrap_or(candidate);

    #[cfg(target_os = "windows")]
    let candidate = candidate
        .strip_prefix('/')
        .filter(|trimmed| trimmed.as_bytes().get(1) == Some(&b':'))
        .unwrap_or(candidate);

    Some(PathBuf::from(candidate))
}

fn load_records(
    originals_dir: &Path,
    thumbnails_dir: &Path,
    metadata_dir: &Path,
    file_refs_dir: &Path,
) -> anyhow::Result<Vec<StoredImage>> {
    let mut records = Vec::new();

    for entry in fs::read_dir(originals_dir)
        .with_context(|| format!("failed to read {}", originals_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if !entry.file_type()?.is_file() {
            continue;
        }

        match load_record(&path, thumbnails_dir, metadata_dir) {
            Ok(Some(record)) => records.push(record),
            Ok(None) => {}
            Err(err) => eprintln!("skipping stored image {}: {err:#}", path.display()),
        }
    }

    for entry in fs::read_dir(file_refs_dir)
        .with_context(|| format!("failed to read {}", file_refs_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if !entry.file_type()?.is_file() {
            continue;
        }

        match load_reference_record(&path, thumbnails_dir, metadata_dir) {
            Ok(Some(record)) => records.push(record),
            Ok(None) => {}
            Err(err) => eprintln!("skipping file reference {}: {err:#}", path.display()),
        }
    }

    records.sort_by(
        |(left_modified, left_record), (right_modified, right_record)| {
            right_modified
                .cmp(left_modified)
                .then_with(|| right_record.hash.cmp(&left_record.hash))
        },
    );

    Ok(records.into_iter().map(|(_, record)| record).collect())
}

fn load_record(
    original_path: &Path,
    thumbnails_dir: &Path,
    metadata_dir: &Path,
) -> anyhow::Result<Option<(SystemTime, StoredImage)>> {
    let Some(hash) = original_path.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(None);
    };

    if is_text_file(original_path) {
        return load_text_record(hash, original_path, metadata_dir);
    }

    let rgba = image::open(original_path)
        .with_context(|| format!("failed to decode {}", original_path.display()))?
        .to_rgba8();
    let thumbnail_path = thumbnails_dir.join(format!("{hash}.png"));
    let display_name = load_display_name(metadata_dir.join(format!("{hash}.txt")), hash);
    let kind = if is_gif_file(original_path) {
        StoredImageKind::Gif
    } else {
        StoredImageKind::Raster
    };

    if !thumbnail_path.exists() {
        imageops::thumbnail(&rgba, 196, 196)
            .save(&thumbnail_path)
            .with_context(|| format!("failed to save {}", thumbnail_path.display()))?;
    }

    let modified = record_recency(
        Some(metadata_dir.join(format!("{hash}.txt"))),
        original_path.to_path_buf(),
    );

    Ok(Some((
        modified,
        StoredImage {
            hash: hash.to_owned(),
            display_name,
            width: rgba.width(),
            height: rgba.height(),
            kind,
            original_path: original_path.to_path_buf(),
            thumbnail_path: Some(thumbnail_path),
            reference_path: None,
            text_preview: String::new(),
            line_count: 0,
            byte_len: fs::metadata(original_path)
                .map(|metadata| metadata.len())
                .unwrap_or_default(),
            file_extension: original_path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.to_ascii_lowercase())
                .unwrap_or_else(|| "png".to_owned()),
        },
    )))
}

fn load_text_record(
    hash: &str,
    original_path: &Path,
    metadata_dir: &Path,
) -> anyhow::Result<Option<(SystemTime, StoredImage)>> {
    let encoded = fs::read(original_path)
        .with_context(|| format!("failed to read {}", original_path.display()))?;
    anyhow::ensure!(
        looks_like_text_bytes(&encoded),
        "{} is not a supported text item",
        original_path.display(),
    );
    let text = std::str::from_utf8(&encoded)
        .with_context(|| format!("{} is not valid UTF-8 text", original_path.display()))?;
    let modified = record_recency(
        Some(metadata_dir.join(format!("{hash}.txt"))),
        original_path.to_path_buf(),
    );

    Ok(Some((
        modified,
        StoredImage {
            hash: hash.to_owned(),
            display_name: load_display_name(metadata_dir.join(format!("{hash}.txt")), hash),
            width: 0,
            height: 0,
            kind: StoredImageKind::Text,
            original_path: original_path.to_path_buf(),
            thumbnail_path: None,
            reference_path: None,
            text_preview: build_text_preview(text),
            line_count: text.lines().count(),
            byte_len: encoded.len() as u64,
            file_extension: normalized_text_extension(original_path),
        },
    )))
}

fn load_reference_record(
    reference_path: &Path,
    thumbnails_dir: &Path,
    metadata_dir: &Path,
) -> anyhow::Result<Option<(SystemTime, StoredImage)>> {
    let Some(hash) = reference_path.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(None);
    };
    let path_text = fs::read_to_string(reference_path)
        .with_context(|| format!("failed to read {}", reference_path.display()))?;
    let original_path = PathBuf::from(path_text);
    if !original_path.is_file() {
        return Ok(None);
    }

    let mut record = build_reference_record_from_disk(&original_path, thumbnails_dir, hash)?;
    record.display_name = load_display_name(metadata_dir.join(format!("{hash}.txt")), hash);
    record.reference_path = Some(reference_path.to_path_buf());
    let modified = record_recency(None, reference_path.to_path_buf());

    Ok(Some((modified, record)))
}

fn is_supported_image_file(path: &Path) -> bool {
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

fn is_png_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("png")),
        Some(true)
    )
}

fn is_gif_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("gif")),
        Some(true)
    )
}

fn is_text_file(path: &Path) -> bool {
    !is_png_file(path) && !is_gif_file(path)
}

fn looks_like_supported_text_path(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    if matches!(
        file_name,
        "Dockerfile"
            | "Makefile"
            | "Justfile"
            | "Procfile"
            | "CMakeLists.txt"
            | ".env"
            | ".gitignore"
            | ".gitattributes"
    ) {
        return true;
    }

    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase()),
        Some(ext)
            if matches!(
                ext.as_str(),
                "txt"
                    | "md"
                    | "markdown"
                    | "rst"
                    | "log"
                    | "csv"
                    | "tsv"
                    | "json"
                    | "jsonc"
                    | "yaml"
                    | "yml"
                    | "toml"
                    | "ini"
                    | "cfg"
                    | "conf"
                    | "xml"
                    | "html"
                    | "htm"
                    | "css"
                    | "scss"
                    | "less"
                    | "js"
                    | "jsx"
                    | "ts"
                    | "tsx"
                    | "mjs"
                    | "cjs"
                    | "py"
                    | "rs"
                    | "c"
                    | "h"
                    | "cpp"
                    | "cc"
                    | "cxx"
                    | "hpp"
                    | "java"
                    | "kt"
                    | "kts"
                    | "swift"
                    | "go"
                    | "rb"
                    | "php"
                    | "sh"
                    | "bash"
                    | "zsh"
                    | "fish"
                    | "sql"
                    | "graphql"
                    | "gql"
                    | "proto"
                    | "env"
                    | "gitignore"
            )
    )
}

fn file_contains_text(path: &Path) -> anyhow::Result<bool> {
    let encoded = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(looks_like_text_bytes(&encoded))
}

fn looks_like_text_bytes(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

fn normalized_text_extension(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .filter(|ext| !ext.is_empty())
        .unwrap_or_else(|| "text".to_owned())
}

fn normalized_file_extension(path: &Path, display_name: &str) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .filter(|ext| !ext.is_empty())
        .unwrap_or_else(|| normalized_file_extension_from_name(display_name))
}

fn normalized_file_extension_from_name(display_name: &str) -> String {
    Path::new(display_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .filter(|ext| !ext.is_empty())
        .unwrap_or_else(|| "file".to_owned())
}

fn persisted_text_extension(path: &Path) -> String {
    normalized_text_extension(path)
}

fn build_text_preview(text: &str) -> String {
    let mut preview = String::new();

    for line in text
        .lines()
        .map(compact_text_preview_line)
        .filter(|line| !line.is_empty())
        .take(8)
    {
        if !preview.is_empty() {
            preview.push('\n');
        }
        preview.push_str(&line);
    }

    if preview.is_empty() {
        "Empty file".to_owned()
    } else {
        preview
    }
}

fn compact_text_preview_line(line: &str) -> String {
    let normalized = line.trim_end().replace('\t', "    ");
    let mut chars = normalized.chars();
    let mut clipped = chars.by_ref().take(88).collect::<String>();

    if chars.next().is_some() {
        clipped.push_str("...");
    }

    clipped
}

fn load_display_name(metadata_path: PathBuf, hash: &str) -> String {
    fs::read_to_string(&metadata_path)
        .ok()
        .map(|name| normalize_display_name(name.trim()))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("Item {}", short_display_id(hash)))
}

fn normalize_display_name(name: &str) -> String {
    let trimmed = name.trim();

    if trimmed.is_empty() {
        "Item".to_owned()
    } else {
        trimmed.to_owned()
    }
}

pub fn short_display_id(hash: &str) -> String {
    hash.chars().take(8).collect::<String>().to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Delay, Frame, Rgba};
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, UNIX_EPOCH},
    };

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_store_root() -> PathBuf {
        let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "pictray-test-{}-{test_id}",
            UNIX_EPOCH.elapsed().unwrap().as_nanos(),
        ))
    }

    fn write_test_gif(path: &Path) -> anyhow::Result<()> {
        let file = fs::File::create(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        let mut encoder = image::codecs::gif::GifEncoder::new(file);
        let frames = vec![
            Frame::from_parts(
                RgbaImage::from_pixel(2, 2, Rgba([255, 0, 0, 255])),
                0,
                0,
                Delay::from_numer_denom_ms(80, 1),
            ),
            Frame::from_parts(
                RgbaImage::from_pixel(2, 2, Rgba([0, 0, 255, 255])),
                0,
                0,
                Delay::from_numer_denom_ms(80, 1),
            ),
        ];

        encoder.encode_frames(frames)?;
        Ok(())
    }

    fn write_text_file(path: &Path, contents: &str) -> anyhow::Result<()> {
        fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))
    }

    #[test]
    fn reloads_saved_images_on_open() {
        let root = temp_store_root();
        let result = (|| -> anyhow::Result<()> {
            let mut store = ImageStore::open_at(root.clone())?;
            let first = RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
            let second = RgbaImage::from_pixel(3, 1, image::Rgba([0, 255, 0, 255]));

            assert!(store.persist_rgba_image(first, "First image")?);
            std::thread::sleep(Duration::from_millis(20));
            assert!(store.persist_rgba_image(second, "Second image")?);

            let reopened = ImageStore::open_at(root.clone())?;
            assert_eq!(reopened.records.len(), 2);
            assert_eq!(
                (reopened.records[0].width, reopened.records[0].height),
                (3, 1)
            );
            assert_eq!(reopened.records[0].display_name, "Second image");
            assert_eq!(
                (reopened.records[1].width, reopened.records[1].height),
                (2, 2)
            );
            assert_eq!(reopened.records[1].display_name, "First image");

            Ok(())
        })();

        let _ = fs::remove_dir_all(&root);
        result.unwrap();
    }

    #[test]
    fn readding_existing_image_moves_it_to_front() {
        let root = temp_store_root();
        let result = (|| -> anyhow::Result<()> {
            let mut store = ImageStore::open_at(root.clone())?;
            let first = RgbaImage::from_pixel(4, 4, image::Rgba([12, 34, 56, 255]));
            let second = RgbaImage::from_pixel(5, 5, image::Rgba([78, 90, 123, 255]));

            assert!(store.persist_rgba_image(first.clone(), "First")?);
            assert!(store.persist_rgba_image(second, "Second")?);
            assert_eq!(store.records[0].display_name, "Second");
            assert_eq!(store.records[1].display_name, "First");

            assert!(store.persist_rgba_image(first, "First")?);
            assert_eq!(store.records.len(), 2);
            assert_eq!(store.records[0].display_name, "First");
            assert_eq!(store.records[1].display_name, "Second");

            Ok(())
        })();

        let _ = fs::remove_dir_all(&root);
        result.unwrap();
    }

    #[test]
    fn imported_gif_is_preserved_and_reloaded() {
        let root = temp_store_root();
        let result = (|| -> anyhow::Result<()> {
            let input_gif = root.join("sample.gif");
            fs::create_dir_all(&root)?;
            write_test_gif(&input_gif)?;
            let canonical_input = input_gif
                .canonicalize()
                .unwrap_or_else(|_| input_gif.clone());

            let mut store = ImageStore::open_at(root.join("store"))?;
            assert!(store.add_image_file(&input_gif)?);
            assert_eq!(store.records.len(), 1);
            assert_eq!(store.records[0].kind, StoredImageKind::Gif);
            assert_eq!(store.records[0].original_path, canonical_input);
            assert!(store.records[0].reference_path.is_some());
            assert_eq!(
                store.records[0]
                    .original_path
                    .extension()
                    .and_then(|ext| ext.to_str()),
                Some("gif")
            );

            let reopened = ImageStore::open_at(root.join("store"))?;
            assert_eq!(reopened.records.len(), 1);
            assert_eq!(reopened.records[0].kind, StoredImageKind::Gif);
            assert_eq!(reopened.records[0].original_path, canonical_input);
            assert!(reopened.records[0].reference_path.is_some());
            assert_eq!(
                reopened.records[0]
                    .original_path
                    .extension()
                    .and_then(|ext| ext.to_str()),
                Some("gif")
            );

            Ok(())
        })();

        let _ = fs::remove_dir_all(&root);
        result.unwrap();
    }

    #[test]
    fn imported_text_file_is_preserved_and_reloaded() {
        let root = temp_store_root();
        let result = (|| -> anyhow::Result<()> {
            let input_file = root.join("snippet.rs");
            fs::create_dir_all(&root)?;
            write_text_file(
                &input_file,
                "fn main() {\n    println!(\"hello from pictray\");\n}\n",
            )?;
            let canonical_input = input_file
                .canonicalize()
                .unwrap_or_else(|_| input_file.clone());

            let mut store = ImageStore::open_at(root.join("store"))?;
            assert!(store.import_path(&input_file, ImportMode::Copy)?);
            assert_eq!(store.records.len(), 1);
            assert_eq!(store.records[0].kind, StoredImageKind::Text);
            assert_eq!(store.records[0].file_extension, "rs");
            assert!(store.records[0].thumbnail_path.is_none());
            assert_eq!(store.records[0].original_path, canonical_input);
            assert!(store.records[0].reference_path.is_some());
            assert!(store.records[0].text_preview.contains("println!"));
            assert!(input_file.exists());

            let reopened = ImageStore::open_at(root.join("store"))?;
            assert_eq!(reopened.records.len(), 1);
            assert_eq!(reopened.records[0].kind, StoredImageKind::Text);
            assert_eq!(reopened.records[0].file_extension, "rs");
            assert_eq!(reopened.records[0].original_path, canonical_input);
            assert!(reopened.records[0].reference_path.is_some());
            assert!(reopened.records[0].text_preview.contains("println!"));

            Ok(())
        })();

        let _ = fs::remove_dir_all(&root);
        result.unwrap();
    }

    #[test]
    fn generic_file_reference_is_preserved_without_copying_source() {
        let root = temp_store_root();
        let result = (|| -> anyhow::Result<()> {
            let input_file = root.join("archive.bin");
            fs::create_dir_all(&root)?;
            fs::write(&input_file, [0, 159, 146, 150, 255])?;

            let mut store = ImageStore::open_at(root.join("store"))?;
            assert!(store.add_file_reference(&input_file)?);
            assert_eq!(store.records.len(), 1);
            assert_eq!(store.records[0].kind, StoredImageKind::File);
            assert_eq!(store.records[0].file_extension, "bin");
            assert_eq!(store.records[0].byte_len, 5);
            assert!(input_file.exists());

            let reference_path = store.records[0]
                .reference_path
                .clone()
                .expect("file references store a pointer file");
            assert!(reference_path.exists());

            let mut reopened = ImageStore::open_at(root.join("store"))?;
            assert_eq!(reopened.records.len(), 1);
            assert_eq!(reopened.records[0].kind, StoredImageKind::File);
            assert_eq!(reopened.records[0].display_name, "archive.bin");

            reopened.delete(0)?;
            assert!(input_file.exists());
            assert!(!reference_path.exists());

            Ok(())
        })();

        let _ = fs::remove_dir_all(&root);
        result.unwrap();
    }

    #[test]
    fn moving_text_file_removes_source_after_buffering() {
        let root = temp_store_root();
        let result = (|| -> anyhow::Result<()> {
            let input_file = root.join("todo.txt");
            fs::create_dir_all(&root)?;
            write_text_file(&input_file, "ship move support\nship copy support\n")?;

            let mut store = ImageStore::open_at(root.join("store"))?;
            assert!(store.import_path(&input_file, ImportMode::Move)?);
            assert_eq!(store.records.len(), 1);
            assert_eq!(store.records[0].kind, StoredImageKind::Text);
            assert!(!input_file.exists());
            assert!(store.records[0].original_path.exists());

            Ok(())
        })();

        let _ = fs::remove_dir_all(&root);
        result.unwrap();
    }
}
