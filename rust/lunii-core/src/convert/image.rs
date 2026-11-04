//! Image conversions (port of `pkg/api/convert_image.py`).

use anyhow::{Context as _, Result};
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageFormat};

pub const LUNII_W: u32 = 320;
pub const LUNII_H: u32 = 240;

/// Converts an RGB888 pixel to RGB565 (LSB first, to reproduce the Flam behaviour).
pub fn pixel_to_rgb565(r: u8, g: u8, b: u8) -> [u8; 2] {
    let r5 = (r as u16 * 32) / 256;
    let g6 = (g as u16 * 64) / 256;
    let b5 = (b as u16 * 32) / 256;
    ((r5 << 11) | (g6 << 5) | b5).to_le_bytes()
}

/// Thumbnail size computation of Pillow's `Image.thumbnail` (aspect ratio preserved).
fn thumbnail_size(width: u32, height: u32, max: u32) -> Option<(u32, u32)> {
    if max >= width && max >= height {
        return None;
    }
    let (mut x, mut y) = (max as f64, max as f64);
    let aspect = width as f64 / height as f64;
    let round_aspect = |number: f64, key: &dyn Fn(f64) -> f64| -> f64 {
        let (lo, hi) = (number.floor(), number.ceil());
        let best = if key(lo) <= key(hi) { lo } else { hi };
        best.max(1.0)
    };
    if x / y >= aspect {
        x = round_aspect(y * aspect, &|n| (aspect - n / y).abs());
    } else {
        y = round_aspect(x / aspect, &|n| {
            if n == 0.0 {
                0.0
            } else {
                (aspect - x / n).abs()
            }
        });
    }
    Some((x as u32, y as u32))
}

/// Converts an image to the Flam LIFF format (104 pixels max, RGB565).
pub fn image_to_liff(image_data: &[u8]) -> Result<Vec<u8>> {
    let mut img = image::load_from_memory(image_data).context("unable to decode image")?;

    if let Some((w, h)) = thumbnail_size(img.width(), img.height(), 104) {
        img = img.resize_exact(w, h, FilterType::CatmullRom);
    }
    let (width, height) = img.dimensions();
    let rgb = img.to_rgb8();

    let mut liff = Vec::with_capacity(13 + rgb.len() + 8);
    liff.extend_from_slice(b"liff");
    liff.extend_from_slice(&width.to_be_bytes());
    liff.extend_from_slice(&height.to_be_bytes());
    liff.push(0xA2);

    for p in rgb.pixels() {
        liff.push(0xFE);
        liff.extend_from_slice(&pixel_to_rgb565(p[0], p[1], p[2]));
    }

    liff.extend_from_slice(&[0u8; 7]);
    liff.push(0x01);
    Ok(liff)
}

/// (compression, width, height) of a BMP file.
fn bmp_header(data: &[u8]) -> Option<(u32, u32, u32)> {
    if data.len() < 34 || &data[0..2] != b"BM" {
        return None;
    }
    let dib_size = u32::from_le_bytes(data[14..18].try_into().ok()?);
    if dib_size < 40 {
        return None;
    }
    let w = i32::from_le_bytes(data[18..22].try_into().ok()?);
    let h = i32::from_le_bytes(data[22..26].try_into().ok()?);
    let compression = u32::from_le_bytes(data[30..34].try_into().ok()?);
    Some((compression, w.unsigned_abs(), h.unsigned_abs()))
}

/// Converts any picture to a 320x240 4 bits grayscale RLE4 bitmap (Lunii format).
pub fn image_to_bitmap_rle4(image_data: &[u8]) -> Result<Vec<u8>> {
    // already in the expected format
    if let Some((2, LUNII_W, LUNII_H)) = bmp_header(image_data) {
        return Ok(image_data.to_vec());
    }

    let mut img = image::load_from_memory(image_data).context("unable to decode image")?;
    if img.dimensions() != (LUNII_W, LUNII_H) {
        img = img.resize_exact(LUNII_W, LUNII_H, FilterType::CatmullRom);
    }
    // bitmap rows are stored bottom-up
    img = img.flipv();

    let (width, height) = img.dimensions();
    let gray: Vec<u8> = match &img {
        DynamicImage::ImageLuma8(l) => l.pixels().map(|p| p[0] / 16).collect(),
        _ => img
            .to_rgb8()
            .pixels()
            .map(|p| {
                ((p[0] as f64 * 0.299 + p[1] as f64 * 0.587 + p[2] as f64 * 0.114) / 16.0) as u8
            })
            .collect(),
    };

    let mut bmp_data = Vec::new();
    for y in 0..height as usize {
        let row = &gray[y * width as usize..(y + 1) * width as usize];
        let mut rle_count = 1u8;
        let mut rle_value = row[0];
        for &value in &row[1..] {
            if value == rle_value && rle_count < 255 {
                rle_count += 1;
            } else {
                bmp_data.push(rle_count);
                bmp_data.push((rle_value << 4) | rle_value);
                rle_count = 1;
                rle_value = value;
            }
        }
        bmp_data.push(rle_count);
        bmp_data.push((rle_value << 4) | rle_value);
        if y < height as usize - 1 {
            bmp_data.extend_from_slice(&[0, 0]);
        }
    }
    bmp_data.extend_from_slice(&[0, 1]);

    const HEADER_SIZE: u32 = 54;
    const PALETTE_SIZE: u32 = 16 * 4;
    let data_size = bmp_data.len() as u32;
    let data_offset = HEADER_SIZE + PALETTE_SIZE;
    let file_size = data_offset + data_size;

    let mut bmp = Vec::with_capacity(file_size as usize);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&[0, 0, 0, 0]); // reserved
    bmp.extend_from_slice(&data_offset.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes()); // header size
    bmp.extend_from_slice(&width.to_le_bytes());
    bmp.extend_from_slice(&height.to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes()); // color planes
    bmp.extend_from_slice(&4u16.to_le_bytes()); // bits per pixel
    bmp.extend_from_slice(&2u32.to_le_bytes()); // compression: RLE4
    bmp.extend_from_slice(&data_size.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes()); // horizontal resolution
    bmp.extend_from_slice(&0u32.to_le_bytes()); // vertical resolution
    bmp.extend_from_slice(&16u32.to_le_bytes()); // palette colors
    bmp.extend_from_slice(&0u32.to_le_bytes()); // important colors

    // grayscale palette
    for i in 0..16u32 {
        let gray = ((255.0 / 16.0) * i as f64) as u8;
        bmp.extend_from_slice(&[gray, gray, gray, 0]);
    }

    bmp.extend_from_slice(&bmp_data);
    Ok(bmp)
}

/// Detects the image format (used to display story thumbnails).
pub fn guess_format(data: &[u8]) -> Option<ImageFormat> {
    image::guess_format(data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb565() {
        assert_eq!(pixel_to_rgb565(255, 255, 255), [0xFF, 0xFF]);
        assert_eq!(pixel_to_rgb565(255, 0, 0), [0x00, 0xF8]);
    }

    #[test]
    fn thumbnail() {
        assert_eq!(thumbnail_size(320, 240, 104), Some((104, 78)));
        assert_eq!(thumbnail_size(240, 320, 104), Some((78, 104)));
        assert_eq!(thumbnail_size(50, 60, 104), None);
    }
}
