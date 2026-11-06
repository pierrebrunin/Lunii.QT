//! Structure of the album stories (STUdio `story.json`): the nodes and
//! transitions of the menus and tracks.

use serde_json::{Value, json};
use uuid::Uuid;

use super::{CHAPTER_PREFIX, COVER_IMAGE, COVER_SOUND, Chapter, track_asset};

/// Structure of an album story (`story.json`) for these tracks: the cover, the
/// menu (one entry per track) and the playback nodes. Only depends on the
/// number of tracks for the device index files, so tracks can be appended to
/// an album on a device by regenerating them.
pub fn story_json(uuid: Uuid, title: &str, description: &str, track_titles: &[&str]) -> Value {
    let n = track_titles.len();
    // stable node UUIDs, derived from the story one
    let node = |kind: &str, i: usize| Uuid::new_v5(&uuid, format!("{kind}{i}").as_bytes());
    let menu_uuid = |i| node("menu", i).to_string();
    let play_uuid = |i| node("play", i).to_string();
    let controls = |wheel, ok, home, pause, autoplay| json!({"wheel": wheel, "ok": ok, "home": home, "pause": pause, "autoplay": autoplay});
    let to = |action: String, option: usize| json!({"actionNode": action, "optionIndex": option});

    let mut stage_nodes = vec![json!({
        "uuid": uuid.to_string(),
        "type": "cover",
        "name": title,
        "image": COVER_IMAGE,
        "audio": COVER_SOUND,
        "okTransition": to("menu".into(), 0),
        "homeTransition": null,
        "controlSettings": controls(true, true, false, false, false),
        "squareOne": true,
    })];
    for (i, track) in track_titles.iter().enumerate() {
        stage_nodes.push(json!({
            "uuid": menu_uuid(i),
            "name": track,
            "image": track_asset("IMG", i, "png"),
            "audio": COVER_SOUND,
            "okTransition": to(format!("play{i}"), 0),
            "homeTransition": null,
            "controlSettings": controls(true, true, true, false, false),
        }));
    }
    for (i, track) in track_titles.iter().enumerate() {
        // end of the track (or OK): next one, the menu after the last
        let next = if i + 1 < n {
            to(format!("play{}", i + 1), 0)
        } else {
            to("menu".into(), 0)
        };
        stage_nodes.push(json!({
            "uuid": play_uuid(i),
            "name": track,
            "image": track_asset("IMG", i, "png"),
            "audio": track_asset("TRK", i, "mp3"),
            "okTransition": next,
            "homeTransition": to("menu".into(), i),
            "controlSettings": controls(false, true, true, true, true),
        }));
    }

    let mut action_nodes = vec![json!({
        "id": "menu",
        "options": (0..n).map(menu_uuid).collect::<Vec<_>>(),
    })];
    for i in 0..n {
        action_nodes.push(json!({"id": format!("play{i}"), "options": [play_uuid(i)]}));
    }

    json!({
        "format": "v1",
        "version": 1,
        "title": title,
        "description": description,
        "nightModeAvailable": false,
        "stageNodes": stage_nodes,
        "actionNodes": action_nodes,
    })
}

/// Structure of an album made of chapters (sub-folders): the cover, the menu
/// of the chapters, a menu of the tracks of each chapter and the playback
/// nodes. The tracks follow each other across the chapters.
pub fn chapters_story_json(
    uuid: Uuid,
    title: &str,
    description: &str,
    chapters: &[Chapter],
    track_titles: &[&str],
) -> Value {
    let n = track_titles.len();
    let node =
        |kind: &str, i: usize| Uuid::new_v5(&uuid, format!("{kind}{i}").as_bytes()).to_string();
    let controls = |wheel, ok, home, pause, autoplay| json!({"wheel": wheel, "ok": ok, "home": home, "pause": pause, "autoplay": autoplay});
    let to = |action: String, option: usize| json!({"actionNode": action, "optionIndex": option});

    // chapter and position in the chapter of each track
    let mut position = Vec::with_capacity(n);
    for (c, ch) in chapters.iter().enumerate() {
        position.extend((0..ch.len).map(|i| (c, i)));
    }
    let first_track = |c: usize| chapters[..c].iter().map(|ch| ch.len).sum::<usize>();

    let mut stage_nodes = vec![json!({
        "uuid": uuid.to_string(),
        "type": "cover",
        "name": title,
        "image": COVER_IMAGE,
        "audio": COVER_SOUND,
        "okTransition": to("chapters".into(), 0),
        "homeTransition": null,
        "controlSettings": controls(true, true, false, false, false),
        "squareOne": true,
    })];
    for (c, ch) in chapters.iter().enumerate() {
        stage_nodes.push(json!({
            "uuid": node("chapter", c),
            "name": ch.title,
            "image": track_asset(CHAPTER_PREFIX, c, "png"),
            "audio": COVER_SOUND,
            "okTransition": to(format!("tracks{c}"), 0),
            "homeTransition": null,
            "controlSettings": controls(true, true, true, false, false),
        }));
    }
    for (i, track) in track_titles.iter().enumerate() {
        let (c, _) = position[i];
        stage_nodes.push(json!({
            "uuid": node("menu", i),
            "name": track,
            "image": track_asset("IMG", i, "png"),
            "audio": COVER_SOUND,
            "okTransition": to(format!("play{i}"), 0),
            "homeTransition": to("chapters".into(), c),
            "controlSettings": controls(true, true, true, false, false),
        }));
    }
    for (i, track) in track_titles.iter().enumerate() {
        let (c, k) = position[i];
        // end of the track (or OK): next one, the chapters menu after the last
        let next = if i + 1 < n {
            to(format!("play{}", i + 1), 0)
        } else {
            to("chapters".into(), 0)
        };
        stage_nodes.push(json!({
            "uuid": node("play", i),
            "name": track,
            "image": track_asset("IMG", i, "png"),
            "audio": track_asset("TRK", i, "mp3"),
            "okTransition": next,
            "homeTransition": to(format!("tracks{c}"), k),
            "controlSettings": controls(false, true, true, true, true),
        }));
    }

    let mut action_nodes = vec![json!({
        "id": "chapters",
        "options": (0..chapters.len()).map(|c| node("chapter", c)).collect::<Vec<_>>(),
    })];
    for (c, ch) in chapters.iter().enumerate() {
        let start = first_track(c);
        action_nodes.push(json!({
            "id": format!("tracks{c}"),
            "options": (start..start + ch.len).map(|i| node("menu", i)).collect::<Vec<_>>(),
        }));
    }
    for i in 0..n {
        action_nodes.push(json!({"id": format!("play{i}"), "options": [node("play", i)]}));
    }

    json!({
        "format": "v1",
        "version": 1,
        "title": title,
        "description": description,
        "nightModeAvailable": false,
        "stageNodes": stage_nodes,
        "actionNodes": action_nodes,
    })
}
