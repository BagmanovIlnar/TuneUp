//! Settings section UI for TuneUp preferences.

use eframe::egui::{self, Color32, RichText};

use crate::theme::PlatformTheme;

use super::notice;

/// Mutable UI state for the settings section.
#[derive(Debug, Default)]
pub struct SettingsViewState {
    pub start_at_login: bool,
    pub synced: bool,
    pub message: Option<(bool, String)>,
    /// Confirmation dialog for full reset.
    pub confirm_reset: bool,
}

impl SettingsViewState {
    /// Applies the OS registration state when opening Settings.
    pub fn apply_os_state(&mut self, enabled: bool) {
        self.start_at_login = enabled;
        self.synced = true;
    }

    /// Renders settings and returns the requested action.
    pub fn ui(&mut self, ui: &mut egui::Ui, theme: &PlatformTheme) -> SettingsAction {
        let mut action = SettingsAction::None;

        egui::Frame::new()
            .fill(theme.panel)
            .stroke(egui::Stroke::new(1.0, theme.border))
            .corner_radius(theme.corner_radius)
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Настройки")
                        .size(22.0)
                        .strong()
                        .color(theme.accent),
                );
                ui.label(
                    RichText::new("Параметры самого TuneUp")
                        .color(theme.text_muted),
                );
                ui.add_space(12.0);

                if let Some((success, message)) = &self.message {
                    notice(
                        ui,
                        theme,
                        message,
                        if *success {
                            Color32::from_rgb(55, 143, 93)
                        } else {
                            Color32::from_rgb(196, 72, 72)
                        },
                    );
                }

                egui::Frame::new()
                    .fill(theme.panel_raised)
                    .stroke(egui::Stroke::new(1.0, theme.border))
                    .corner_radius(theme.corner_radius)
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        let mut enabled = self.start_at_login;
                        if ui
                            .checkbox(
                                &mut enabled,
                                RichText::new("Запускать автоматически при входе в систему")
                                    .color(Color32::WHITE),
                            )
                            .changed()
                        {
                            self.start_at_login = enabled;
                            action = SettingsAction::SetStartAtLogin(enabled);
                        }
                        ui.label(
                            RichText::new(
                                "TuneUp будет стартовать вместе с пользователем (Windows / macOS / Linux).",
                            )
                            .small()
                            .color(theme.text_muted),
                        );
                    });

                ui.add_space(16.0);

                egui::Frame::new()
                    .fill(theme.panel_raised)
                    .stroke(egui::Stroke::new(1.0, theme.border))
                    .corner_radius(theme.corner_radius)
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Сброс")
                                .strong()
                                .color(Color32::WHITE),
                        );
                        ui.label(
                            RichText::new(
                                "Разбудит все усыплённые приложения (вернёт автозапуск), очистит список сна/игнора и сбросит настройки TuneUp.",
                            )
                            .small()
                            .color(theme.text_muted),
                        );
                        ui.add_space(8.0);
                        if ui
                            .button(
                                RichText::new("Сбросить всё")
                                    .color(Color32::from_rgb(255, 160, 160)),
                            )
                            .clicked()
                        {
                            self.confirm_reset = true;
                        }
                    });
            });

        if self.confirm_reset {
            egui::Window::new("Сбросить всё?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    ui.label(
                        "Все спящие приложения будут разбужены (автозапуск восстановится). Сохранённое состояние TuneUp будет очищено.",
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Отмена").clicked() {
                            self.confirm_reset = false;
                        }
                        if ui
                            .button(RichText::new("Сбросить").color(Color32::LIGHT_RED))
                            .clicked()
                        {
                            self.confirm_reset = false;
                            action = SettingsAction::ResetAll;
                        }
                    });
                });
        }

        action
    }
}

/// Action requested by the settings view.
pub enum SettingsAction {
    None,
    SetStartAtLogin(bool),
    /// Wake every sleeping group and clear persisted TuneUp state.
    ResetAll,
}
