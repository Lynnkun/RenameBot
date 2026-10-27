//! Any common image (the logo file, or a photo / image sent by the admin) -> a Telegram thumbnail:
//! JPEG, at most 320x320 px and 200 KB.

use std::path::Path;

use anyhow::{bail, Context, Result};
use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, ExtendedColorType, ImageEncoder};

const MAX_SIDE: u32 = 320;
const MAX_BYTES: usize = 200_000;

pub fn load(path: &Path) -> Result<Vec<u8>> {
    let cwd = std::env::current_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    let raw = std::fs::read(path).with_context(|| {
        format!(
            "cannot read the logo at {} (the bot was started from {cwd})",
            path.display()
        )
    })?;
    from_bytes(&raw).with_context(|| format!("the logo at {} cannot be used", path.display()))
}

/// Raw bytes of an image (png, jpeg, webp, gif, bmp, tiff - the format is detected from the content, not the name)
/// -> a Telegram-ready JPEG thumbnail. Animated images use their first frame.
pub fn from_bytes(raw: &[u8]) -> Result<Vec<u8>> {
    let mut img = image::load_from_memory(raw)
        .context("this is not an image I can read (use png, jpg, webp, gif, bmp or tiff)")?;
    if img.width() > MAX_SIDE || img.height() > MAX_SIDE {
        img = img.resize(MAX_SIDE, MAX_SIDE, FilterType::Lanczos3); // keeps the aspect ratio
    }

    // JPEG has no transparency: flatten the PNG onto black.
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for p in rgba.pixels() {
        let alpha = p[3] as u32;
        for c in 0..3 {
            rgb.push(((p[c] as u32 * alpha) / 255) as u8);
        }
    }

    for quality in [90u8, 80, 70, 60, 50, 40] {
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, quality).write_image(
            &rgb,
            w,
            h,
            ExtendedColorType::Rgb8,
        )?;
        if out.len() <= MAX_BYTES {
            return Ok(out);
        }
    }
    bail!("the image does not fit in 200 KB even at low quality - use a simpler image")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            w,
            h,
            image::Rgba([200, 30, 30, 255]),
        ));
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn big_image_becomes_small_jpeg() {
        let jpeg = from_bytes(&png(1600, 900)).unwrap();
        assert!(jpeg.len() <= MAX_BYTES);
        let back = image::load_from_memory(&jpeg).unwrap();
        assert!(back.width() <= MAX_SIDE && back.height() <= MAX_SIDE);
        assert!(back.width() > back.height()); // aspect ratio kept
    }

    #[test]
    fn not_an_image_is_an_error() {
        assert!(from_bytes(b"definitely not an image").is_err());
    }
}
