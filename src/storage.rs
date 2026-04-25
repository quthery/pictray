use std::{
    borrow::Cow,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, anyhow};
use arboard::{Clipboard, ImageData};
use image::{RgbaImage, imageops};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredImageKind {
    Raster,
    Gif,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredImage {
    pub hash: String,
    pub display_name: String,
    pub width: u32,
    pub height: u32,
    pub kind: StoredImageKind,
    pub original_path: PathBuf,
    pub thumbnail_path: PathBuf,
}

impl StoredImage {
    pub fn preview_rgba(&self) -> anyhow::Result<(u32, u32, Vec<u8>)> {
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

        fs::create_dir_all(&originals_dir)?;
        fs::create_dir_all(&thumbnails_dir)?;
        fs::create_dir_all(&metadata_dir)?;

        let records = load_records(&originals_dir, &thumbnails_dir, &metadata_dir)?;

        Ok(Self {
            root,
            originals_dir,
            thumbnails_dir,
            metadata_dir,
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

    pub fn add_current_clipboard_image(&mut self) -> anyhow::Result<bool> {
        let mut clipboard = Clipboard::new()?;
        let image = match clipboard.get_image() {
            Ok(image) => image,
            Err(_) => return Ok(false),
        };

        self.add_clipboard_image(image)
    }

    pub fn add_image_file(&mut self, path: &Path) -> anyhow::Result<bool> {
        let display_name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("Imported image");

        if is_gif_file(path) {
            self.persist_gif_image(path, display_name)
        } else {
            let rgba_image = image::open(path)
                .with_context(|| format!("failed to decode {}", path.display()))?
                .to_rgba8();
            self.persist_rgba_image(rgba_image, display_name)
        }
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
                thumbnail_path,
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
                thumbnail_path,
            },
        );

        Ok(true)
    }

    pub fn copy_to_clipboard(&self, index: usize) -> anyhow::Result<()> {
        let record = self
            .records
            .get(index)
            .ok_or_else(|| anyhow!("image index {index} does not exist"))?;
        let rgba = image::open(&record.original_path)
            .with_context(|| format!("failed to open {}", record.original_path.display()))?
            .to_rgba8();

        let mut clipboard = Clipboard::new()?;
        clipboard.set_image(ImageData {
            width: rgba.width() as usize,
            height: rgba.height() as usize,
            bytes: Cow::Owned(rgba.into_raw()),
        })?;

        Ok(())
    }

    pub fn copy_latest_to_clipboard(&self) -> anyhow::Result<bool> {
        if self.records.is_empty() {
            return Ok(false);
        }

        self.copy_to_clipboard(0)?;
        Ok(true)
    }

    pub fn delete(&mut self, index: usize) -> anyhow::Result<()> {
        if index >= self.records.len() {
            return Ok(());
        }

        let record = self.records.remove(index);
        remove_file_if_exists(record.original_path)?;
        remove_file_if_exists(record.thumbnail_path)?;
        remove_file_if_exists(self.metadata_dir.join(format!("{}.txt", record.hash)))?;
        Ok(())
    }

    pub fn clear(&mut self) -> anyhow::Result<()> {
        for record in self.records.drain(..) {
            remove_file_if_exists(record.original_path)?;
            remove_file_if_exists(record.thumbnail_path)?;
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

        true
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

pub fn image_fingerprint(width: usize, height: usize, bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&width.to_le_bytes());
    hasher.update(&height.to_le_bytes());
    hasher.update(bytes);
    hasher.finalize().to_hex().to_string()
}

fn load_records(
    originals_dir: &Path,
    thumbnails_dir: &Path,
    metadata_dir: &Path,
) -> anyhow::Result<Vec<StoredImage>> {
    let mut records = Vec::new();

    for entry in fs::read_dir(originals_dir)
        .with_context(|| format!("failed to read {}", originals_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if !entry.file_type()?.is_file() || !is_supported_original_file(&path) {
            continue;
        }

        match load_record(&path, thumbnails_dir, metadata_dir) {
            Ok(Some(record)) => records.push(record),
            Ok(None) => {}
            Err(err) => eprintln!("skipping stored image {}: {err:#}", path.display()),
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

    let modified = fs::metadata(original_path)
        .with_context(|| format!("failed to read metadata for {}", original_path.display()))?
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH);

    Ok(Some((
        modified,
        StoredImage {
            hash: hash.to_owned(),
            display_name,
            width: rgba.width(),
            height: rgba.height(),
            kind,
            original_path: original_path.to_path_buf(),
            thumbnail_path,
        },
    )))
}

fn is_supported_original_file(path: &Path) -> bool {
    is_png_file(path) || is_gif_file(path)
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

fn load_display_name(metadata_path: PathBuf, hash: &str) -> String {
    fs::read_to_string(&metadata_path)
        .ok()
        .map(|name| normalize_display_name(name.trim()))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("Image {}", short_display_id(hash)))
}

fn normalize_display_name(name: &str) -> String {
    let trimmed = name.trim();

    if trimmed.is_empty() {
        "Image".to_owned()
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

            let mut store = ImageStore::open_at(root.join("store"))?;
            assert!(store.add_image_file(&input_gif)?);
            assert_eq!(store.records.len(), 1);
            assert_eq!(store.records[0].kind, StoredImageKind::Gif);
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
}
