use eframe::egui::{Context, Pos2, ViewportCommand};

/// Controls main window visibility for tray-first UX.
///
/// eframe calls `set_visible(true)` after the **first painted** frame
/// (`EpiIntegration::post_rendering`). Painting is skipped when
/// `ViewportInfo::visible()` is false, which happens when the native window
/// is already minimized. So we minimize in [`Self::park_at_creation`] before
/// the event loop paints — then the first-frame reveal never runs, and there
/// is no flash.
pub struct WindowController {
    visible: bool,
}

impl WindowController {
    /// Starts hidden — process + tray only.
    pub fn hidden() -> Self {
        Self { visible: false }
    }

    /// Minimize/hide the native window before the first egui frame.
    ///
    /// Must run from [`eframe::CreationContext`] so the first
    /// `update_viewport_info` already sees `is_minimized() == true`.
    /// Then eframe takes the logic-only path and never calls
    /// `post_rendering` → no forced `set_visible(true)`.
    pub fn park_at_creation(creation_context: &eframe::CreationContext<'_>) {
        if let Some(window) = creation_context.winit_window() {
            window.set_minimized(true);
            window.set_visible(false);
        }
        creation_context
            .egui_ctx
            .send_viewport_cmd(ViewportCommand::Minimized(true));
        creation_context
            .egui_ctx
            .send_viewport_cmd(ViewportCommand::Visible(false));
    }

    /// Marks the window as open when tray creation failed (no native Frame yet).
    pub fn prepare_open_from_context(&mut self, context: &Context) {
        self.visible = true;
        context.send_viewport_cmd(ViewportCommand::Minimized(false));
        context.send_viewport_cmd(ViewportCommand::Visible(true));
        context.send_viewport_cmd(ViewportCommand::Focus);
    }

    /// Shows and focuses the main window (tray «Открыть» / click).
    pub fn show(&mut self, context: &Context, frame: &eframe::Frame) {
        self.visible = true;
        if let Some(window) = frame.winit_window() {
            window.set_minimized(false);
            window.set_visible(true);
        }
        if let Some(center) = ViewportCommand::center_on_screen(context) {
            context.send_viewport_cmd(center);
        } else {
            context.send_viewport_cmd(ViewportCommand::OuterPosition(Pos2::new(120.0, 80.0)));
        }
        context.send_viewport_cmd(ViewportCommand::Minimized(false));
        context.send_viewport_cmd(ViewportCommand::Visible(true));
        context.send_viewport_cmd(ViewportCommand::Focus);
        context.request_repaint();
    }

    /// Hides the window without quitting (close button → tray).
    pub fn hide(&mut self, context: &Context, frame: &eframe::Frame) {
        self.visible = false;
        Self::apply_native_hidden(context, frame);
        context.request_repaint();
    }

    /// Re-applies hidden/minimized state if something made the window visible.
    pub fn enforce_hidden_if_needed(&self, context: &Context, frame: &eframe::Frame) {
        if !self.visible {
            Self::apply_native_hidden(context, frame);
        }
    }

    fn apply_native_hidden(context: &Context, frame: &eframe::Frame) {
        if let Some(window) = frame.winit_window() {
            window.set_minimized(true);
            window.set_visible(false);
        }
        context.send_viewport_cmd(ViewportCommand::Minimized(true));
        context.send_viewport_cmd(ViewportCommand::Visible(false));
    }
}
