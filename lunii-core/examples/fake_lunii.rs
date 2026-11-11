//! Fake Lunii v2 with invented stories, for trying the application or taking
//! screenshots without a real device. Run through `tools/fake-lunii.sh`.
//!
//! `cargo run -p lunii-core --example fake_lunii -- <dir>` creates
//! `<dir>/lunii` (the device) and `<dir>/cfg` (its configuration directory,
//! to pass as `LUNII_CFG_DIR`: it holds the titles of the stories).

use std::io::Cursor;
use std::path::{Path, PathBuf};

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use anyhow::{Context as _, Result, bail};
use image::{ImageFormat, Rgb, RgbImage};
use lunii_core::constants::*;
use lunii_core::{Context, album, crypto, db, open_device};
use uuid::Uuid;

struct Fake {
    title: &'static str,
    description: &'static str,
    minutes: u32,
    /// cover colors: background, disc
    bg: [u8; 3],
    fg: [u8; 3],
}

const fn fake(
    title: &'static str,
    description: &'static str,
    minutes: u32,
    bg: [u8; 3],
    fg: [u8; 3],
) -> Fake {
    Fake {
        title,
        description,
        minutes,
        bg,
        fg,
    }
}

#[rustfmt::skip]
const STORIES: &[Fake] = &[
    fake("Le Phare des Lucioles", "Une gardienne de phare et ses lucioles qui guident les bateaux.", 118, [32, 48, 96], [250, 210, 90]),
    fake("Noisette et le Grand Hiver", "Un écureuil prépare la plus longue saison de la forêt.", 64, [150, 90, 50], [235, 235, 245]),
    fake("Le Voyage de Pistache", "Un petit dragon vert part à la recherche du soleil.", 97, [60, 140, 80], [250, 180, 60]),
    fake("La Bibliothèque des Nuages", "Des livres qui flottent au-dessus de la ville.", 35, [120, 170, 220], [255, 255, 255]),
    fake("Les Jardins de Mila", "Mila fait pousser des légumes qui chantent.", 52, [90, 160, 70], [240, 120, 160]),
    fake("Le Robot qui Voulait Rêver", "Bip découvre ce qui se passe quand on ferme les yeux.", 81, [90, 100, 120], [120, 220, 230]),
    fake("Capitaine Moustache", "Un chat pirate sur un bateau en carton.", 45, [40, 40, 60], [220, 70, 60]),
    fake("Berceuse pour une Étoile", "Une histoire douce pour s'endormir.", 22, [20, 20, 60], [200, 200, 255]),
];

/// On the device but missing from its story list: found by the lost stories
/// search.
#[rustfmt::skip]
const LOST: &[Fake] = &[
    fake("La Montagne qui Éternuait", "Un village apprend à vivre avec une montagne enrhumée.", 40, [110, 80, 130], [250, 250, 250]),
    fake("Le Train des Saisons", "Un train qui traverse le printemps, l'été, l'automne et l'hiver.", 25, [180, 70, 50], [250, 220, 120]),
];

/// Stories flagged for the night mode, and hidden, by index in `STORIES`.
const NIGHT_MODE: &[usize] = &[3, 7];
const HIDDEN: &[usize] = &[6, 7];

const FONT: &[u8] = include_bytes!("../../res/fonts/DejaVuSans-Bold.ttf");

fn main() -> Result<()> {
    let Some(out) = std::env::args_os().nth(1).map(PathBuf::from) else {
        bail!("usage: fake_lunii <output dir>");
    };
    let root = out.join("lunii");
    let cfg = out.join("cfg");
    if root.exists() || cfg.exists() {
        bail!("{} already holds a fake device", out.display());
    }
    std::fs::create_dir_all(&cfg)?;
    set_cfg_dir(&cfg);
    // empty official database: nothing to download
    std::fs::write(cfg.join("official.db"), "{}")?;
    db::load_db(false);

    fake_lunii_v2(&root)?;
    let ctx = Context::default();
    let mut dev = open_device(&ctx, &root)?;
    let work = out.join("work");
    std::fs::create_dir_all(&work)?;
    let font = FontRef::try_from_slice(FONT)?;

    let mut uuids = Vec::new();
    for (i, s) in STORIES.iter().chain(LOST).enumerate() {
        let uuid = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("fake-lunii/{}", s.title).as_bytes(),
        );
        let audio = work.join(format!("{i}.mp3"));
        std::fs::write(&audio, mp3(s.minutes))?;
        let cover = cover(&font, s, i)?;
        let story = album::SingleStory {
            uuid,
            title: s.title,
            description: s.description,
            header: "",
            footer: "",
            thumbnail: Some(&cover),
            audio: &audio,
        };
        let zip = work.join(format!("{i}.zip"));
        story.write_studio_zip(&zip)?;
        dev.import_story(&ctx, &zip)
            .with_context(|| format!("import of {}", s.title))?;
        std::fs::remove_file(&zip)?;
        std::fs::remove_file(&audio)?;
        uuids.push(uuid);
    }
    let pick = |idx: &[usize]| idx.iter().map(|&i| uuids[i]).collect::<Vec<_>>();
    dev.as_lunii_mut()
        .context("not a Lunii")?
        .toggle_night_mode(&pick(NIGHT_MODE))?;
    dev.toggle_hidden(&ctx, &pick(HIDDEN))?;
    std::fs::remove_dir_all(&work)?;

    // the lost stories taken out of the list (.pi): their files stay
    let lost = &uuids[STORIES.len()..];
    let pi = root.join(".pi");
    let kept: Vec<u8> = std::fs::read(&pi)?
        .chunks(16)
        .filter(|u| !lost.iter().any(|l| l.as_bytes() == *u))
        .flatten()
        .copied()
        .collect();
    std::fs::write(&pi, kept)?;
    Ok(())
}

/// Lunii v2 (firmware 2.22) with a settings file: the night mode is offered.
fn fake_lunii_v2(root: &Path) -> Result<()> {
    let mut md = vec![0u8; 512];
    md[0..2].copy_from_slice(&3u16.to_le_bytes());
    md[6..8].copy_from_slice(&2u16.to_le_bytes());
    md[8..10].copy_from_slice(&22u16.to_le_bytes());
    md[10..18].copy_from_slice(&[0, 0, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]);
    md[18..20].copy_from_slice(&0x0483u16.to_le_bytes());
    md[20..22].copy_from_slice(&0xA341u16.to_le_bytes());
    let devkey: Vec<u8> = (0..=255u8).collect();
    md[0x100..0x200].copy_from_slice(&crypto::xxtea_encrypt(&devkey, &lunii_generic_key())?);
    std::fs::create_dir_all(root.join(".content"))?;
    std::fs::write(root.join(".md"), md)?;
    // version 0x100, default values
    std::fs::write(root.join(".cfg"), [0x00, 0x01])?;
    Ok(())
}

/// `minutes` of MPEG-1 layer III frames, 32 kbps, 44.1 kHz, mono (silence for
/// the application: it is not played).
fn mp3(minutes: u32) -> Vec<u8> {
    let mut frame = [0u8; 104];
    frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x10, 0xC4]);
    frame.repeat((minutes as usize * 60 * 44_100).div_ceil(1152))
}

/// Cover: its title over a vertical gradient, and a disc whose place varies
/// with `n`.
fn cover(font: &FontRef, s: &Fake, n: usize) -> Result<Vec<u8>> {
    let (w, h) = (320u32, 240u32);
    let (cx, cy) = (70.0 + (n % 3) as f32 * 90.0, 165.0 + (n % 2) as f32 * 20.0);
    let mut img = RgbImage::from_fn(w, h, |x, y| {
        let t = y as f32 / h as f32;
        let shade = |c: u8| (c as f32 * (1.0 - 0.4 * t)) as u8;
        let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
        if d < 45.0 {
            Rgb(s.fg)
        } else {
            Rgb(s.bg.map(shade))
        }
    });
    // white title with a shadow, wrapped
    let scale = PxScale::from(30.0);
    let sf = font.as_scaled(scale);
    let mut lines = vec![String::new()];
    for word in s.title.split(' ') {
        let line = lines.last_mut().unwrap();
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        let width: f32 = candidate
            .chars()
            .map(|c| sf.h_advance(sf.glyph_id(c)))
            .sum();
        if width > (w - 32) as f32 && !line.is_empty() {
            lines.push(word.to_string());
        } else {
            *line = candidate;
        }
    }
    for (row, line) in lines.iter().enumerate() {
        let y = 16.0 + sf.ascent() + row as f32 * sf.height();
        for (offset, color) in [(2.0, [0, 0, 0]), (0.0, [255, 255, 255])] {
            let mut x = 16.0;
            for c in line.chars() {
                let id = sf.glyph_id(c);
                let glyph = id.with_scale_and_position(scale, point(x + offset, y + offset));
                x += sf.h_advance(id);
                let Some(outline) = font.outline_glyph(glyph) else {
                    continue;
                };
                let b = outline.px_bounds();
                outline.draw(|gx, gy, cov| {
                    let (px, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                    if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                        return;
                    }
                    let p = img.get_pixel_mut(px as u32, py as u32);
                    for k in 0..3 {
                        p[k] = (p[k] as f32 * (1.0 - cov) + color[k] as f32 * cov) as u8;
                    }
                });
            }
        }
    }
    let mut png = Vec::new();
    img.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)?;
    Ok(png)
}
