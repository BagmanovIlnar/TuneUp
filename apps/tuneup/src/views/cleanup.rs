//! Cleanup section UI state and rendering.

use std::collections::{BTreeMap, BTreeSet};

use eframe::egui::{self, Color32, RichText};
use tuneup_core::cleanup::{CleanupItem, CleanupReport, CleanupScan, CleanupSelection};

use crate::theme::PlatformTheme;

use super::{format_bytes, notice};

/// Mutable UI state for the cleanup section.
#[derive(Debug, Default)]
pub struct CleanupViewState {
    pub scan: Option<CleanupScan>,
    pub selected: BTreeSet<String>,
    pub busy: bool,
    pub last_report: Option<CleanupReport>,
    pub message: Option<(bool, String)>,
    pub confirm_clean: bool,
}

impl CleanupViewState {
    /// Applies a completed scan and selects default-safe items.
    pub fn apply_scan(&mut self, scan: CleanupScan) {
        self.selected = scan
            .items
            .iter()
            .filter(|item| item.selected_by_default)
            .map(|item| item.id.clone())
            .collect();
        self.scan = Some(scan);
        self.busy = false;
        self.message = Some((true, "Сканирование завершено".into()));
    }

    /// Applies a cleanup report.
    pub fn apply_report(&mut self, report: CleanupReport) {
        let success = report.failed.is_empty();
        self.message = Some((
            success,
            if success {
                format!("Освобождено {}", format_bytes(report.freed_bytes))
            } else {
                format!(
                    "Освобождено {} · ошибок: {}",
                    format_bytes(report.freed_bytes),
                    report.failed.len()
                )
            },
        ));
        self.last_report = Some(report);
        self.busy = false;
        self.confirm_clean = false;
    }

    /// Builds a selection payload for the worker.
    pub fn selection(&self) -> CleanupSelection {
        CleanupSelection {
            item_ids: self.selected.iter().cloned().collect(),
        }
    }

    /// Renders the cleanup section.
    pub fn ui(&mut self, ui: &mut egui::Ui, theme: &PlatformTheme, enabled: bool) -> CleanupAction {
        let mut action = CleanupAction::None;
        egui::Frame::new()
            .fill(theme.panel)
            .stroke(egui::Stroke::new(1.0, theme.border))
            .corner_radius(theme.corner_radius)
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Очистка")
                            .size(22.0)
                            .strong()
                            .color(theme.accent),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let clean_enabled = enabled
                            && !self.busy
                            && !self.selected.is_empty()
                            && self.scan.is_some();
                        if ui
                            .add_enabled(
                                clean_enabled,
                                egui::Button::new(
                                    RichText::new("Очистить выбранное").color(Color32::WHITE),
                                )
                                .fill(theme.accent)
                                .min_size(egui::vec2(160.0, 34.0)),
                            )
                            .clicked()
                        {
                            self.confirm_clean = true;
                        }
                        if ui
                            .add_enabled(
                                enabled && !self.busy,
                                egui::Button::new("Сканировать").min_size(egui::vec2(120.0, 34.0)),
                            )
                            .clicked()
                        {
                            self.busy = true;
                            self.message = Some((true, "Сканирование…".into()));
                            action = CleanupAction::Scan;
                        }
                    });
                });
                ui.label(
                    RichText::new(
                        "Общий мусор: кэш, временные файлы и корзина. Связанные файлы конкретного приложения — в разделе «Удаление»",
                    )
                    .color(theme.text_muted),
                );
                ui.add_space(12.0);

                let selected_bytes = self
                    .scan
                    .as_ref()
                    .map(|scan| {
                        scan.items
                            .iter()
                            .filter(|item| self.selected.contains(&item.id))
                            .map(|item| item.size_bytes)
                            .sum::<u64>()
                    })
                    .unwrap_or(0);
                ui.columns(2, |columns| {
                    summary_card(
                        &mut columns[0],
                        theme,
                        "НАЙДЕНО",
                        &format_bytes(
                            self.scan
                                .as_ref()
                                .map(CleanupScan::total_bytes)
                                .unwrap_or(0),
                        ),
                    );
                    summary_card(
                        &mut columns[1],
                        theme,
                        "К УДАЛЕНИЮ",
                        &format_bytes(selected_bytes),
                    );
                });
                ui.add_space(10.0);

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

                if self.busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Выполняется операция…").color(theme.text_muted));
                    });
                }

                match &self.scan {
                    None => {
                        ui.vertical_centered(|ui| {
                            ui.add_space(40.0);
                            ui.label(
                                RichText::new("Нажмите «Сканировать», чтобы найти мусор")
                                    .color(theme.text_muted),
                            );
                        });
                    }
                    Some(scan) => {
                        if !scan.warnings.is_empty() {
                            for warning in &scan.warnings {
                                notice(ui, theme, warning, Color32::from_rgb(171, 124, 45));
                            }
                        }
                        let grouped = group_by_category(&scan.items);
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for (category, items) in grouped {
                                    let category_bytes: u64 =
                                        items.iter().map(|item| item.size_bytes).sum();
                                    ui.add_space(8.0);
                                    ui.label(
                                        RichText::new(format!(
                                            "{} · {}",
                                            category.label(),
                                            format_bytes(category_bytes)
                                        ))
                                        .strong()
                                        .size(16.0),
                                    );
                                    for item in items {
                                        let mut checked = self.selected.contains(&item.id);
                                        ui.horizontal(|ui| {
                                            if ui
                                                .add_enabled(
                                                    !self.busy,
                                                    egui::Checkbox::new(
                                                        &mut checked,
                                                        RichText::new(&item.description)
                                                            .color(Color32::WHITE),
                                                    ),
                                                )
                                                .changed()
                                            {
                                                if checked {
                                                    self.selected.insert(item.id.clone());
                                                } else {
                                                    self.selected.remove(&item.id);
                                                }
                                            }
                                            ui.label(
                                                RichText::new(format_bytes(item.size_bytes))
                                                    .color(theme.text_muted),
                                            );
                                            if item.requires_elevation {
                                                ui.label(
                                                    RichText::new("требуются права")
                                                        .small()
                                                        .color(Color32::from_rgb(244, 184, 96)),
                                                );
                                            }
                                        });
                                        ui.label(
                                            RichText::new(item.path.display().to_string())
                                                .small()
                                                .color(theme.text_muted),
                                        );
                                    }
                                }
                            });
                    }
                }
            });

        if self.confirm_clean {
            egui::Window::new("Подтверждение очистки")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    ui.label(format!(
                        "Удалить {} выбранных объектов ({})?",
                        self.selected.len(),
                        format_bytes(selected_bytes_of(self))
                    ));
                    ui.colored_label(
                        Color32::YELLOW,
                        "Системные объекты с повышением прав будут пропущены, пока helper не подтвердит операцию.",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Отмена").clicked() {
                            self.confirm_clean = false;
                        }
                        if ui
                            .button(RichText::new("Очистить").color(Color32::LIGHT_RED))
                            .clicked()
                        {
                            self.busy = true;
                            self.confirm_clean = false;
                            action = CleanupAction::Clean(self.selection());
                        }
                    });
                });
        }
        action
    }
}

/// Action requested by the cleanup view.
pub enum CleanupAction {
    None,
    Scan,
    Clean(CleanupSelection),
}

fn selected_bytes_of(state: &CleanupViewState) -> u64 {
    state
        .scan
        .as_ref()
        .map(|scan| {
            scan.items
                .iter()
                .filter(|item| state.selected.contains(&item.id))
                .map(|item| item.size_bytes)
                .sum()
        })
        .unwrap_or(0)
}

fn group_by_category(
    items: &[CleanupItem],
) -> Vec<(tuneup_core::cleanup::CleanupCategory, Vec<&CleanupItem>)> {
    let mut map: BTreeMap<_, Vec<&CleanupItem>> = BTreeMap::new();
    for item in items {
        map.entry(item.category as u8).or_default().push(item);
    }
    let mut result = Vec::new();
    for items in map.into_values() {
        if let Some(first) = items.first() {
            result.push((first.category, items));
        }
    }
    result
}

fn summary_card(ui: &mut egui::Ui, theme: &PlatformTheme, title: &str, value: &str) {
    egui::Frame::new()
        .fill(theme.panel_raised)
        .stroke(egui::Stroke::new(1.0, theme.border))
        .corner_radius(theme.corner_radius)
        .inner_margin(14.0)
        .show(ui, |ui| {
            ui.set_min_height(54.0);
            ui.label(
                RichText::new(title)
                    .size(11.0)
                    .strong()
                    .color(theme.text_muted),
            );
            ui.add_space(4.0);
            ui.label(RichText::new(value).size(22.0).strong().color(theme.accent));
        });
}
