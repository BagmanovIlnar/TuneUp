//! Uninstall section UI state and rendering.
//!
//! App Cleaner–style list: expand an application to review related files, then
//! remove the app together with the selected leftovers.

use std::collections::{BTreeMap, BTreeSet};

use eframe::egui::{self, Color32, RichText};
use tuneup_core::uninstall::{
    InstalledApplication, LeftoverItem, UninstallReport, UninstallRequest,
};

use crate::theme::PlatformTheme;

use super::{expand_toggle, format_bytes, notice};

/// Mutable UI state for the uninstall section.
#[derive(Debug, Default)]
pub struct UninstallViewState {
    pub applications: Vec<InstalledApplication>,
    pub filter: String,
    pub selected_id: Option<String>,
    pub expanded: BTreeSet<String>,
    pub leftovers_by_app: BTreeMap<String, Vec<LeftoverItem>>,
    pub selected_leftovers: BTreeMap<String, BTreeSet<String>>,
    pub include_user_data: BTreeMap<String, bool>,
    pub scanning: BTreeSet<String>,
    pub pending_confirm: Option<String>,
    pub busy: bool,
    pub loaded: bool,
    pub confirm: bool,
    pub message: Option<(bool, String)>,
    /// Show protected / system packages in the list.
    pub show_system_packages: bool,
    /// Indices into `applications` for the current filter (rebuilt lazily).
    filtered_indices: Vec<usize>,
    filter_fingerprint: (u64, bool, String),
}

impl UninstallViewState {
    /// Replaces the application list.
    pub fn apply_applications(&mut self, applications: Vec<InstalledApplication>) {
        self.applications = applications;
        self.loaded = true;
        self.busy = false;
        self.invalidate_filter_cache();
        let valid: BTreeSet<_> = self.applications.iter().map(|app| app.id.clone()).collect();
        self.expanded.retain(|id| valid.contains(id));
        self.leftovers_by_app.retain(|id, _| valid.contains(id));
        self.selected_leftovers.retain(|id, _| valid.contains(id));
        self.include_user_data.retain(|id, _| valid.contains(id));
        self.scanning.retain(|id| valid.contains(id));
        if let Some(id) = &self.selected_id
            && !valid.contains(id)
        {
            self.selected_id = None;
            self.confirm = false;
        }
        self.message = Some((
            true,
            format!("Список программ обновлён · {}", self.applications.len()),
        ));
    }

    /// Applies leftover discovery for an application.
    pub fn apply_leftovers(&mut self, application_id: String, leftovers: Vec<LeftoverItem>) {
        self.scanning.remove(&application_id);
        let selected = leftovers
            .iter()
            .map(|item| item.id.clone())
            .collect::<BTreeSet<_>>();
        let has_user_data = leftovers.iter().any(|item| item.kind.is_user_data());
        self.selected_leftovers
            .insert(application_id.clone(), selected);
        self.include_user_data
            .insert(application_id.clone(), has_user_data);
        self.leftovers_by_app
            .insert(application_id.clone(), leftovers);
        if self.pending_confirm.as_deref() == Some(application_id.as_str()) {
            self.pending_confirm = None;
            self.selected_id = Some(application_id);
            self.confirm = true;
        }
        self.busy = false;
    }

    /// Applies an uninstall report.
    pub fn apply_report(&mut self, report: UninstallReport) {
        let success = report.uninstalled && report.errors.is_empty();
        self.message = Some((
            success,
            if success {
                format!(
                    "Удаление завершено · очищено: {}",
                    format_bytes(report.leftovers_freed_bytes)
                )
            } else if report.errors.is_empty() {
                "Удаление завершено с предупреждениями".into()
            } else {
                report.errors.join("\n")
            },
        ));
        self.busy = false;
        self.confirm = false;
        if report.uninstalled
            && let Some(id) = self.selected_id.take()
        {
            self.applications.retain(|app| app.id != id);
            self.expanded.remove(&id);
            self.leftovers_by_app.remove(&id);
            self.selected_leftovers.remove(&id);
            self.include_user_data.remove(&id);
            self.scanning.remove(&id);
        }
    }

    /// Builds an uninstall request from the current selection.
    pub fn request(&self) -> Option<UninstallRequest> {
        let application_id = self.selected_id.clone()?;
        let leftover_ids = self
            .selected_leftovers
            .get(&application_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let include_user_data = self
            .include_user_data
            .get(&application_id)
            .copied()
            .unwrap_or(false);
        Some(UninstallRequest {
            application_id,
            leftover_ids,
            include_user_data,
        })
    }

    fn leftovers_total(leftovers: &[LeftoverItem]) -> u64 {
        leftovers.iter().map(|item| item.size_bytes).sum()
    }

    fn selected_total(&self, app_id: &str) -> u64 {
        let Some(selected) = self.selected_leftovers.get(app_id) else {
            return 0;
        };
        self.leftovers_by_app
            .get(app_id)
            .into_iter()
            .flatten()
            .filter(|item| selected.contains(&item.id))
            .map(|item| item.size_bytes)
            .sum()
    }

    fn toggle_leftover(&mut self, app_id: &str, leftover: &LeftoverItem, checked: bool) {
        let selected = self
            .selected_leftovers
            .entry(app_id.to_owned())
            .or_default();
        if checked {
            selected.insert(leftover.id.clone());
        } else {
            selected.remove(&leftover.id);
        }
        self.sync_include_user_data(app_id);
    }

    /// Keeps the user-data flag in sync with currently selected leftovers.
    fn sync_include_user_data(&mut self, app_id: &str) {
        let has_user_data = self
            .leftovers_by_app
            .get(app_id)
            .into_iter()
            .flatten()
            .any(|item| {
                item.kind.is_user_data()
                    && self
                        .selected_leftovers
                        .get(app_id)
                        .is_some_and(|set| set.contains(&item.id))
            });
        self.include_user_data
            .insert(app_id.to_owned(), has_user_data);
    }

    fn select_all_related(&mut self, app_id: &str) {
        let Some(leftovers) = self.leftovers_by_app.get(app_id) else {
            return;
        };
        let selected = leftovers.iter().map(|item| item.id.clone()).collect();
        self.selected_leftovers.insert(app_id.to_owned(), selected);
        self.include_user_data.insert(app_id.to_owned(), true);
    }

    fn set_include_user_data(&mut self, app_id: &str, include: bool) {
        self.include_user_data.insert(app_id.to_owned(), include);
        let Some(items) = self.leftovers_by_app.get(app_id).cloned() else {
            return;
        };
        let selected = self
            .selected_leftovers
            .entry(app_id.to_owned())
            .or_default();
        if include {
            for item in &items {
                if item.kind.is_user_data() {
                    selected.insert(item.id.clone());
                }
            }
        } else {
            selected.retain(|id| {
                items
                    .iter()
                    .find(|item| &item.id == id)
                    .is_none_or(|item| !item.kind.is_user_data())
            });
        }
    }

    fn invalidate_filter_cache(&mut self) {
        self.filter_fingerprint = (usize::MAX as u64, false, String::new());
        self.filtered_indices.clear();
    }

    fn rebuild_filter_if_needed(&mut self) {
        let fingerprint = (
            self.applications.len() as u64,
            self.show_system_packages,
            self.filter.clone(),
        );
        if self.filter_fingerprint == fingerprint {
            return;
        }
        self.filter_fingerprint = fingerprint;
        let filter = self.filter.to_ascii_lowercase();
        self.filtered_indices = self
            .applications
            .iter()
            .enumerate()
            .filter(|(_, app)| {
                if !self.show_system_packages && app.protected {
                    return false;
                }
                filter.is_empty()
                    || app.name.to_ascii_lowercase().contains(&filter)
                    || app
                        .publisher
                        .as_ref()
                        .is_some_and(|value| value.to_ascii_lowercase().contains(&filter))
                    || app.id.to_ascii_lowercase().contains(&filter)
            })
            .map(|(index, _)| index)
            .collect();
    }

    fn set_expanded(&mut self, app_id: &str, expand: bool) -> Option<UninstallAction> {
        if expand {
            self.expanded.insert(app_id.to_owned());
            self.selected_id = Some(app_id.to_owned());
            let needs_scan = !self.leftovers_by_app.contains_key(app_id)
                && !self.scanning.contains(app_id);
            if needs_scan {
                self.scanning.insert(app_id.to_owned());
                self.busy = true;
                return Some(UninstallAction::ScanLeftovers(app_id.to_owned()));
            }
        } else {
            self.expanded.remove(app_id);
        }
        None
    }

    /// Renders the uninstall section.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &PlatformTheme,
        enabled: bool,
    ) -> UninstallAction {
        let mut action = UninstallAction::None;
        if !self.loaded && !self.busy && enabled {
            self.busy = true;
            action = UninstallAction::Load;
        }

        egui::Frame::new()
            .fill(theme.panel)
            .stroke(egui::Stroke::new(1.0, theme.border))
            .corner_radius(theme.corner_radius)
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Удаление")
                            .size(22.0)
                            .strong()
                            .color(theme.accent),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                enabled && !self.busy,
                                egui::Button::new("Обновить").min_size(egui::vec2(110.0, 34.0)),
                            )
                            .clicked()
                        {
                            self.busy = true;
                            action = UninstallAction::Load;
                        }
                    });
                });
                ui.label(
                    RichText::new(
                        "Раскройте приложение, чтобы увидеть связанные файлы в системе, затем удалите программу вместе с выбранными остатками",
                    )
                    .color(theme.text_muted),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label("Поиск:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .desired_width(280.0)
                            .hint_text("название или издатель"),
                    );
                    ui.checkbox(&mut self.show_system_packages, "Системные");
                });
                ui.add_space(8.0);

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
                if self.busy && self.scanning.is_empty() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Выполняется операция…").color(theme.text_muted));
                    });
                }

                self.rebuild_filter_if_needed();
                let filtered_count = self.filtered_indices.len();
                ui.label(
                    RichText::new(format!(
                        "Показано {filtered_count} из {}",
                        self.applications.len()
                    ))
                    .small()
                    .color(theme.text_muted),
                );
                ui.add_space(4.0);

                egui::ScrollArea::vertical()
                    .id_salt("uninstall-app-cleaner")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let indices = self.filtered_indices.clone();
                        for app_index in indices {
                            let Some(app) = self.applications.get(app_index).cloned() else {
                                continue;
                            };
                            let expanded = self.expanded.contains(&app.id);
                            let has_leftovers = self.leftovers_by_app.contains_key(&app.id);
                            let scanning = self.scanning.contains(&app.id);
                            let leftover_total = self
                                .leftovers_by_app
                                .get(&app.id)
                                .map(|items| Self::leftovers_total(items))
                                .unwrap_or(0);
                            let selected_bytes = self.selected_total(&app.id);
                            let app_size = app.estimated_size.unwrap_or(0);

                            // Skip painting off-screen collapsed rows (expanded ones stay measured).
                            if !expanded {
                                let next = ui.cursor().top();
                                let approx = egui::Rect::from_min_size(
                                    egui::pos2(ui.max_rect().left(), next),
                                    egui::vec2(ui.available_width(), 78.0),
                                );
                                if !ui.is_rect_visible(approx) {
                                    ui.add_space(78.0);
                                    ui.add_space(6.0);
                                    continue;
                                }
                            }

                            egui::Frame::new()
                                .fill(theme.panel_raised)
                                .stroke(egui::Stroke::new(1.0, theme.border))
                                .corner_radius(theme.corner_radius)
                                .inner_margin(egui::Margin::symmetric(12, 10))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        if expand_toggle(ui, expanded, enabled && !self.busy)
                                            .clicked()
                                        {
                                            if let Some(next) =
                                                self.set_expanded(&app.id, !expanded)
                                            {
                                                action = next;
                                            }
                                        }

                                        ui.label(
                                            RichText::new(if app.protected {
                                                format!("{} 🔒", app.name)
                                            } else {
                                                app.name.clone()
                                            })
                                            .strong()
                                            .size(15.0)
                                            .color(Color32::WHITE),
                                        );

                                        ui.label(
                                            RichText::new({
                                                let mut parts = Vec::new();
                                                if app_size > 0 {
                                                    parts.push(format!(
                                                        "приложение {}",
                                                        format_bytes(app_size)
                                                    ));
                                                }
                                                if has_leftovers {
                                                    parts.push(format!(
                                                        "остатки {}",
                                                        format_bytes(leftover_total)
                                                    ));
                                                } else if scanning {
                                                    parts.push("поиск остатков…".into());
                                                }
                                                parts.join(" · ")
                                            })
                                            .color(theme.text_muted),
                                        );

                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                let can_remove = enabled
                                                    && !self.busy
                                                    && app.removable
                                                    && !app.protected;
                                                if ui
                                                    .add_enabled(
                                                        can_remove,
                                                        egui::Button::new(
                                                            RichText::new("Удалить")
                                                                .color(Color32::WHITE),
                                                        )
                                                        .fill(Color32::from_rgb(180, 60, 60)),
                                                    )
                                                    .clicked()
                                                {
                                                    self.selected_id = Some(app.id.clone());
                                                    if let Some(next) =
                                                        self.set_expanded(&app.id, true)
                                                    {
                                                        self.pending_confirm =
                                                            Some(app.id.clone());
                                                        action = next;
                                                    } else if has_leftovers {
                                                        self.confirm = true;
                                                    } else {
                                                        self.pending_confirm =
                                                            Some(app.id.clone());
                                                    }
                                                }
                                            },
                                        );
                                    });

                                    ui.label(
                                        RichText::new(format!(
                                            "{} · {}",
                                            app.uninstall_kind.label(),
                                            app.version
                                                .as_deref()
                                                .unwrap_or("версия неизвестна")
                                        ))
                                        .small()
                                        .color(theme.text_muted),
                                    );
                                    if let Some(root) = &app.install_root {
                                        ui.label(
                                            RichText::new(root.display().to_string())
                                                .small()
                                                .color(theme.text_muted),
                                        );
                                    }

                                    if expanded {
                                        ui.add_space(8.0);
                                        self.render_inline_leftovers(
                                            ui,
                                            theme,
                                            &app.id,
                                            scanning,
                                            leftover_total,
                                            selected_bytes,
                                        );
                                    }
                                });
                            ui.add_space(6.0);
                        }
                        if filtered_count == 0 && self.loaded {
                            ui.label(
                                RichText::new("Программы не найдены").color(theme.text_muted),
                            );
                        }
                    });
            });

        if self.confirm {
            let app = self
                .selected_id
                .as_ref()
                .and_then(|id| self.applications.iter().find(|app| &app.id == id));
            let name = app
                .map(|app| app.name.clone())
                .unwrap_or_else(|| "программу".into());
            let app_id = self.selected_id.clone().unwrap_or_default();
            let leftover_count = self
                .selected_leftovers
                .get(&app_id)
                .map(|set| set.len())
                .unwrap_or(0);
            let selected_bytes = self.selected_total(&app_id);
            let include_user_data = self
                .include_user_data
                .get(&app_id)
                .copied()
                .unwrap_or(false);
            let has_user_data = include_user_data
                && self
                    .leftovers_by_app
                    .get(&app_id)
                    .into_iter()
                    .flatten()
                    .any(|item| {
                        self.selected_leftovers
                            .get(&app_id)
                            .is_some_and(|set| set.contains(&item.id))
                            && item.kind.is_user_data()
                    });
            egui::Window::new("Подтверждение удаления")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    ui.label(format!(
                        "Удалить «{name}» и {leftover_count} связанных объектов ({})?",
                        format_bytes(selected_bytes)
                    ));
                    if has_user_data {
                        ui.label(
                            "Будут удалены также профили и настройки. При повторной установке приложение создаст их заново.",
                        );
                    } else {
                        ui.label(
                            RichText::new("Профили и настройки не выбраны.")
                                .color(theme.text_muted),
                        );
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Отмена").clicked() {
                            self.confirm = false;
                        }
                        if ui
                            .button(RichText::new("Удалить").color(Color32::LIGHT_RED))
                            .clicked()
                            && let Some(request) = self.request()
                        {
                            self.busy = true;
                            self.confirm = false;
                            action = UninstallAction::Uninstall(request);
                        }
                    });
                });
        }

        action
    }

    fn render_inline_leftovers(
        &mut self,
        ui: &mut egui::Ui,
        theme: &PlatformTheme,
        app_id: &str,
        scanning: bool,
        leftover_total: u64,
        selected_bytes: u64,
    ) {
        if scanning {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    RichText::new("Ищем связанные файлы в системе…").color(theme.text_muted),
                );
            });
            return;
        }

        let Some(items) = self.leftovers_by_app.get(app_id).cloned() else {
            ui.label(
                RichText::new("Раскройте строку ещё раз или нажмите Обновить")
                    .color(theme.text_muted),
            );
            return;
        };

        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Связанные файлы")
                    .strong()
                    .color(theme.text_muted),
            );
            ui.label(
                RichText::new(format!(
                    "{} · выбрано {}",
                    format_bytes(leftover_total),
                    format_bytes(selected_bytes)
                ))
                .color(theme.text_muted),
            );
            if ui
                .add_enabled(
                    !self.busy && !items.is_empty(),
                    egui::Button::new("Выбрать все"),
                )
                .clicked()
            {
                self.select_all_related(app_id);
            }
        });

        let mut include_user_data = self
            .include_user_data
            .get(app_id)
            .copied()
            .unwrap_or(false);
        if ui
            .checkbox(&mut include_user_data, "Включая профили и настройки")
            .changed()
        {
            self.set_include_user_data(app_id, include_user_data);
        }
        if include_user_data {
            ui.label(
                RichText::new(
                    "После удаления данные не восстановятся. При новой установке приложение создаст их заново.",
                )
                .small()
                .color(theme.text_muted),
            );
        }

        if items.is_empty() {
            ui.label(
                RichText::new("Связанные файлы не найдены — будет удалено только приложение")
                    .italics()
                    .color(theme.text_muted),
            );
            return;
        }

        for leftover in &items {
            let mut checked = self
                .selected_leftovers
                .get(app_id)
                .is_some_and(|set| set.contains(&leftover.id));
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.busy,
                        egui::Checkbox::new(
                            &mut checked,
                            format!(
                                "{} · {} · {}",
                                leftover.kind.label(),
                                format_bytes(leftover.size_bytes),
                                leftover.description
                            ),
                        ),
                    )
                    .changed()
                {
                    self.toggle_leftover(app_id, leftover, checked);
                }
            });
            ui.label(
                RichText::new(leftover.path.display().to_string())
                    .small()
                    .color(Color32::from_rgb(190, 196, 210)),
            );
        }
    }
}

/// Action requested by the uninstall view.
pub enum UninstallAction {
    None,
    Load,
    ScanLeftovers(String),
    Uninstall(UninstallRequest),
}
