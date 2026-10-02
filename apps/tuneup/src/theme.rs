//! Adaptive visual tokens shared by the TuneUp shell.
//!
//! One UX is preserved across platforms; only colors, spacing and corner radii
//! shift slightly toward Fluent, macOS and Linux desktop conventions.

use eframe::egui::{self, Color32, CornerRadius, Margin, Stroke, Visuals};

/// Platform-tuned visual tokens for the main window shell.
#[derive(Debug, Clone, Copy)]
pub struct PlatformTheme {
    pub sidebar_width: f32,
    pub background: Color32,
    pub panel: Color32,
    pub panel_raised: Color32,
    pub border: Color32,
    pub accent: Color32,
    pub text_muted: Color32,
    pub corner_radius: u8,
    pub spacing: f32,
}

impl PlatformTheme {
    /// Returns the adaptive theme for the compilation target OS.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let theme = PlatformTheme::current();
    /// assert!(theme.sidebar_width > 0.0);
    /// ```
    pub fn current() -> Self {
        #[cfg(target_os = "windows")]
        {
            Self {
                sidebar_width: 148.0,
                background: Color32::from_rgb(32, 32, 32),
                panel: Color32::from_rgb(45, 45, 45),
                panel_raised: Color32::from_rgb(55, 55, 55),
                border: Color32::from_rgb(70, 70, 70),
                accent: Color32::from_rgb(0, 120, 212),
                text_muted: Color32::from_rgb(170, 170, 170),
                corner_radius: 4,
                spacing: 10.0,
            }
        }
        #[cfg(target_os = "macos")]
        {
            Self {
                sidebar_width: 156.0,
                background: Color32::from_rgb(28, 28, 30),
                panel: Color32::from_rgb(38, 38, 42),
                panel_raised: Color32::from_rgb(48, 48, 54),
                border: Color32::from_rgb(68, 68, 74),
                accent: Color32::from_rgb(10, 132, 255),
                text_muted: Color32::from_rgb(152, 152, 157),
                corner_radius: 10,
                spacing: 12.0,
            }
        }
        #[cfg(target_os = "linux")]
        {
            Self {
                sidebar_width: 152.0,
                background: Color32::from_rgb(24, 24, 27),
                panel: Color32::from_rgb(36, 36, 41),
                panel_raised: Color32::from_rgb(46, 46, 52),
                border: Color32::from_rgb(64, 64, 72),
                accent: Color32::from_rgb(53, 132, 228),
                text_muted: Color32::from_rgb(154, 160, 166),
                corner_radius: 8,
                spacing: 11.0,
            }
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            Self {
                sidebar_width: 152.0,
                background: Color32::from_rgb(20, 23, 30),
                panel: Color32::from_rgb(27, 31, 40),
                panel_raised: Color32::from_rgb(34, 39, 50),
                border: Color32::from_rgb(54, 61, 76),
                accent: Color32::from_rgb(92, 145, 255),
                text_muted: Color32::from_rgb(155, 164, 184),
                corner_radius: 8,
                spacing: 10.0,
            }
        }
    }

    /// Applies egui visuals derived from these tokens.
    pub fn apply(self, context: &egui::Context) {
        let mut visuals = Visuals::dark();
        visuals.panel_fill = self.background;
        visuals.window_fill = self.panel;
        visuals.faint_bg_color = self.panel_raised;
        visuals.extreme_bg_color = self.background;
        visuals.selection.bg_fill = self.accent.gamma_multiply(0.55);
        visuals.widgets.inactive.bg_fill = self.panel_raised;
        visuals.widgets.inactive.weak_bg_fill = self.panel_raised;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, self.border);
        visuals.widgets.hovered.bg_fill = self.panel_raised.gamma_multiply(1.15);
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, self.accent);
        visuals.widgets.active.bg_fill = self.accent.gamma_multiply(0.7);
        visuals.window_corner_radius = CornerRadius::same(self.corner_radius);
        visuals.menu_corner_radius = CornerRadius::same(self.corner_radius);
        context.set_visuals(visuals);

        let mut style = (*context.style_of(egui::Theme::Dark)).clone();
        style.spacing.item_spacing = egui::vec2(self.spacing, self.spacing * 0.8);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.spacing.window_margin = Margin::same(18);
        context.set_style_of(egui::Theme::Dark, style);
    }
}
