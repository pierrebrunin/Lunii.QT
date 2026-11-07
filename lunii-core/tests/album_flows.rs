//! MP3 album import on a fake Flam (no network, no ffmpeg: the MP3 files are
//! already mono 44.1 kHz).

use std::path::{Path, PathBuf};

use lunii_core::constants::*;
use lunii_core::{Context, album, crypto, db, open_device};

/// Configuration directory shared by the tests of this binary (it is global).
fn setup() -> &'static Path {
    static CFG: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    CFG.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        set_cfg_dir(dir.path());
        std::fs::write(dir.path().join("official.db"), "{}").unwrap();
        std::fs::write(dir.path().join("third-party.db"), "{}").unwrap();
        db::load_db(false);
        dir
    })
    .path()
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
    // cover + 2 track screens; one silence (cover + menu) + 2 tracks
    assert_eq!(count("rf/000"), 3);
    assert_eq!(count("sf/000"), 3);
    // the firmware needs 8 characters names (fixed size `ri` / `si` entries)
    for sub in ["rf/000", "sf/000"] {
        for f in std::fs::read_dir(story_dir.join(sub)).unwrap() {
            let name = f.unwrap().file_name().to_string_lossy().to_string();
            assert_eq!(name.len(), 8, "{sub}/{name}");
        }
    }
    assert_eq!(dev.stories()[0].name(), "Album test");
    // progress counted in archive bytes: it ends exactly on the assets size,
    // whatever the conversions (tags removed, images turned into bitmaps)
    let archive = lunii_core::archive::Archive::open(&zip).unwrap();
    let assets: u64 = archive
        .file_names()
        .iter()
        .filter(|n| n.starts_with("assets/"))
        .map(|n| archive.size(n).unwrap())
        .sum();
    assert_eq!(ctx.progress(), assets);

    // same album again: refused
    assert!(dev.import_story(&ctx, &zip).is_err());

    // append a track to the album on the device
    let story = dev.stories()[0].clone();
    assert_eq!(dev.album_track_count(&story), Some(2));
    let extra = tmp.path().join("extra.mp3");
    std::fs::write(&extra, with_tags("Troisième", 9, mp3(20))).unwrap();
    let old_track = std::fs::read(story_dir.join("sf/000/TRK00001")).unwrap();
    // excerpt of an album created by a previous version: removed on append
    std::fs::write(story_dir.join("sf/000/EXC00001"), b"old excerpt").unwrap();
    let extra_tracks = album::scan_files(&[extra]).unwrap().tracks;
    assert_eq!(dev.add_album_tracks(&ctx, &uuid, &extra_tracks).unwrap(), 1);

    assert_eq!(dev.album_track_count(&story), Some(3));
    for f in ["rf/000/IMG00003", "sf/000/TRK00003"] {
        assert!(story_dir.join(f).is_file(), "missing {f}");
    }
    assert!(!story_dir.join("sf/000/EXC00001").exists());
    // existing tracks untouched
    assert_eq!(
        std::fs::read(story_dir.join("sf/000/TRK00001")).unwrap(),
        old_track
    );
    // ni (plain on Flam): cover + 3 menu entries + 3 playback nodes
    let ni = std::fs::read(story_dir.join("ni")).unwrap();
    assert_eq!(u32::from_le_bytes(ni[12..16].try_into().unwrap()), 7);
    assert_eq!(u32::from_le_bytes(ni[16..20].try_into().unwrap()), 4);
    assert_eq!(u32::from_le_bytes(ni[20..24].try_into().unwrap()), 4);

    // not an album: refused
    std::fs::remove_file(story_dir.join("sf/000/SNDCOVER")).unwrap();
    assert_eq!(dev.album_track_count(&story), None);
    assert!(dev.add_album_tracks(&ctx, &uuid, &extra_tracks).is_err());
}

fn fake_lunii_v2(dir: &Path) -> PathBuf {
    let mut md = vec![0u8; 512];
    md[0..2].copy_from_slice(&3u16.to_le_bytes());
    md[6..8].copy_from_slice(&2u16.to_le_bytes());
    md[8..10].copy_from_slice(&22u16.to_le_bytes());
    md[10..18].copy_from_slice(&[0, 0, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]);
    md[18..20].copy_from_slice(&0x0483u16.to_le_bytes());
    md[20..22].copy_from_slice(&0xA341u16.to_le_bytes());
    let devkey: Vec<u8> = (0..=255u8).collect();
    md[0x100..0x200]
        .copy_from_slice(&crypto::xxtea_encrypt(&devkey, &lunii_generic_key()).unwrap());
    let root = dir.join("lunii_v2");
    std::fs::create_dir_all(root.join(".content")).unwrap();
    std::fs::write(root.join(".md"), md).unwrap();
    root
}

/// A podcast episode as a story of its own, on a Lunii v2 and on a Flam.
#[test]
fn episode_story_on_lunii_and_flam() {
    let _cfg = setup();
    let tmp = tempfile::tempdir().unwrap();
    let audio = tmp.path().join("episode.mp3");
    std::fs::write(&audio, mp3(30)).unwrap();
    let uuid = lunii_core::podcast::episode_uuid("https://example.org/feed.xml", "ep-1");
    let story = album::SingleStory {
        uuid,
        title: "Pourquoi le ciel est bleu ?",
        description: "Les P'tits Bateaux",
        header: "Les P'tits Bateaux",
        footer: "04/11/2026",
        thumbnail: None,
        audio: &audio,
    };
    let zip = tmp.path().join("episode.zip");
    story.write_studio_zip(&zip).unwrap();

    let ctx = Context::default();
    for root in [fake_lunii_v2(tmp.path()), fake_flam(tmp.path())] {
        let mut dev = open_device(&ctx, &root).unwrap();
        dev.import_story(&ctx, &zip).unwrap();
        let s = dev
            .stories()
            .get(&uuid.to_string())
            .expect("story imported")
            .clone();
        assert_eq!(s.name(), "Pourquoi le ciel est bleu ?");
        // a single story, not an album
        assert_eq!(dev.album_track_count(&s), None);
        let dir = dev.story_path(&s);
        for f in ["ni", "ri", "si", "li"] {
            assert!(dir.join(f).is_file(), "{}: missing {f}", root.display());
        }
        assert!(dev.import_story(&ctx, &zip).is_err(), "imported twice");
    }
}

/// Tracks appended to an album on a Lunii v2: new files, regenerated index
/// files and authorization file (derived from `ri`).
#[test]
fn mp3_album_append_on_lunii() {
    let _cfg = setup();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Album Lunii");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("1.mp3"), with_tags("Un", 1, mp3(30))).unwrap();
    std::fs::write(dir.join("2.mp3"), with_tags("Deux", 2, mp3(30))).unwrap();
    let album = album::scan(&dir).unwrap();
    let zip = tmp.path().join("album.zip");
    album.write_studio_zip(&zip).unwrap();

    let ctx = Context::default();
    let root = fake_lunii_v2(tmp.path());
    let mut dev = open_device(&ctx, &root).unwrap();
    dev.import_story(&ctx, &zip).unwrap();
    let uuid = album.uuid();
    let story = dev.stories().get(&uuid.to_string()).unwrap().clone();
    assert_eq!(dev.album_track_count(&story), Some(2));

    let story_dir = dev.story_path(&story);
    let old_bt = std::fs::read(story_dir.join("bt")).unwrap();
    let old_track = std::fs::read(story_dir.join("sf/000/TRK00001")).unwrap();
    let extra = tmp.path().join("extra.mp3");
    std::fs::write(&extra, with_tags("Trois", 3, mp3(20))).unwrap();
    let tracks = album::scan_files(&[extra]).unwrap().tracks;
    assert_eq!(dev.add_album_tracks(&ctx, &uuid, &tracks).unwrap(), 1);

    assert_eq!(dev.album_track_count(&story), Some(3));
    for f in ["rf/000/IMG00003", "sf/000/TRK00003"] {
        assert!(story_dir.join(f).is_file(), "missing {f}");
    }
    assert_eq!(
        std::fs::read(story_dir.join("sf/000/TRK00001")).unwrap(),
        old_track
    );
    // ni (plain): cover + 3 menu entries + 3 playback nodes
    let ni = std::fs::read(story_dir.join("ni")).unwrap();
    assert_eq!(u32::from_le_bytes(ni[12..16].try_into().unwrap()), 7);
    // `ri` grew (cover + 3 screens): the authorization file follows
    let bt = std::fs::read(story_dir.join("bt")).unwrap();
    assert_ne!(bt, old_bt);
    // still a valid story once the device is reopened
    let dev = open_device(&ctx, &root).unwrap();
    assert!(dev.stories().get(&uuid.to_string()).is_some());
}

/// Editing an album: tracks removed, moved and added in one go, on Lunii and
/// Flam. The titles are kept on the device (`album.json`).
#[test]
fn mp3_album_edit() {
    use album::AlbumEntry::{Existing, New};
    let _cfg = setup();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Album à éditer");
    std::fs::create_dir_all(&dir).unwrap();
    for (i, title) in ["Un", "Deux", "Trois"].iter().enumerate() {
        std::fs::write(
            dir.join(format!("{i}.mp3")),
            with_tags(title, i as u32 + 1, mp3(20 + i)),
        )
        .unwrap();
    }
    let album = album::scan(&dir).unwrap();
    let zip = tmp.path().join("album.zip");
    album.write_studio_zip(&zip).unwrap();
    let extra = tmp.path().join("extra.mp3");
    std::fs::write(&extra, with_tags("Quatre", 4, mp3(40))).unwrap();
    let added = album::scan_files(&[extra]).unwrap().tracks.remove(0);
    let uuid = album.uuid();
    let ctx = Context::default();

    for root in [fake_lunii_v2(tmp.path()), fake_flam(tmp.path())] {
        let mut dev = open_device(&ctx, &root).unwrap();
        dev.import_story(&ctx, &zip).unwrap();
        let story = dev.stories().get(&uuid.to_string()).unwrap().clone();
        let story_dir = dev.story_path(&story);
        let info = dev.album_info(&story).unwrap();
        assert_eq!(info.tracks, ["Un", "Deux", "Trois"]);
        // durations measured at the import (short tracks: 1 s)
        let before = info.durations.clone();
        assert_eq!(before.len(), 3);
        assert!(before.iter().all(|&d| d > 0), "{before:?}");
        assert_eq!(
            lunii_core::mp3::story_duration(&story_dir),
            Some(before.iter().sum())
        );
        let trois = std::fs::read(story_dir.join("sf/000/TRK00003")).unwrap();
        let screen_un = std::fs::read(story_dir.join("rf/000/IMG00001")).unwrap();

        // "Trois" first, "Deux" removed, "Quatre" added, "Un" last
        dev.edit_album(&ctx, &uuid, &[Existing(2), New(added.clone()), Existing(0)])
            .unwrap();
        let info = dev.album_info(&story).unwrap();
        assert_eq!(info.tracks, ["Trois", "Quatre", "Un"]);
        // moved tracks keep their duration, the added one gets its own
        assert_eq!(info.durations[0], before[2]);
        assert_eq!(info.durations[2], before[0]);
        assert!(info.durations[1] > 0);
        assert_eq!(dev.album_track_count(&story), Some(3));
        assert_eq!(
            std::fs::read(story_dir.join("sf/000/TRK00001")).unwrap(),
            trois
        );
        // "Un" moved: its screen shows its new number
        assert_ne!(
            std::fs::read(story_dir.join("rf/000/IMG00003")).unwrap(),
            screen_un
        );
        assert!(!story_dir.join("sf/000/TRK00004").exists());
        assert!(!story_dir.join("edit.tmp").exists());
        // ni (plain): cover + 3 menu entries + 3 playback nodes
        let ni = std::fs::read(story_dir.join("ni")).unwrap();
        assert_eq!(u32::from_le_bytes(ni[12..16].try_into().unwrap()), 7);
        assert!(db::story_description(&uuid).ends_with("3 MP3"));

        // invalid lists, the album is left as it was
        assert!(dev.edit_album(&ctx, &uuid, &[]).is_err());
        assert!(
            dev.edit_album(&ctx, &uuid, &[Existing(0), Existing(0)])
                .is_err()
        );
        assert!(dev.edit_album(&ctx, &uuid, &[Existing(3)]).is_err());
        assert_eq!(dev.album_info(&story).unwrap().tracks.len(), 3);

        // a single track left
        dev.edit_album(&ctx, &uuid, &[Existing(1)]).unwrap();
        assert_eq!(dev.album_info(&story).unwrap().tracks, ["Quatre"]);
        assert!(!story_dir.join("rf/000/IMG00002").exists());
        // still a valid story once the device is reopened
        let dev = open_device(&ctx, &root).unwrap();
        assert!(dev.stories().get(&uuid.to_string()).is_some());
    }
}

/// A folder of sub-folders (CDs): two menus, chapter screens, the cover file
/// of the folder; tracks can't be appended.
#[test]
fn chapters_album_on_flam() {
    let _cfg = setup();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Intégrale");
    for (cd, tracks) in [("CD 10", 1), ("CD 2", 2)] {
        let sub = dir.join(cd);
        std::fs::create_dir_all(&sub).unwrap();
        for t in 0..tracks {
            std::fs::write(
                sub.join(format!("{t}.mp3")),
                with_tags(&format!("{cd} piste {t}"), t + 1, mp3(20)),
            )
            .unwrap();
        }
    }
    // cover of the folder (the tags of `with_tags` hold none)
    let mut cover = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(64, 64, image::Rgb([200, 100, 50]))
        .write_to(&mut cover, image::ImageFormat::Png)
        .unwrap();
    std::fs::write(dir.join("Cover.png"), cover.get_ref()).unwrap();

    assert!(album::is_album_dir(&dir));
    let album = album::scan(&dir).unwrap();
    // same album tag in both CDs: it is the title
    assert_eq!(album.title, "Album test");
    assert_eq!(album.cover.as_deref(), Some(cover.get_ref().as_slice()));
    let chapters: Vec<_> = album
        .chapters
        .iter()
        .map(|c| (c.title.as_str(), c.len))
        .collect();
    // natural order: CD 2 before CD 10
    assert_eq!(chapters, [("CD 2", 2), ("CD 10", 1)]);
    assert_eq!(album.tracks[0].title, "CD 2 piste 0");

    let zip = tmp.path().join("album.zip");
    album.write_studio_zip(&zip).unwrap();
    let ctx = Context::default();
    let root = fake_flam(tmp.path());
    let mut dev = open_device(&ctx, &root).unwrap();
    dev.import_story(&ctx, &zip).unwrap();
    let story = dev
        .stories()
        .get(&album.uuid().to_string())
        .unwrap()
        .clone();
    let story_dir = dev.story_path(&story);
    for f in ["rf/000/CHP00001", "rf/000/CHP00002", "sf/000/TRK00003"] {
        assert!(story_dir.join(f).is_file(), "missing {f}");
    }
    // cover + 2 chapters + 3 menu entries + 3 playback nodes
    let ni = std::fs::read(story_dir.join("ni")).unwrap();
    assert_eq!(u32::from_le_bytes(ni[12..16].try_into().unwrap()), 9);
    assert_eq!(dev.album_track_count(&story), None);
    // the titles can be displayed, not edited
    let info = dev.album_info(&story).unwrap();
    assert_eq!(info.tracks.len(), 3);
    assert_eq!(info.chapters, album.chapters);
}

/// Podcast update: the albums are remembered, the new episodes found.
#[test]
fn podcast_new_episodes() {
    let _cfg = setup();
    let feed = |items: &[&str]| {
        let items: String = items
            .iter()
            .map(|g| {
                format!(
                    "<item><title>{g}</title><guid>{g}</guid><enclosure url=\"https://example.org/{g}.mp3\"/></item>"
                )
            })
            .collect();
        format!("<rss><channel><title>Mon podcast</title>{items}</channel></rss>")
    };
    let url = "https://example.org/update-test.xml";
    let p = lunii_core::podcast::parse_feed(url, &feed(&["b", "a"])).unwrap();
    lunii_core::podcast::record_imported(&p, &["a".into(), "b".into()], true).unwrap();
    let saved = lunii_core::podcast::saved_podcasts();
    let entry = saved.iter().find(|s| s.feed_url == url).expect("saved");
    assert_eq!(entry.album, p.album_uuid());
    assert_eq!(entry.title, "Mon podcast");

    let p = lunii_core::podcast::parse_feed(url, &feed(&["d", "c", "b", "a"])).unwrap();
    let new: Vec<_> = lunii_core::podcast::new_episodes(&p)
        .into_iter()
        .map(|e| e.guid)
        .collect();
    assert_eq!(new, ["c", "d"]);
    // an older episode left out is not new
    lunii_core::podcast::record_imported(&p, &["d".into()], false).unwrap();
    assert!(lunii_core::podcast::new_episodes(&p).is_empty());
    lunii_core::podcast::record_imported(&p, &["a".into(), "b".into()], true).unwrap();

    // followed without import, then no longer followed
    let url2 = "https://example.org/follow-test.xml";
    let p2 = lunii_core::podcast::parse_feed(url2, &feed(&["x"])).unwrap();
    lunii_core::podcast::follow(&p2).unwrap();
    assert!(
        lunii_core::podcast::saved_podcasts()
            .iter()
            .any(|s| s.feed_url == url2)
    );
    assert_eq!(lunii_core::podcast::new_episodes(&p2).len(), 1);
    // following again keeps the imported episodes
    lunii_core::podcast::follow(&p).unwrap();
    assert_eq!(lunii_core::podcast::new_episodes(&p).len(), 2);
    lunii_core::podcast::unfollow(&p2.album_uuid()).unwrap();
    assert!(
        !lunii_core::podcast::saved_podcasts()
            .iter()
            .any(|s| s.feed_url == url2)
    );
}
