//! Shared leftover deletion helpers for uninstall providers.

use tuneup_core::{
    TuneupError,
    fs_util::{is_protected_root, remove_within},
    uninstall::{LeftoverItem, UninstallReport},
};

/// Deletes selected leftovers, skipping user data unless explicitly allowed.
pub fn remove_selected_leftovers(
    leftovers: &[LeftoverItem],
    selected_ids: &[String],
    include_user_data: bool,
) -> Result<UninstallReport, TuneupError> {
    let mut report = UninstallReport {
        uninstalled: true,
        ..UninstallReport::default()
    };
    for leftover in leftovers
        .iter()
        .filter(|item| selected_ids.contains(&item.id))
    {
        if leftover.kind.is_user_data() && !include_user_data {
            report.warnings.push(format!(
                "пропущены пользовательские данные: {}",
                leftover.path.display()
            ));
            continue;
        }
        if is_protected_root(&leftover.path) {
            report
                .remaining_leftovers
                .push(leftover.path.display().to_string());
            continue;
        }
        let Some(parent) = leftover.path.parent() else {
            report
                .remaining_leftovers
                .push(leftover.path.display().to_string());
            continue;
        };
        match remove_within(&leftover.path, parent) {
            Ok(bytes) => {
                report.leftovers_freed_bytes = report.leftovers_freed_bytes.saturating_add(bytes);
            }
            Err(error) => {
                report
                    .remaining_leftovers
                    .push(leftover.path.display().to_string());
                report.warnings.push(error.to_string());
            }
        }
    }
    Ok(report)
}
