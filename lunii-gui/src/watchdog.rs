//! Watches the UI thread: when the window stops responding (the system then
//! says the application froze), the log tells for how long and what it was
//! doing.
//!
//! A thread asks the event loop for an answer every [`TICK`]. An answer
//! arriving after [`SLOW`] or more is logged once the window responds again
//! (panel and file). A window still not responding after [`STUCK`] is written
//! to the file at once: the application may be killed before it answers.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::i18n::trf;

const TICK: Duration = Duration::from_millis(250);
const SLOW: Duration = Duration::from_secs(1);
const STUCK: Duration = Duration::from_secs(5);

/// What the UI thread is doing: the event being handled (`None`: an action
/// of the window, a timer, drawing...).
static HANDLING: Mutex<Option<&'static str>> = Mutex::new(None);

/// The request sent to the event loop, not answered yet.
struct Pending {
    sent: Instant,
    /// what the UI thread was doing once it was late (a tick without answer)
    doing: Option<Option<&'static str>>,
    /// already written to the file as stuck
    stuck: bool,
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

/// Runs `f`, handling the event `name` (named in the log if it blocks).
pub fn handling<R>(name: &'static str, f: impl FnOnce() -> R) -> R {
    let previous = HANDLING.lock().unwrap().replace(name);
    let result = f();
    *HANDLING.lock().unwrap() = previous;
    result
}

fn doing(what: Option<&'static str>) -> String {
    match what {
        Some(event) => trf("while handling {}", &[event]),
        None => crate::i18n::tr("in an action of the window"),
    }
}

/// Starts watching (once the window is shown).
pub fn spawn() {
    let _ = std::thread::Builder::new()
        .name("watchdog".into())
        .spawn(|| {
            loop {
                std::thread::sleep(TICK);
                if !tick() {
                    return;
                }
            }
        });
}

/// `false`: the event loop is gone (application exit).
fn tick() -> bool {
    let mut pending = PENDING.lock().unwrap();
    let Some(p) = pending.as_mut() else {
        *pending = Some(Pending {
            sent: Instant::now(),
            doing: None,
            stuck: false,
        });
        drop(pending);
        return slint::invoke_from_event_loop(answered).is_ok();
    };
    let late = p.sent.elapsed();
    if late >= TICK && p.doing.is_none() {
        p.doing = Some(*HANDLING.lock().unwrap());
    }
    if late >= STUCK && !p.stuck {
        p.stuck = true;
        let what = doing(p.doing.flatten());
        crate::logfile::write(&format!(
            "[WARNING] {}",
            trf(
                "The window has not responded for {} s, {}",
                &[&STUCK.as_secs().to_string(), &what]
            )
        ));
    }
    true
}

/// The event loop answered: logged if late.
fn answered() {
    let Some(p) = PENDING.lock().unwrap().take() else {
        return;
    };
    let late = p.sent.elapsed();
    if late >= SLOW {
        log::warn!(
            target: "lunii",
            "The window did not respond for {:.1} s, {}",
            late.as_secs_f32(),
            doing(p.doing.flatten())
        );
    }
}
