//! Screens of the album stories (320x240, gray levels), drawn with the
//! embedded DejaVu Sans Bold font.

use std::io::Cursor;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use anyhow::{Context as _, Result};
use image::{DynamicImage, GrayImage, ImageFormat, Luma, Rgb, RgbImage, imageops::FilterType};

use crate::convert::image::{LUNII_H, LUNII_W};

pub(super) const FONT: &[u8] = include_bytes!("../../../res/fonts/DejaVuSans-Bold.ttf");

/// The embedded font, to draw the screens.
pub fn font() -> Result<FontRef<'static>> {
    FontRef::try_from_slice(FONT).context("embedded font")
}

// ---------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------

/// Screen of a track: the album, its title and its number. No "n / total": it
/// would be wrong once tracks are appended.
pub(super) fn track_screen(
    font: &FontRef,
    album_title: &str,
    title: &str,
    number: usize,
) -> GrayImage {
    text_screen(font, album_title, title, &number.to_string())
}

/// Screen with a small header, a big title and a small footer.
pub(super) fn text_screen(font: &FontRef, header: &str, title: &str, footer: &str) -> GrayImage {
    let mut img = blank();
    draw_text_block(&mut img, font, header, 10, 6, 28, 16.0);
    draw_text_block(&mut img, font, title, 16, 44, 152, 36.0);
    draw_text_block(&mut img, font, footer, 10, 204, 30, 20.0);
    img
}

pub(super) const BACKGROUND: u8 = 0;
pub(super) const FOREGROUND: u8 = 255;
pub(super) const MARGIN: u32 = 12;

pub(super) fn blank() -> GrayImage {
    GrayImage::from_pixel(LUNII_W, LUNII_H, Luma([BACKGROUND]))
}

pub(super) fn png<P: image::PixelWithColorType>(
    img: &image::ImageBuffer<P, Vec<P::Subpixel>>,
) -> Result<Vec<u8>>
where
    [P::Subpixel]: image::EncodableLayout,
{
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png)?;
    Ok(out.into_inner())
}

/// Cover scaled to fit the screen, centered on a black background.
pub(super) fn fit_cover(img: &DynamicImage) -> GrayImage {
    let scaled = img
        .resize(LUNII_W, LUNII_H, FilterType::CatmullRom)
        .to_luma8();
    let mut out = blank();
    let x = (LUNII_W - scaled.width()) / 2;
    let y = (LUNII_H - scaled.height()) / 2;
    image::imageops::overlay(&mut out, &scaled, x.into(), y.into());
    out
}

/// Same as [`fit_cover`], in color: the thumbnail of the story (application,
/// Flam story list), where the screens of the device are gray.
pub(super) fn fit_cover_color(img: &DynamicImage) -> RgbImage {
    let scaled = img
        .resize(LUNII_W, LUNII_H, FilterType::CatmullRom)
        .to_rgb8();
    let mut out = RgbImage::from_pixel(LUNII_W, LUNII_H, Rgb([BACKGROUND; 3]));
    let x = (LUNII_W - scaled.width()) / 2;
    let y = (LUNII_H - scaled.height()) / 2;
    image::imageops::overlay(&mut out, &scaled, x.into(), y.into());
    out
}

pub(super) fn text_width(font: &FontRef, scale: PxScale, text: &str) -> f32 {
    let f = font.as_scaled(scale);
    let mut width = 0.0;
    let mut prev = None;
    for c in text.chars() {
        let id = f.glyph_id(c);
        if let Some(p) = prev {
            width += f.kern(p, id);
        }
        width += f.h_advance(id);
        prev = Some(id);
    }
    width
}

/// Word wrapping; a word too long for a line is cut.
pub(super) fn wrap(font: &FontRef, scale: PxScale, text: &str, max_width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if text_width(font, scale, &candidate) <= max_width {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        // the word alone may still be too long
        for c in word.chars() {
            line.push(c);
            if text_width(font, scale, &line) > max_width && line.chars().count() > 1 {
                let last = line.pop().unwrap();
                lines.push(std::mem::take(&mut line));
                line.push(last);
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Draws `text` centered in the band `[top, top + height)`, as big as possible
/// up to `max_size` px (and down to 12 px, then lines are dropped with "…").
pub(super) fn draw_text_block(
    img: &mut GrayImage,
    font: &FontRef,
    text: &str,
    max_lines: usize,
    top: u32,
    height: u32,
    max_size: f32,
) {
    let max_width = (LUNII_W - 2 * MARGIN) as f32;
    let mut size = max_size;
    let (scale, mut lines) = loop {
        let scale = PxScale::from(size);
        let lines = wrap(font, scale, text, max_width);
        let line_h = font.as_scaled(scale).height();
        let fits = lines.len() <= max_lines && line_h * lines.len() as f32 <= height as f32;
        if fits || size <= 12.0 {
            break (scale, lines);
        }
        size -= 2.0;
    };
    let f = font.as_scaled(scale);
    let line_h = f.height();
    let max_fit = ((height as f32 / line_h).floor() as usize).clamp(1, max_lines);
    if lines.len() > max_fit {
        lines.truncate(max_fit);
        if let Some(last) = lines.last_mut() {
            while !last.is_empty() && text_width(font, scale, &format!("{last}…")) > max_width {
                last.pop();
            }
            last.push('…');
        }
    }

    let block_h = line_h * lines.len() as f32;
    let mut y = top as f32 + (height as f32 - block_h).max(0.0) / 2.0;
    for line in &lines {
        let x = (LUNII_W as f32 - text_width(font, scale, line)) / 2.0;
        draw_line(img, font, scale, line, x, y + f.ascent());
        y += line_h;
    }
}

pub(super) fn draw_line(
    img: &mut GrayImage,
    font: &FontRef,
    scale: PxScale,
    text: &str,
    x: f32,
    baseline: f32,
) {
    let f = font.as_scaled(scale);
    let mut caret = x;
    let mut prev = None;
    for c in text.chars() {
        let id = f.glyph_id(c);
        if let Some(p) = prev {
            caret += f.kern(p, id);
        }
        let glyph = id.with_scale_and_position(scale, point(caret, baseline));
        caret += f.h_advance(id);
        prev = Some(id);
        let Some(outline) = font.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outline.px_bounds();
        outline.draw(|gx, gy, coverage| {
            let px = bounds.min.x as i32 + gx as i32;
            let py = bounds.min.y as i32 + gy as i32;
            if px < 0 || py < 0 || px >= LUNII_W as i32 || py >= LUNII_H as i32 {
                return;
            }
            let p = img.get_pixel_mut(px as u32, py as u32);
            let v = p[0] as f32 + (FOREGROUND as f32 - p[0] as f32) * coverage.clamp(0.0, 1.0);
            p[0] = v.round() as u8;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping() {
        let font = FontRef::try_from_slice(FONT).unwrap();
        let lines = wrap(
            &font,
            PxScale::from(36.0),
            "Un titre plutôt long pour un écran",
            296.0,
        );
        assert!(lines.len() > 1);
        assert!(
            lines
                .iter()
                .all(|l| text_width(&font, PxScale::from(36.0), l) <= 296.0)
        );
    }
}
