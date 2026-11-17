//! Files and images going into a chat message, ported from `src/main/chat/attach.ts` and
//! `images.ts`. Picked files: images are read and sent as images (the paths came from the
//! app's own dialog), everything else goes on as a path. Pasted images for CLIs that read them
//! from a file (agy's `@path`, codex's localImage) are written to a private temp folder; files
//! older than a day go the next time.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use crate::json::random_token;
use crate::types::{
    ChatImage, ChatImageType, ChatPickResult, CHAT_FILES_MAX, CHAT_IMAGES_MAX, CHAT_IMAGE_MAX,
};

/// The extension of a path, lower case with the dot (`path.extname(p).toLowerCase()`).
fn ext_of(p: &Path) -> String {
    p.extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}

/// What the attach picker gave, split into images to send as images and paths.
pub fn read_picked(paths: &[PathBuf]) -> ChatPickResult {
    let mut out = ChatPickResult::default();
    for p in paths {
        let ty = ChatImageType::from_ext(&ext_of(p));
        // Base64 is 4/3 of the size: a bigger image goes as a path, claude can still read it.
        let fits = ty.is_some()
            && out.images.len() < CHAT_IMAGES_MAX
            && fs::metadata(p)
                .map(|m| m.len().div_ceil(3) * 4 <= CHAT_IMAGE_MAX as u64)
                .unwrap_or(false);
        let image = match (ty, fits) {
            (Some(media_type), true) => fs::read(p).ok().map(|bytes| ChatImage {
                media_type,
                data: STANDARD.encode(bytes),
            }),
            _ => None,
        };
        match image {
            Some(img) => out.images.push(img),
            None if out.files.len() < CHAT_FILES_MAX => {
                out.files.push(p.to_string_lossy().into_owned())
            }
            None => {}
        }
    }
    out
}

const KEEP_MS: i64 = 24 * 60 * 60 * 1000;

/// The default folder for [`save_chat_images`].
pub fn chat_images_dir() -> PathBuf {
    std::env::temp_dir().join("wraithgrid-chat-images")
}

/// Writes each image to a file in `dir` and returns their paths, in order. `now` is ms.
pub fn save_chat_images(images: &[ChatImage], dir: &Path, now: i64) -> io::Result<Vec<PathBuf>> {
    if images.is_empty() {
        return Ok(Vec::new());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    for entry in fs::read_dir(dir)?.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|d| now - d.as_millis() as i64 > KEEP_MS);
        if old {
            // Gone already is fine.
            let _ = fs::remove_file(entry.path());
        }
    }
    images
        .iter()
        .enumerate()
        .map(|(i, img)| {
            let ext = match img.media_type {
                ChatImageType::Png => ".png",
                ChatImageType::Jpeg => ".jpg",
                ChatImageType::Gif => ".gif",
                ChatImageType::Webp => ".webp",
            };
            let file = dir.join(format!("img-{now}-{i}-{}{ext}", random_token()));
            let bytes = STANDARD
                .decode(&img.data)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            write_private(&file, &bytes)?;
            Ok(file)
        })
        .collect()
}

/// Writes a file only its owner can read.
fn write_private(file: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    io::Write::write_all(&mut opts.open(file)?, bytes)
}

/// Longest side of a sent image's thumbnail, in pixels.
const THUMB_SIDE: u32 = 320;

/// A sent image's thumbnail as a data URL for the saved view (#50): scaled to fit
/// [`THUMB_SIDE`], as PNG; None when it doesn't decode or comes out too big to keep.
pub fn thumb_data_url(img: &ChatImage) -> Option<String> {
    let bytes = STANDARD.decode(&img.data).ok()?;
    let decoded = image::load_from_memory(&bytes).ok()?;
    let small = if decoded.width() > THUMB_SIDE || decoded.height() > THUMB_SIDE {
        decoded.thumbnail(THUMB_SIDE, THUMB_SIDE)
    } else {
        decoded
    };
    let mut out = io::Cursor::new(Vec::new());
    small.write_to(&mut out, image::ImageFormat::Png).ok()?;
    let url = format!(
        "data:image/png;base64,{}",
        STANDARD.encode(out.into_inner())
    );
    crate::thread::valid_thumbs(&serde_json::json!([url.clone()])).map(|_| url)
}

#[cfg(test)]
mod thumb_tests {
    use super::*;

    #[test]
    fn thumbnails_fit_and_are_kept() {
        let big = image::RgbImage::from_pixel(800, 400, image::Rgb([200, 30, 30]));
        let mut png = io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(big)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let img = ChatImage {
            media_type: ChatImageType::Png,
            data: STANDARD.encode(png.into_inner()),
        };
        let url = thumb_data_url(&img).expect("a thumbnail");
        let data = STANDARD
            .decode(url.trim_start_matches("data:image/png;base64,"))
            .unwrap();
        let small = image::load_from_memory(&data).unwrap();
        assert_eq!((small.width(), small.height()), (320, 160));
        assert!(thumb_data_url(&ChatImage {
            media_type: ChatImageType::Png,
            data: "bm90IGFuIGltYWdl".into()
        })
        .is_none());
    }
}
