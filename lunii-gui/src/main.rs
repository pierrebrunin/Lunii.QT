//! Lunii graphical interface (Slint).

// no console window on Windows (release builds; debug ones keep it for the logs)
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

slint::include_modules!();

mod app;
mod i18n;
mod logfile;
#[cfg(feature = "screenshot")]
mod screenshot;
mod worker;

fn main() -> anyhow::Result<()> {
    #[cfg(feature = "screenshot")]
    if let Some(out) = std::env::var_os("LUNII_SCREENSHOT") {
        return screenshot::run(out.into());
    }

    // the winit backend is needed for file drops
    backend().select()?;

    logfile::init();
    #[cfg(target_os = "linux")]
    if prefer_x11() {
        log::info!(target: "lunii", "Wayland session: using X11 (XWayland) for file drops, LUNII_WAYLAND=1 to disable");
    }
    let ui = MainWindow::new()?;
    // must be called after the creation of the first component
    i18n::apply_language();
    let app = app::App::new(&ui);

    ui.show()?;
    app.start();
    slint::run_event_loop()?;
    app.shutdown();
    Ok(())
}

/// winit backend selector. Under a Wayland session, X11 (XWayland) is preferred
/// when available: winit 0.30 only reports file drops on X11.
/// `LUNII_WAYLAND=1` keeps the native Wayland backend.
fn backend() -> slint::BackendSelector {
    let selector = slint::BackendSelector::new().backend_name("winit".into());
    #[cfg(target_os = "linux")]
    if prefer_x11() {
        use slint::winit_030::SlintEvent;
        use slint::winit_030::winit::event_loop::EventLoop;
        use slint::winit_030::winit::platform::x11::EventLoopBuilderExtX11;

        let mut builder = EventLoop::<SlintEvent>::with_user_event();
        builder.with_x11();
        return selector.with_winit_event_loop_builder(builder);
    }
    selector
}

#[cfg(target_os = "linux")]
fn prefer_x11() -> bool {
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    set("WAYLAND_DISPLAY") && set("DISPLAY") && !set("LUNII_WAYLAND")
}
