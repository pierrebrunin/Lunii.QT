//! Tasks: their launch, progress (bytes written, time elapsed), abort.

use super::*;

/// The running task.
#[derive(Default)]
pub(super) struct TaskState {
    pub(super) worker: Option<(std::thread::JoinHandle<()>, Context)>,
    pub(super) transfer: Option<Transfer>,
    /// the task computes the sizes
    pub(super) sizing: bool,
}

/// Bytes written to the device during an import, and since when.
pub(super) struct Transfer {
    start: Instant,
    /// bytes expected for the whole import
    total: u64,
    /// `Context::progress()` when the import started
    base: u64,
}

/// `12.3 MB / 85.0 MB · 72.7 MB left · 3 min 20 s`: measured, not
/// estimated (the writes pause during the audio conversions).
pub(crate) fn transfer_text(done: u64, total: u64, elapsed: Duration) -> String {
    let done = done.min(total);
    trf(
        "{} / {} · {} left · {}",
        &[
            &lunii_core::format::size(done),
            &lunii_core::format::size(total),
            &lunii_core::format::size(total - done),
            &lunii_core::format::secs(elapsed.as_secs()),
        ],
    )
}

impl App {
    pub(super) fn launch(&self, task: Task) {
        if self.state.borrow().task.worker.is_some() {
            return;
        }
        let ctx = self.ctx();
        let device = self.state.borrow().devices.opened.clone();
        let audio = lunii_core::convert::audio::AudioOptions {
            normalize_volume: self.ui().get_normalize_volume(),
        };
        let handle = worker::spawn(task, device, audio, ctx.clone(), sink());
        self.state.borrow_mut().task.worker = Some((handle, ctx));
        let ui = self.ui();
        ui.set_busy(true);
        ui.set_abort_visible(true);
    }

    pub(super) fn start_transfer(self: &Rc<Self>, total: u64) {
        let Some(base) = self
            .state
            .borrow()
            .task
            .worker
            .as_ref()
            .map(|(_, c)| c.progress())
        else {
            return;
        };
        if total == 0 {
            return;
        }
        // the total known so far, updated (podcast episodes)
        if let Some(transfer) = self.state.borrow_mut().task.transfer.as_mut() {
            transfer.total = total;
            return;
        }
        self.state.borrow_mut().task.transfer = Some(Transfer {
            start: Instant::now(),
            total,
            base,
        });
        self.update_transfer();
        let w = Rc::downgrade(self);
        self.transfer_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(1),
            move || {
                if let Some(app) = w.upgrade() {
                    app.update_transfer();
                }
            },
        );
    }

    pub(super) fn update_transfer(&self) {
        let Some(written) = self
            .state
            .borrow()
            .task
            .worker
            .as_ref()
            .map(|(_, c)| c.progress())
        else {
            return;
        };
        let text = {
            let st = self.state.borrow();
            let Some(transfer) = st.task.transfer.as_ref() else {
                return;
            };
            transfer_text(
                written.saturating_sub(transfer.base),
                transfer.total,
                transfer.start.elapsed(),
            )
        };
        self.ui().set_transfer_text(text.into());
    }

    pub(super) fn stop_transfer(&self) {
        self.transfer_timer.stop();
        self.state.borrow_mut().task.transfer = None;
        self.ui().set_transfer_text("".into());
    }

    pub(super) fn abort(&self) {
        if let Some((_, ctx)) = self.state.borrow().task.worker.as_ref() {
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
    fn transfer() {
        let mb = 1024 * 1024;
        assert_eq!(
            transfer_text(12 * mb, 85 * mb, Duration::from_secs(200)),
            "12.0 MB / 85.0 MB · 73.0 MB left · 3 min 20 s"
        );
        // more written than expected (index files...): nothing left
        assert_eq!(
            transfer_text(90 * mb, 85 * mb, Duration::from_secs(5)),
            "85.0 MB / 85.0 MB · 0.0 MB left · 5 s"
        );
    }
}
