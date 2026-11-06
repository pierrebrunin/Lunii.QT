//! 7z reading (sevenz-rust2), on an archive built by the test itself.

use lunii_core::archive::Archive;

#[test]
fn read_7z_archive() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(src.join("assets")).unwrap();
    std::fs::write(src.join("story.json"), br#"{"title":"7z"}"#).unwrap();
    let audio: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(src.join("assets").join("a.mp3"), &audio).unwrap();

    let path = tmp.path().join("story.7z");
    sevenz_rust2::compress_to_path(&src, &path).unwrap();

    let archive = Archive::open(&path).unwrap();
    assert!(archive.contains("story.json"));
    assert!(archive.contains("assets/a.mp3"));
    assert_eq!(archive.size("assets/a.mp3"), Some(audio.len() as u64));
    assert_eq!(archive.read("story.json").unwrap(), br#"{"title":"7z"}"#);
    assert_eq!(archive.read("assets/a.mp3").unwrap(), audio);
    assert!(archive.read("missing").is_err());
}
