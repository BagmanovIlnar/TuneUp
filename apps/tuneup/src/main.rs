#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod deactivator;
mod logging;
mod maintenance_worker;
mod notify;
mod platform_runtime;
mod scanner;
mod single_instance;
mod theme;
mod tray;
mod views;
mod window;

use std::sync::mpsc;

use app::TuneupApp;
use eframe::egui;

fn main() -> eframe::Result {
    let _logging = logging::init().ok();
    let (tray_event_tx, tray_event_rx) = mpsc::channel();
    let context_slot = single_instance::context_slot();
    let Ok(_instance) = single_instance::ensure_single(tray_event_tx.clone(), context_slot.clone())
    else {
        // Another TuneUp is already running — asked it to show the window.
        return Ok(());
    };
    tracing::info!("TuneUp starting (tray-only until window is opened)");
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("TuneUp")
            .with_inner_size([1_180.0, 720.0])
            .with_min_inner_size([920.0, 560.0])
            // Start invisible. WindowController keeps the native window minimized
            // until tray open so eframe's first-frame set_visible(true) does not flash.
            .with_visible(false)
            .with_active(false)
            .with_taskbar(false),
        centered: false,
        ..Default::default()
    };
    configure_platform_windowing(&mut options);
    eframe::run_native(
        "TuneUp",
        options,
        Box::new(move |creation_context| {
            if let Ok(mut slot) = context_slot.lock() {
                *slot = Some(creation_context.egui_ctx.clone());
            }
            Ok(Box::new(TuneupApp::new(
                creation_context,
                tray_event_tx,
                tray_event_rx,
            )))
        }),
    )
}

/// Platform tweaks required for tray-first window lifecycle.
fn configure_platform_windowing(options: &mut eframe::NativeOptions) {
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        options.event_loop_builder = Some(Box::new(|builder| {
            builder.with_activation_policy(ActivationPolicy::Accessory);
        }));
    }

    #[cfg(target_os = "linux")]
    {
        // winit's Wayland backend implements `set_visible` as a no-op, so
        // tray-first hide and "close → tray" break (window stays forever because
        // we CancelClose). Prefer X11 / XWayland when DISPLAY is available.
        if std::env::var_os("DISPLAY").is_some() {
            use winit::platform::x11::EventLoopBuilderExtX11;
            tracing::info!("forcing X11/XWayland backend for tray window hide/show");
            options.event_loop_builder = Some(Box::new(|builder| {
                builder.with_x11();
            }));
        } else {
            tracing::warn!(
                "DISPLAY is unset; Wayland cannot hide windows — close may not return to tray"
            );
        }
    }

    let _ = options;
}
