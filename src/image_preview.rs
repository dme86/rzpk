//! Terminal image previews with native graphics when the terminal supports it
//! and Unicode half-blocks everywhere else.
//!
//! A small decoded image is kept while selected. Resize + protocol encoding
//! happens on a worker thread, not on the ratatui draw/event thread.
use std::{
    fs, io,
    path::Path,
    sync::{OnceLock, mpsc::{self, Receiver}},
    thread,
};

use image::GenericImageView;
use ratatui_image::{
    picker::Picker,
    thread::{ResizeResponse, ThreadProtocol},
};

const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 12_000_000;

static PICKER: OnceLock<Picker> = OnceLock::new();

/// Call once after entering the terminal's alternate screen and before
/// starting the event loop. An unsupported terminal automatically falls back
/// to the cross-platform Unicode half-block renderer.
pub fn init_terminal_picker() {
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let _ = PICKER.set(picker);
}

fn picker() -> &'static Picker {
    PICKER.get_or_init(Picker::halfblocks)
}

pub fn supports_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
            )
        })
}

/// Owns the per-preview image protocol and its dedicated encoding worker.
/// On drop, the request sender closes and the worker naturally exits.
pub struct ImagePreview {
    pub dimensions: (u32, u32),
    pub format: &'static str,
    pub protocol: ThreadProtocol,
    completed: Receiver<ResizeResponse>,
}

impl ImagePreview {
    pub fn open(path: &Path) -> io::Result<Self> {
        let metadata = fs::metadata(path)?;
        if metadata.len() > MAX_IMAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "image exceeds 20 MiB preview limit",
            ));
        }
        let (width, height) = image::image_dimensions(path)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "image exceeds 12-megapixel preview limit",
            ));
        }
        let decoded = image::open(path)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let dimensions = decoded.dimensions();
        let (request_tx, request_rx) = mpsc::channel();
        let (completed_tx, completed_rx) = mpsc::channel();
        thread::Builder::new()
            .name("rzpk-image-encode".into())
            .spawn(move || {
                while let Ok(request) = request_rx.recv() {
                    if let Ok(response) = request.resize_encode()
                        && completed_tx.send(response).is_err()
                    {
                        break;
                    }
                }
            })?;

        let protocol = ThreadProtocol::new(request_tx, Some(picker().new_resize_protocol(decoded)));
        let format = path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("image");
        let format = match format.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => "JPEG",
            "png" => "PNG",
            "gif" => "GIF",
            "webp" => "WebP",
            "bmp" => "BMP",
            _ => "Image",
        };
        Ok(Self {
            dimensions,
            format,
            protocol,
            completed: completed_rx,
        })
    }

    /// Consume completed encodes without blocking the UI.
    /// Must run between frame draws to avoid borrow conflicts.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(result) = self.completed.try_recv() {
            changed |= self.protocol.update_resized_protocol(result);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn path() -> std::path::PathBuf {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("rzpk-image-{suffix}-{}.png", std::process::id()))
    }

    #[test]
    fn recognizes_common_image_extensions_without_touching_source() {
        for extension in ["PNG", "jpg", "jpeg", "gif", "WEBP", "bmp"] {
            assert!(supports_path(Path::new(&format!("image.{extension}"))));
        }
        for extension in ["svg", "md", "rs", "txt"] {
            assert!(!supports_path(Path::new(&format!("image.{extension}"))));
        }
    }

    #[test]
    fn opens_small_png_in_halfblock_mode() -> io::Result<()> {
        let file = path();
        let image = ImageBuffer::from_pixel(4, 3, Rgb([184u8, 187, 38]));
        image.save(&file).map_err(io::Error::other)?;
        let preview = ImagePreview::open(&file)?;
        assert_eq!(preview.dimensions, (4, 3));
        assert_eq!(preview.format, "PNG");
        fs::remove_file(file)?;
        Ok(())
    }

    #[test]
    fn oversized_or_invalid_images_report_errors() -> io::Result<()> {
        let file = path();
        fs::write(&file, b"not really a png")?;
        assert!(ImagePreview::open(&file).is_err());
        fs::remove_file(&file)?;
        let large = vec![0u8; MAX_IMAGE_BYTES as usize + 1];
        fs::write(&file, large)?;
        assert!(ImagePreview::open(&file).is_err());
        fs::remove_file(file)?;
        Ok(())
    }
}
