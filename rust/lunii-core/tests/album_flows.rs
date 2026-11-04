//! MP3 album import on a fake Flam (no network, no ffmpeg: the MP3 files are
//! already mono 44.1 kHz).

use std::path::{Path, PathBuf};

use lunii_core::constants::*;
use lunii_core::{Context, album, db, open_device};

fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    set_cfg_dir(dir.path());
    std::fs::write(dir.path().join("official.db"), "{}").unwrap();
    std::fs::write(dir.path().join("third-party.db"), "{}").unwrap();
    db::load_db(false);
    dir
}

fn fake_flam(dir: &Path) -> PathBuf {
    let root = dir.join("flam");
    std::fs::create_dir_all(root.join("str")).unwrap();
    std::fs::create_dir_all(root.join("etc/library")).unwrap();
    let mut mdf = vec![0u8; 0x80];
    let fw = b"main: 2.0.0-1\ncomm: 2.0.0-1";
    mdf[2..2 + fw.len()].copy_from_slice(fw);
    mdf[50..74].copy_from_slice(b"303A819E0123456789ABCDEF");
    mdf[74..76].copy_from_slice(&0x303Au16.to_le_bytes());
    mdf[76..78].copy_from_slice(&0x819Eu16.to_le_bytes());
    for (i, b) in mdf[0x4E..0x6E].iter_mut().enumerate() {
        *b = i as u8 * 3;
    }
    std::fs::write(root.join(".mdf"), mdf).unwrap();
    root
}

/// `frames` MPEG-1 layer III frames, 128 kbps, 44.1 kHz, mono.
fn mp3(frames: usize) -> Vec<u8> {
    let mut frame = vec![0u8; 417];
    frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0xC4]);
    frame.repeat(frames)
}

/// ID3v2.4 tag (UTF-8) with a title, an album and a track number
/// (frame sizes are below 128: no syncsafe encoding needed for them).
fn with_tags(title: &str, track: u32, audio: Vec<u8>) -> Vec<u8> {
    let frame = |id: &[u8], text: &str| {
        let mut f = id.to_vec();
        f.extend_from_slice(&(text.len() as u32 + 1).to_be_bytes());
        f.extend_from_slice(&[0, 0, 3]); // flags, UTF-8
        f.extend_from_slice(text.as_bytes());
        f
    };
    let mut body = frame(b"TIT2", title);
    body.extend(frame(b"TALB", "Album test"));
    body.extend(frame(b"TRCK", &track.to_string()));
    let size = body.len() as u32;
    let syncsafe = [
        (size >> 21 & 0x7F) as u8,
        (size >> 14 & 0x7F) as u8,
        (size >> 7 & 0x7F) as u8,
        (size & 0x7F) as u8,
    ];
    let mut out = b"ID3\x04\x00\x00".to_vec();
    out.extend_from_slice(&syncsafe);
    out.extend(body);
    out.extend(audio);
    out
}

#[test]
fn mp3_album_on_flam() {
    let _cfg = setup();
    let tmp = tempfile::tempdir().unwrap();

    // file names in the reverse order of the track numbers: the tags win
    let dir = tmp.path().join("Mon album");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.mp3"), with_tags("Deuxième", 2, mp3(40))).unwrap();
    std::fs::write(dir.join("b.mp3"), with_tags("Première", 1, mp3(30))).unwrap();
    std::fs::write(dir.join("notes.txt"), "not audio").unwrap();
    assert!(album::is_album_dir(&dir));

    let album = album::scan(&dir).unwrap();
    assert_eq!(album.title, "Album test");
    let titles: Vec<_> = album.tracks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["Première", "Deuxième"]);

    let zip = tmp.path().join("album.zip");
    album.write_studio_zip(&zip).unwrap();

    let ctx = Context::default();
    let root = fake_flam(tmp.path());
    let mut dev = open_device(&ctx, &root).unwrap();
    assert_eq!(dev.version(), DeviceVersion::FlamV1);
    dev.import_story(&ctx, &zip).unwrap();

    let uuid = album.uuid();
    let story_dir = root.join("str").join(uuid.to_string());
    for f in ["ni", "ri", "si", "li", "key", "info", "img/thumbnail.lif"] {
        assert!(story_dir.join(f).is_file(), "missing {f}");
    }
    let count = |sub: &str| std::fs::read_dir(story_dir.join(sub)).unwrap().count();
    // cover + 2 track screens; cover silence + 2 excerpts + 2 tracks
    assert_eq!(count("rf/000"), 3);
    assert_eq!(count("sf/000"), 5);
    // the firmware needs 8 characters names (fixed size `ri` / `si` entries)
    for sub in ["rf/000", "sf/000"] {
        for f in std::fs::read_dir(story_dir.join(sub)).unwrap() {
            let name = f.unwrap().file_name().to_string_lossy().to_string();
            assert_eq!(name.len(), 8, "{sub}/{name}");
        }
    }
    assert_eq!(dev.stories()[0].name(), "Album test");
    // bytes counted for the remaining time estimation
    assert!(ctx.written() >= album.audio_size() / 2);

    // same album again: refused
    assert!(dev.import_story(&ctx, &zip).is_err());
}
