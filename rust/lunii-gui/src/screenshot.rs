//! Development tool: renders the main screens to PNG files without a display
//! (software renderer). Built with `--features screenshot`, run with
//! `LUNII_QT_SCREENSHOT=<output dir>` and `LUNII_QT_DEVICES=<fake device>`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::Rgb8Pixel;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{EventLoopProxy, Platform, PlatformError, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize};

use crate::app::App;
use crate::{DialogKind, MainWindow};

type Queue = Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>;

thread_local! {
    static WINDOWS: RefCell<Vec<Rc<MinimalSoftwareWindow>>> = const { RefCell::new(Vec::new()) };
}

struct ShotPlatform {
    queue: Queue,
    start: Instant,
}

struct Proxy(Queue);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        Ok(())
    }
    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), slint::EventLoopError> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

impl Platform for ShotPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let w = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        WINDOWS.with(|ws| ws.borrow_mut().push(w.clone()));
        Ok(w)
    }
    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }
    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.queue.clone())))
    }
}

/// Processes queued events and timers for `ms` milliseconds.
fn pump(queue: &Queue, ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        let events: Vec<_> = std::mem::take(&mut *queue.lock().unwrap());
        for e in events {
            e();
        }
        slint::platform::update_timers_and_animations();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn render(window: usize, path: &Path) {
    WINDOWS.with(|ws| {
        let w = ws.borrow()[window].clone();
        w.request_redraw();
        let size = w.size();
        let mut buffer = vec![Rgb8Pixel::default(); (size.width * size.height) as usize];
        w.draw_if_needed(|renderer| {
            renderer.render(&mut buffer, size.width as usize);
        });
        let raw: Vec<u8> = buffer.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        image::RgbImage::from_raw(size.width, size.height, raw)
            .unwrap()
            .save(path)
            .unwrap();
        println!("{}", path.display());
    });
}

pub fn run(out: PathBuf) -> anyhow::Result<()> {
    let queue: Queue = Arc::default();
    slint::platform::set_platform(Box::new(ShotPlatform {
        queue: queue.clone(),
        start: Instant::now(),
    }))
    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    std::fs::create_dir_all(&out)?;

    let ui = MainWindow::new()?;
    let lang = crate::i18n::apply_language();
    ui.window().set_size(PhysicalSize::new(900, 650));
    let app = App::new(&ui);
    ui.show()?;
    app.start();
    pump(&queue, 300);
    if ui.get_device_index() < 0 && app_devices(&ui) > 0 {
        ui.set_device_index(0);
        app.select_device(0);
    }
    pump(&queue, 500);
    let shot = |name: &str| out.join(format!("{lang}-{name}.png"));
    render(0, &shot("01-main"));

    app.row_pointer(0, false, false, false);
    pump(&queue, 500);
    render(0, &shot("02-details"));

    app.row_pointer(2, true, false, false);
    pump(&queue, 200);
    render(0, &shot("03-multi-selection"));

    ui.set_filter("studio".into());
    ui.invoke_filter_edited();
    pump(&queue, 200);
    render(0, &shot("04-filter"));
    ui.set_filter("".into());
    ui.invoke_filter_edited();

    ui.set_show_size(true);
    app.action("show-size");
    pump(&queue, 800);
    render(0, &shot("05-sizes"));

    app.row_pointer(1, false, false, false);
    app.action("remove");
    pump(&queue, 200);
    render(0, &shot("06-confirm-remove"));
    ui.set_dialog(DialogKind::None);

    app.action("night-mode-settings");
    pump(&queue, 200);
    render(0, &shot("07-night-mode"));
    ui.set_dialog(DialogKind::None);

    app.action("about");
    pump(&queue, 200);
    render(0, &shot("08-about"));
    ui.set_dialog(DialogKind::None);

    app.action("get-firmware");
    pump(&queue, 200);
    render(0, &shot("09-login"));
    ui.set_dialog(DialogKind::None);

    app.show_log();
    pump(&queue, 300);
    render(0, &shot("10-log"));

    // a long task: progress bars
    app.row_pointer(0, false, false, false);
    app.action("hide");
    pump(&queue, 300);
    render(0, &shot("11-hidden-story"));

    // progress area layout (events as sent by a running task)
    ui.set_busy(true);
    ui.set_abort_visible(true);
    app.handle_event(crate::worker::Event::Total(1, 3));
    app.handle_event(crate::worker::Event::Story("1A2B3C4D".into(), 4, 10));
    app.handle_event(crate::worker::Event::File("1,234 KB/s".into(), 60, 100));
    ui.set_eta_text(crate::i18n::trf("Time left: {}", &["3 min 20 s"]).into());
    pump(&queue, 100);
    render(0, &shot("12-progress"));
    app.handle_event(crate::worker::Event::Finished);

    // real import through the UI
    if let Some(list) = std::env::var_os("LUNII_QT_SHOT_IMPORT") {
        app.import(std::env::split_paths(&list).collect());
        let end = Instant::now() + Duration::from_secs(30);
        pump(&queue, 100);
        while ui.get_busy() && Instant::now() < end {
            pump(&queue, 100);
        }
        render(0, &shot("13-import-done"));
    }

    // second device (Flam)
    if app_devices(&ui) > 1 {
        ui.set_device_index(1);
        app.select_device(1);
        pump(&queue, 300);
        render(0, &shot("14-second-device"));
    }

    Ok(())
}

fn app_devices(ui: &MainWindow) -> usize {
    use slint::Model;
    ui.get_devices().row_count()
}
