//! End to end flows on fake devices (no network, isolated configuration directory).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use lunii_core::constants::*;
use lunii_core::{Context, Device, db, open_device};
use uuid::Uuid;

mod common;
use common::{flam, lunii_v2};

/// Shared configuration directory for the whole test binary.
fn setup() -> &'static Path {
    static CFG: OnceLock<tempfile::TempDir> = OnceLock::new();
    CFG.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        set_cfg_dir(dir.path());
        std::fs::write(dir.path().join("official.db"), OFFICIAL_DB).unwrap();
        std::fs::write(dir.path().join("third-party.db"), "{}").unwrap();
        db::load_db(false);
        dir
    })
    .path()
}

const OFFICIAL_UUID: &str = "0ff1c1a1-0000-4000-8000-0000000000aa";
const OFFICIAL_DB: &str = r#"{"x": {"uuid": "0ff1c1a1-0000-4000-8000-0000000000aa",
    "locales_available": {"fr_FR": {}},
    "localized_infos": {"fr_FR": {"title": "Officielle", "description": "<p>Une histoire</p>"}}}}"#;

fn ctx() -> Context {
    Context::default()
}

fn lunii_v3(dir: &Path, name: &str) -> PathBuf {
    let mut md = vec![0u8; 128];
    md[0] = 7;
    md[2..7].copy_from_slice(b"3.2.2");
    md[0x1A..0x1A + 14].copy_from_slice(b"23023030099999");
    for (i, b) in md[0x40..0x60].iter_mut().enumerate() {
        *b = i as u8 * 7;
    }
    let root = dir.join(name);
    std::fs::create_dir_all(root.join(".content")).unwrap();
    std::fs::write(root.join(".md"), md).unwrap();
    root
}

fn plain_story(dir: &Path, uuid: Uuid, title: &str) -> PathBuf {
    let path = dir.join(format!("{title}.plain.pk"));
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    let mut ni = vec![0u8; 0x200 + 0x2C];
    ni[0] = 1;
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("uuid.bin", uuid.as_bytes().to_vec()),
        ("ri.plain", b"000\\AAAAAAAA".to_vec()),
        ("si.plain", b"000\\BBBBBBBB".to_vec()),
        ("li.plain", vec![0u8; 8]),
        ("ni", ni),
        (
            "rf/000/AAAAAAAA.bmp",
            (0..1500u32).map(|i| (i * 13) as u8).collect(),
        ),
        (
            "sf/000/BBBBBBBB.mp3",
            (0..3000u32).map(|i| (i * 7) as u8).collect(),
        ),
        (
            "_metadata.json",
            format!(r#"{{"uuid": "{uuid}", "title": "{title}", "description": "d"}}"#).into_bytes(),
        ),
    ];
    for (name, data) in files {
        zip.start_file(name, opts).unwrap();
        zip.write_all(&data).unwrap();
    }
    zip.finish().unwrap();
    path
}

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            (
                e.path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn v2_plain_export_reimport_is_identical() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let uuid = Uuid::parse_str("12345678-aaaa-4bbb-8ccc-0000deadbeef").unwrap();
    let archive = plain_story(tmp.path(), uuid, "Roundtrip");

    let root = lunii_v2(tmp.path());
    let mut dev = open_device(&ctx(), &root).unwrap();
    assert_eq!(dev.version(), DeviceVersion::LuniiV2);
    dev.import_story(&ctx(), &archive).unwrap();
    assert_eq!(dev.stories()[0].name(), "Roundtrip");
    assert!(
        dev.import_story(&ctx(), &archive).is_err(),
        "already loaded"
    );

    let out = tmp.path().join("export");
    std::fs::create_dir_all(&out).unwrap();
    let exported = dev.export_story(&ctx(), "DEADBEEF", &out).unwrap();
    assert!(
        exported
            .to_string_lossy()
            .ends_with("Roundtrip.DEADBEEF.plain.pk")
    );

    // re-import on another (identical) device
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let root2 = lunii_v2(&other);
    let mut dev2 = open_device(&ctx(), &root2).unwrap();
    dev2.import_story(&ctx(), &exported).unwrap();
    assert_eq!(tree(&root.join(".content")), tree(&root2.join(".content")));
}

#[test]
fn v3_backup_restore_goes_to_short_uuid_dir() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let uuid = Uuid::parse_str("87654321-aaaa-4bbb-8ccc-0000cafe0001").unwrap();
    let archive = plain_story(tmp.path(), uuid, "Backup");

    let root = lunii_v3(tmp.path(), "v3a");
    let mut dev = open_device(&ctx(), &root).unwrap();
    assert!(dev.can_import());
    dev.import_story(&ctx(), &archive).unwrap();

    // no device keys: personal backup export
    let out = tmp.path().join("export");
    std::fs::create_dir_all(&out).unwrap();
    let backup = dev.export_story(&ctx(), "CAFE0001", &out).unwrap();
    assert!(
        backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("23023030099999.")
    );

    let root2 = lunii_v3(tmp.path(), "v3b");
    let mut dev2 = open_device(&ctx(), &root2).unwrap();
    dev2.import_story(&ctx(), &backup).unwrap();
    assert!(root2.join(".content/CAFE0001/ri").is_file());
    assert_eq!(tree(&root.join(".content")), tree(&root2.join(".content")));
}

#[test]
fn hide_move_nm_remove_recover_cleanup() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let ids: Vec<Uuid> = (1..=3u8)
        .map(|i| Uuid::parse_str(&format!("aaaaaaaa-0000-4000-8000-0000000000{i:02}")).unwrap())
        .collect();
    let root = lunii_v2(tmp.path());
    std::fs::write(root.join(".cfg"), [0x00, 0x01]).unwrap();
    let mut dev = open_device(&ctx(), &root).unwrap();
    for (i, id) in ids.iter().enumerate() {
        dev.import_story(&ctx(), &plain_story(tmp.path(), *id, &format!("S{i}")))
            .unwrap();
    }
    let order = |d: &dyn Device| d.stories().iter().map(|s| s.uuid).collect::<Vec<_>>();

    // move the last one to the top
    dev.move_stories(&[ids[2]], -2).unwrap();
    assert_eq!(order(dev.as_ref()), vec![ids[2], ids[0], ids[1]]);
    let pi = std::fs::read(root.join(".pi")).unwrap();
    assert_eq!(&pi[..16], ids[2].as_bytes());
    // dragged: the first one before the last one, then back to the top
    dev.move_stories_before(&[ids[2]], Some(ids[1])).unwrap();
    assert_eq!(order(dev.as_ref()), vec![ids[0], ids[2], ids[1]]);
    dev.move_stories_before(&[ids[2]], Some(ids[0])).unwrap();
    assert_eq!(order(dev.as_ref()), vec![ids[2], ids[0], ids[1]]);
    let pi = std::fs::read(root.join(".pi")).unwrap();
    assert_eq!(&pi[..16], ids[2].as_bytes());

    // hide the first imported one
    dev.toggle_hidden(&ctx(), &[ids[0]]).unwrap();
    assert!(root.join(".content.hidden/00000001").is_dir());
    assert!(!root.join(".content/00000001").exists());
    assert_eq!(
        std::fs::read(root.join(".pi.hidden")).unwrap(),
        ids[0].as_bytes()
    );

    // night mode flag (the archives have no nm file)
    dev.as_lunii_mut()
        .unwrap()
        .toggle_night_mode(&[ids[1], ids[0]])
        .unwrap();
    assert!(root.join(".content/00000002/nm").is_file());

    // reopening keeps everything (hidden stories come from .pi.hidden, read last)
    let mut dev = open_device(&ctx(), &root).unwrap();
    assert_eq!(order(dev.as_ref()), vec![ids[2], ids[1], ids[0]]);
    assert!(dev.stories()[2].hidden);
    assert!(dev.stories()[1].nm);
    assert!(dev.stories()[2].nm, "night mode of a hidden story is kept");

    // remove
    dev.remove_story(&ctx(), "00000003").unwrap();
    assert!(!root.join(".content/00000003").exists());
    assert_eq!(dev.stories().len(), 2);

    // lost index: recover finds the two remaining story dirs
    std::fs::remove_file(root.join(".pi")).unwrap();
    std::fs::remove_file(root.join(".pi.hidden")).unwrap();
    let mut dev = open_device(&ctx(), &root).unwrap();
    assert_eq!(dev.stories().len(), 0);
    assert_eq!(dev.recover_stories(&ctx(), true).unwrap(), 2);
    assert_eq!(dev.stories().len(), 0, "dry run");
    assert_eq!(dev.recover_stories(&ctx(), false).unwrap(), 2);
    assert_eq!(dev.stories().len(), 2);
    assert!(dev.stories().iter().any(|s| s.hidden));

    // cleanup removes directories missing from the index
    std::fs::create_dir_all(root.join(".content/0BADF00D/rf")).unwrap();
    std::fs::write(root.join(".content/0BADF00D/ri"), vec![0u8; 2048 * 1024]).unwrap();
    let (removed, mb) = dev.cleanup_stories(&ctx()).unwrap();
    assert_eq!((removed, mb), (1, 2));
    assert!(!root.join(".content/0BADF00D").exists());
    assert!(root.join(".content/00000002").exists());
}

#[test]
fn night_mode_config_roundtrip() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = lunii_v2(tmp.path());
    // written by a firmware handling the night mode: version, (index, value)
    std::fs::write(root.join(".cfg"), [0x00, 0x01, 0x03, 0x00, 0x00, 0x00]).unwrap();
    let mut dev = open_device(&ctx(), &root).unwrap();
    let lunii = dev.as_lunii_mut().unwrap();
    assert!(lunii.night_mode_supported());
    assert!(!lunii.night_mode_enabled());
    lunii.config[LUNII_CFGPOS_NM_ENABLED] = 1;
    lunii.config[LUNII_CFGPOS_NM_STORYCOUNT] = 7;
    lunii.update_config().unwrap();

    let dev = open_device(&ctx(), &root).unwrap();
    let lunii = dev.as_lunii().unwrap();
    assert!(lunii.night_mode_enabled());
    assert_eq!(lunii.config[LUNII_CFGPOS_NM_STORYCOUNT], 7);
    let cfg = std::fs::read(root.join(".cfg")).unwrap();
    assert_eq!(&cfg[..2], &[0x00, 0x01]);
    assert_eq!(cfg.len(), 2 + 4 * 10);
}

#[test]
fn night_mode_unavailable_without_config_file() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = lunii_v2(tmp.path());
    let mut dev = open_device(&ctx(), &root).unwrap();
    dev.import_story(
        &ctx(),
        &plain_story(
            tmp.path(),
            Uuid::parse_str("aaaaaaaa-0000-4000-8000-000000000042").unwrap(),
            "Histoire",
        ),
    )
    .unwrap();
    let uuid = dev.stories()[0].uuid;
    let lunii = dev.as_lunii_mut().unwrap();
    assert!(!lunii.night_mode_supported());
    assert!(!lunii.night_mode_enabled());
    assert!(lunii.update_config().is_err());
    assert!(lunii.toggle_night_mode(&[uuid]).is_err());
    assert!(!root.join(".cfg").exists());

    // unknown format: not offered either
    std::fs::write(root.join(".cfg"), [0x00, 0x02]).unwrap();
    let dev = open_device(&ctx(), &root).unwrap();
    assert!(!dev.as_lunii().unwrap().night_mode_supported());
}

#[test]
fn official_stories_cannot_be_exported_from_lunii_v2() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let uuid = Uuid::parse_str(OFFICIAL_UUID).unwrap();
    let root = lunii_v2(tmp.path());
    let mut dev = open_device(&ctx(), &root).unwrap();
    dev.import_story(&ctx(), &plain_story(tmp.path(), uuid, "Officielle"))
        .unwrap();

    let story = dev.stories()[0].clone();
    assert!(story.is_official());
    assert_eq!(story.name(), "Officielle");
    assert_eq!(story.description(), "Une histoire</p>");
    assert!(!dev.story_exportable(&story));
    let out = tmp.path().join("export");
    std::fs::create_dir_all(&out).unwrap();
    assert!(dev.export_story(&ctx(), "000000AA", &out).is_err());
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
}

#[test]
fn official_stories_are_only_backed_up_from_flam() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = flam(tmp.path());
    let mut dev = open_device(&ctx(), &root).unwrap();
    assert_eq!(dev.version(), DeviceVersion::FlamV1);
    let official = Uuid::parse_str(OFFICIAL_UUID).unwrap();
    let custom = Uuid::parse_str("cccccccc-0000-4000-8000-0000000000cc").unwrap();
    for (uuid, title) in [(official, "Officielle"), (custom, "Perso")] {
        dev.import_story(&ctx(), &plain_story(tmp.path(), uuid, title))
            .unwrap();
    }
    let out = tmp.path().join("export");
    std::fs::create_dir_all(&out).unwrap();

    // the device keys are known: a custom story is exported in plain...
    let plain = dev.export_story(&ctx(), "000000CC", &out).unwrap();
    assert!(!plain.to_string_lossy().contains(&dev.snu_str()));
    // ...but an official one stays ciphered (`<SNU>.<name>.<short uuid>.zip`)
    let backup = dev.export_story(&ctx(), "000000AA", &out).unwrap();
    let name = backup.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with(&dev.snu_str()), "{name}");
    assert!(name.ends_with(".zip"), "{name}");
    let zip = zip::ZipArchive::new(std::fs::File::open(&backup).unwrap()).unwrap();
    assert!(
        zip.file_names()
            .all(|n| n.unwrap().starts_with(&official.to_string()))
    );
}

#[test]
fn abort_cleans_partial_import() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let uuid = Uuid::parse_str("bbbbbbbb-0000-4000-8000-0000000000ff").unwrap();
    let archive = plain_story(tmp.path(), uuid, "Aborted");
    let root = lunii_v2(tmp.path());
    let mut dev = open_device(&ctx(), &root).unwrap();

    let ctx = ctx();
    ctx.request_abort();
    let err = dev.import_story(&ctx, &archive).unwrap_err();
    assert!(err.is::<lunii_core::Aborted>());
    assert!(!root.join(".content/000000FF").exists());
    assert!(dev.stories().is_empty());
}

/// Flam story in plain format: a script, its info and a sound.
fn flam_plain_story(dir: &Path, uuid: Uuid, title: &str) -> PathBuf {
    let path = dir.join(format!("{title}.plain.pk"));
    write_zip(
        &path,
        &[
            ("uuid.bin", uuid.as_bytes().to_vec()),
            ("main.lua", (0..64u8).collect()),
            ("info.plain", (0..48u8).map(|i| i * 3).collect()),
            ("sounds/a.mp3", (0..100u8).collect()),
            (
                "_metadata.json",
                format!(r#"{{"uuid": "{uuid}", "title": "{title}", "description": "d"}}"#)
                    .into_bytes(),
            ),
        ],
    );
    path
}

/// `archive` with a thumbnail added (written in the device's `img/` on a Flam).
fn with_thumbnail(archive: PathBuf) -> PathBuf {
    let mut files: Vec<(String, Vec<u8>)> = zip_entries(&archive).into_iter().collect();
    let mut png = Vec::new();
    image::RgbImage::from_pixel(8, 8, image::Rgb([200, 100, 50]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    files.push((FILE_THUMB.to_string(), png));
    let refs: Vec<(&str, Vec<u8>)> = files.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
    write_zip(&archive, &refs);
    archive
}

fn write_zip(path: &Path, files: &[(&str, Vec<u8>)]) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, data) in files {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

/// Zip of a story directory, entries under `prefix/`, top level files first
/// (the UUID is read in the first entry).
fn zip_story_dir(path: &Path, story_dir: &Path, prefix: &str) {
    let mut files: Vec<(String, Vec<u8>)> = tree(story_dir)
        .into_iter()
        .map(|(rel, data)| (rel.replace('\\', "/"), data))
        .collect();
    files.sort_by_key(|(rel, _)| (rel.matches('/').count(), rel.clone()));
    let files: Vec<(String, Vec<u8>)> = files
        .into_iter()
        .map(|(rel, data)| (format!("{prefix}/{rel}"), data))
        .collect();
    let refs: Vec<(&str, Vec<u8>)> = files.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
    write_zip(path, &refs);
}

fn zip_entries(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    (0..zip.len())
        .map(|i| {
            let mut f = zip.by_index(i).unwrap();
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut data).unwrap();
            (f.name().unwrap().to_string(), data)
        })
        .collect()
}

fn out_dir(dir: &Path, name: &str) -> PathBuf {
    let out = dir.join(name);
    std::fs::create_dir_all(&out).unwrap();
    out
}

/// A v2 archive (story directory of a Lunii v2, ciphered) imported on
/// another Lunii v2 and on a Flam: same files, same plain export.
#[test]
fn v2_archive_imports_on_lunii_and_flam() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let uuid = Uuid::parse_str("12345678-aaaa-4bbb-8ccc-0000000000b2").unwrap();
    let plain = plain_story(tmp.path(), uuid, "Archive v2");
    let root = lunii_v2(&tmp.path().join("a"));
    let mut dev = open_device(&ctx(), &root).unwrap();
    dev.import_story(&ctx(), &plain).unwrap();

    let v2 = tmp.path().join("Archive.v2.pk");
    zip_story_dir(&v2, &root.join(".content/000000B2"), &uuid.to_string());

    let root2 = lunii_v2(&tmp.path().join("b"));
    let mut dev2 = open_device(&ctx(), &root2).unwrap();
    dev2.import_story(&ctx(), &v2).unwrap();
    assert_eq!(tree(&root.join(".content")), tree(&root2.join(".content")));

    let root3 = flam(&tmp.path().join("c"));
    let mut dev3 = open_device(&ctx(), &root3).unwrap();
    dev3.import_story(&ctx(), &v2).unwrap();
    let from_lunii = dev
        .export_story(&ctx(), "000000B2", &out_dir(tmp.path(), "out_a"))
        .unwrap();
    let from_flam = dev3
        .export_story(&ctx(), "000000B2", &out_dir(tmp.path(), "out_c"))
        .unwrap();
    // the same, the UUID in hexadecimal under a parent directory
    let hex = tmp.path().join("Archive hex.v2.pk");
    let prefix = format!("sauvegarde/{}", uuid.simple().to_string().to_uppercase());
    zip_story_dir(&hex, &root.join(".content/000000B2"), &prefix);
    let root4 = lunii_v2(&tmp.path().join("d"));
    let mut dev4 = open_device(&ctx(), &root4).unwrap();
    dev4.import_story(&ctx(), &hex).unwrap();
    assert_eq!(tree(&root.join(".content")), tree(&root4.join(".content")));
    let root5 = flam(&tmp.path().join("e"));
    let mut dev5 = open_device(&ctx(), &root5).unwrap();
    dev5.import_story(&ctx(), &hex).unwrap();
    assert_eq!(tree(&root3.join("str")), tree(&root5.join("str")));

    // on the Flam, the ciphered files of a Lunii story are padded to 16 bytes
    let (lunii, flam) = (zip_entries(&from_lunii), zip_entries(&from_flam));
    assert_eq!(
        lunii.keys().collect::<Vec<_>>(),
        flam.keys().collect::<Vec<_>>()
    );
    for (name, data) in &lunii {
        let padded = &flam[name];
        assert!(padded.starts_with(data), "{name}");
        assert!(padded[data.len()..].iter().all(|&b| b == 0), "{name}");
        assert!(padded.len() - data.len() < 16, "{name}");
    }
}

/// A Flam story exported in plain, then imported on another Flam.
#[test]
fn flam_plain_export_reimport_is_identical() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let uuid = Uuid::parse_str("cccccccc-0000-4000-8000-0000000000f1").unwrap();
    let archive = flam_plain_story(tmp.path(), uuid, "Flam en clair");
    let root = flam(&tmp.path().join("a"));
    let mut dev = open_device(&ctx(), &root).unwrap();
    dev.import_story(&ctx(), &archive).unwrap();
    let exported = dev
        .export_story(&ctx(), "000000F1", &out_dir(tmp.path(), "out"))
        .unwrap();
    assert!(exported.to_string_lossy().ends_with(".plain.pk"));

    let root2 = flam(&tmp.path().join("b"));
    let mut dev2 = open_device(&ctx(), &root2).unwrap();
    dev2.import_story(&ctx(), &exported).unwrap();
    assert_eq!(tree(&root.join("str")), tree(&root2.join("str")));
}

/// Backups of official stories on a Flam (a Lunii story, then a Flam story)
/// put back after their removal: same files.
#[test]
fn flam_backups_restore_identically() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let official = Uuid::parse_str(OFFICIAL_UUID).unwrap();
    let root = flam(tmp.path());
    let mut dev = open_device(&ctx(), &root).unwrap();
    for (index, archive) in [
        with_thumbnail(plain_story(tmp.path(), official, "Officielle")),
        flam_plain_story(tmp.path(), official, "Officielle Flam"),
    ]
    .iter()
    .enumerate()
    {
        dev.import_story(&ctx(), archive).unwrap();
        let before = tree(&root.join("str"));
        let backup = dev
            .export_story(
                &ctx(),
                "000000AA",
                &out_dir(tmp.path(), &format!("out{index}")),
            )
            .unwrap();
        dev.remove_story(&ctx(), "000000AA").unwrap();
        dev.import_story(&ctx(), &backup)
            .unwrap_or_else(|e| panic!("restore {index}: {e:#}"));
        assert_eq!(before, tree(&root.join("str")), "restore {index}");
        dev.remove_story(&ctx(), "000000AA").unwrap();
    }
}
