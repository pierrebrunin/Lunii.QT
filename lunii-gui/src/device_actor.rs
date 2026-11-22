//! The opened device, owned by a thread of its own.
//!
//! Nothing else touches it: the tasks and the quick changes of the UI (move,
//! hide...) are jobs sent to this thread, run one after the other. Their
//! results come back to the UI as events. The UI never waits for the device:
//! it displays copies of its state (see `worker::snapshot`).

use std::panic::AssertUnwindSafe;
use std::sync::mpsc;
use std::thread::JoinHandle;

use lunii_core::Device;

type Job = Box<dyn FnOnce(&mut dyn Device) + Send>;

/// Sends jobs to the thread of a device (cheap to clone).
#[derive(Clone)]
pub struct DeviceHandle(mpsc::Sender<Job>);

impl DeviceHandle {
    /// Queues `job`, run on the device thread. `false` if the thread is gone
    /// (the job is dropped, not run).
    #[must_use]
    pub fn run(&self, job: impl FnOnce(&mut dyn Device) + Send + 'static) -> bool {
        self.0.send(Box::new(job)).is_ok()
    }
}

/// The thread owning the opened device.
pub struct DeviceActor {
    handle: DeviceHandle,
    thread: JoinHandle<()>,
}

impl DeviceActor {
    pub fn spawn(mut dev: Box<dyn Device>) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let thread = std::thread::Builder::new()
            .name("device".into())
            .spawn(move || {
                for job in rx {
                    // a job panicking: the device state is still usable
                    // by the next ones (as the poisoned mutex was before)
                    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| job(dev.as_mut())));
                }
            })
            .expect("device thread");
        DeviceActor {
            handle: DeviceHandle(tx),
            thread,
        }
    }

    pub fn handle(&self) -> DeviceHandle {
        self.handle.clone()
    }

    /// Waits for the jobs queued (application exit: a task stopping).
    pub fn join(self) {
        drop(self.handle);
        let _ = self.thread.join();
    }
}
