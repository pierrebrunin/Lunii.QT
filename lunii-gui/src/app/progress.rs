//! Tasks: their launch, progress and remaining time, abort.

use super::*;

/// Remaining time estimation of an import, from the bytes written to the device.
pub(super) struct Eta {
    start: Instant,
    /// bytes expected for the whole import
    total: u64,
    /// `Context::progress()` when the import started
    base: u64,
    /// (time, bytes written) over the last `ETA_WINDOW`
    samples: VecDeque<(Instant, u64)>,
}

/// The speed is measured over this window: reactive, but smoothing the
/// pauses (file closing, audio conversion...).
pub(super) const ETA_WINDOW: Duration = Duration::from_secs(20);

/// No estimation before this delay: the first seconds aren't representative.
pub(super) const ETA_WARMUP: Duration = Duration::from_secs(4);

impl Eta {
    /// Remaining duration, `None` while it can't be estimated yet.
    fn estimate(&mut self, now: Instant, done: u64) -> Option<Duration> {
        self.samples.push_back((now, done));
        while self.samples.len() > 2 && now.duration_since(self.samples[0].0) > ETA_WINDOW {
            self.samples.pop_front();
        }
        let elapsed = now.duration_since(self.start);
        if elapsed < ETA_WARMUP || done == 0 {
            return None;
        }
        let (t0, d0) = self.samples[0];
        let window = now.duration_since(t0).as_secs_f64();
        let mut rate = if window > 1.0 {
            (done - d0) as f64 / window
        } else {
            0.0
        };
        if rate <= 0.0 {
            // nothing written lately (transcoding...): average since the start
            rate = done as f64 / elapsed.as_secs_f64();
        }
        let remaining = self.total.saturating_sub(done) as f64;
        Some(Duration::from_secs_f64(remaining / rate))
    }
}

/// `45 s`, `3 min 20 s`, `1 h 05 min`: rounded, so it doesn't flicker.
pub(super) fn format_eta(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{} s", s.div_ceil(5).max(1) * 5)
    } else if s < 3600 {
        let s = s.div_ceil(10) * 10;
        format!("{} min {:02} s", s / 60, s % 60)
    } else {
        let m = s.div_ceil(60);
        format!("{} h {:02} min", m / 60, m % 60)
    }
}

impl App {
    pub(super) fn launch(&self, task: Task) {
        if self.state.borrow().worker.is_some() {
            return;
        }
        let ctx = self.ctx();
        let device = self.state.borrow().device.clone();
        let audio = lunii_core::convert::audio::AudioOptions {
            normalize_volume: self.ui().get_normalize_volume(),
        };
        let handle = worker::spawn(task, device, audio, ctx.clone(), sink());
        self.state.borrow_mut().worker = Some((handle, ctx));
        let ui = self.ui();
        ui.set_busy(true);
        ui.set_abort_visible(true);
    }

    pub(super) fn start_eta(self: &Rc<Self>, total: u64) {
        let Some(base) = self
            .state
            .borrow()
            .worker
            .as_ref()
            .map(|(_, c)| c.progress())
        else {
            return;
        };
        if total == 0 {
            return;
        }
        self.state.borrow_mut().eta = Some(Eta {
            start: Instant::now(),
            total,
            base,
            samples: VecDeque::new(),
        });
        self.update_eta();
        let w = Rc::downgrade(self);
        self.eta_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(1),
            move || {
                if let Some(app) = w.upgrade() {
                    app.update_eta();
                }
            },
        );
    }

    pub(super) fn update_eta(&self) {
        let Some(written) = self
            .state
            .borrow()
            .worker
            .as_ref()
            .map(|(_, c)| c.progress())
        else {
            return;
        };
        let text = {
            let mut st = self.state.borrow_mut();
            let Some(eta) = st.eta.as_mut() else {
                return;
            };
            let done = written.saturating_sub(eta.base);
            if done >= eta.total {
                // last writes, database update...
                String::new()
            } else {
                match eta.estimate(Instant::now(), done) {
                    Some(d) => trf("Time left: {}", &[&format_eta(d)]),
                    None => tr("Estimating time left..."),
                }
            }
        };
        self.ui().set_eta_text(text.into());
    }

    pub(super) fn stop_eta(&self) {
        self.eta_timer.stop();
        self.state.borrow_mut().eta = None;
        self.ui().set_eta_text("".into());
    }

    pub(super) fn abort(&self) {
        if let Some((_, ctx)) = self.state.borrow().worker.as_ref() {
            ctx.request_abort();
        } else {
            return;
        }
        self.status(&tr("Abort requested, please wait..."));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eta_format() {
        assert_eq!(format_eta(Duration::from_secs(0)), "5 s");
        assert_eq!(format_eta(Duration::from_secs(42)), "45 s");
        assert_eq!(format_eta(Duration::from_secs(200)), "3 min 20 s");
        assert_eq!(format_eta(Duration::from_secs(201)), "3 min 30 s");
        assert_eq!(format_eta(Duration::from_secs(3_900)), "1 h 05 min");
    }

    #[test]
    fn eta_estimate() {
        let t = Instant::now();
        let mut eta = Eta {
            start: t,
            total: 10_000_000,
            base: 0,
            samples: VecDeque::new(),
        };
        let at = |s: u64| t + Duration::from_secs(s);
        // warm-up: no estimation
        assert_eq!(eta.estimate(at(0), 0), None);
        assert_eq!(eta.estimate(at(2), 200_000), None);
        // 100 KB/s, 9.5 MB left
        let d = eta.estimate(at(5), 500_000).unwrap();
        assert_eq!(d.as_secs(), 95);
        // nothing written for 30 s (transcoding): average since the start
        let d = eta.estimate(at(35), 500_000).unwrap();
        assert!(d.as_secs() > 600, "{d:?}");
        // speed measured on the window, not since the start
        let d = eta.estimate(at(45), 2_500_000).unwrap();
        assert!(d.as_secs() < 100, "{d:?}");
    }
}
