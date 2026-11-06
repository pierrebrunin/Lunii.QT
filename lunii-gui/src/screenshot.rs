//! Development tool: renders the main screens to PNG files without a display
//! (software renderer). Built with `--features screenshot`, run with
//! `LUNII_SCREENSHOT=<output dir>` and `LUNII_DEVICES=<fake device>`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::Rgb8Pixel;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{EventLoopProxy, Platform, PlatformError, WindowAdapter};
use slint::{ComponentHandle, Model, PhysicalSize};

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
    app.action(crate::Action::ShowSize);
    pump(&queue, 800);
    render(0, &shot("05-sizes"));

    app.row_pointer(1, false, false, false);
    app.action(crate::Action::Remove);
    pump(&queue, 200);
    render(0, &shot("06-confirm-remove"));
    ui.set_dialog(DialogKind::None);

    app.action(crate::Action::NightModeSettings);
    pump(&queue, 200);
    render(0, &shot("07-night-mode"));
    ui.set_dialog(DialogKind::None);

    app.action(crate::Action::About);
    pump(&queue, 200);
    render(0, &shot("08-about"));
    ui.set_dialog(DialogKind::None);

    app.action(crate::Action::DeviceInfo);
    pump(&queue, 200);
    render(0, &shot("08-device-info"));
    ui.set_dialog(DialogKind::None);

    app.action(crate::Action::GetFirmware);
    pump(&queue, 200);
    render(0, &shot("09-login"));
    ui.set_dialog(DialogKind::None);

    app.show_log();
    pump(&queue, 300);
    render(0, &shot("10-log"));

    // a long task: progress bars
    app.row_pointer(0, false, false, false);
    app.action(crate::Action::Hide);
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

    // Podcasts view, a followed podcast from a feed file (LUNII_SHOT_FEED)
    if let Some(feed) = std::env::var_os("LUNII_SHOT_FEED") {
        let xml = std::fs::read_to_string(feed)?;
        let url = "https://example.org/feed.xml";
        let podcast = lunii_core::podcast::parse_feed(url, &xml)?;
        lunii_core::podcast::follow(&podcast)?;
        app.handle_event(crate::worker::Event::Feed(
            url.into(),
            Ok(Box::new(podcast)),
        ));
        ui.set_view(1);
        ui.invoke_view_changed();
        pump(&queue, 200);
        render(0, &shot("14-podcasts"));
        let index = lunii_core::podcast::saved_podcasts()
            .iter()
            .position(|p| p.feed_url == url)
            .unwrap_or(0);
        ui.invoke_podcast_select(index as i32);
        let n = ui.get_podcast_rows().row_count();
        for row in [0, 2, n - 1] {
            let index = ui.get_podcast_rows().row_data(row).unwrap().index;
            ui.invoke_podcast_toggle(index);
        }
        pump(&queue, 300);
        render(0, &shot("14-podcast-episodes"));
        ui.set_view(0);
        ui.invoke_view_changed();
    }

    // real import through the UI
    if let Some(list) = std::env::var_os("LUNII_SHOT_IMPORT") {
        app.import(std::env::split_paths(&list).collect());
        let end = Instant::now() + Duration::from_secs(30);
        pump(&queue, 100);
        while ui.get_busy() && Instant::now() < end {
            pump(&queue, 100);
        }
        render(0, &shot("13-import-done"));

        // an imported album: its tracks in the details panel, edited there
        let last = ui.get_rows().row_count();
        if last > 0 {
            ui.invoke_row_pointer(last as i32 - 1, false, false, false);
            pump(&queue, 300);
            render(0, &shot("15-album-details"));
            if ui.get_album_editable() {
                ui.set_album_current(2);
                ui.invoke_album_move(-1);
                ui.invoke_album_remove();
                pump(&queue, 100);
                render(0, &shot("16-album-modified"));
                ui.invoke_album_apply();
                pump(&queue, 100);
                let end = Instant::now() + Duration::from_secs(30);
                while ui.get_busy() && Instant::now() < end {
                    pump(&queue, 100);
                }
                pump(&queue, 300);
                render(0, &shot("17-album-edited"));
            }
        }
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
