//! Cleanup and uninstall view state/helpers for the TuneUp shell.

pub mod cleanup;
pub mod settings;
pub mod uninstall;

use eframe::egui::{self, Color32, RichText};

use crate::theme::PlatformTheme;

/// Formats a byte count for UI cards.
pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1_048_576.0;
    const GIB: f64 = 1_073_741_824.0;
    if bytes as f64 >= GIB {
        format!("{:.1} ГБ", bytes as f64 / GIB)
    } else if bytes as f64 >= MIB {
        format!("{:.0} МБ", bytes as f64 / MIB)
    } else if bytes as f64 >= KIB {
        format!("{:.0} КБ", bytes as f64 / KIB)
    } else {
        format!("{bytes} Б")
    }
}

/// Expand/collapse control drawn as vector triangles (avoids missing font glyphs).
pub fn expand_toggle(ui: &mut egui::Ui, expanded: bool, enabled: bool) -> egui::Response {
    let desired = egui::vec2(18.0, 18.0);
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(desired, sense);
    let color = if enabled {
        ui.style().interact(&response).fg_stroke.color
    } else {
        ui.visuals().weak_text_color()
    };
    let center = rect.center();
    let painter = ui.painter();
    let points = if expanded {
        [
            center + egui::vec2(-5.0, -2.5),
            center + egui::vec2(5.0, -2.5),
            center + egui::vec2(0.0, 4.0),
        ]
    } else {
        [
            center + egui::vec2(-3.0, -5.0),
            center + egui::vec2(-3.0, 5.0),
            center + egui::vec2(5.0, 0.0),
        ]
    };
    painter.add(egui::Shape::convex_polygon(
        points.to_vec(),
        color,
        egui::Stroke::NONE,
    ));
    response
}

/// Draws a notice banner inside a section.
pub fn notice(ui: &mut egui::Ui, theme: &PlatformTheme, message: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.65)))
        .corner_radius(theme.corner_radius)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.label(RichText::new(message).color(color));
        });
    ui.add_space(5.0);
}
