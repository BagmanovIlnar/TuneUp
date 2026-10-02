//! Desktop notifications for sleep/wake transitions.

/// Shows a best-effort OS notification. Failures are logged and ignored.
pub fn show(summary: &str, body: &str) {
    let summary = summary.to_owned();
    let body = body.to_owned();
    let _ = std::thread::Builder::new()
        .name("tuneup-notify".into())
        .spawn(move || {
            if let Err(error) = show_impl(&summary, &body) {
                tracing::warn!(%error, %summary, %body, "desktop notification failed");
            } else {
                tracing::debug!(%summary, %body, "desktop notification shown");
            }
        });
}

/// Notifies that an application was put to sleep / deactivated.
pub fn app_deactivated(name: &str, automatic: bool) {
    let body = if automatic {
        format!("«{name}» автоматически деактивировано")
    } else {
        format!("«{name}» деактивировано")
    };
    show("Приложение деактивировано", &body);
}

/// Notifies that a sleeping application was started again / activated.
pub fn app_activated(name: &str, automatic: bool) {
    let body = if automatic {
        format!("«{name}» запущено и активировано")
    } else {
        format!("«{name}» активировано")
    };
    show("Приложение активировано", &body);
}

#[cfg(target_os = "macos")]
fn show_impl(summary: &str, body: &str) -> Result<(), String> {
    // Do not use notify-rust / mac-notification-sys here: it looks up an app named
    // "use_default" via AppleScript and opens "Where is use_default?".
    let script = format!(
        "display notification \"{}\" with title \"{}\" subtitle \"TuneUp\"",
        apple_script_string(body),
        apple_script_string(summary)
    );
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

#[cfg(target_os = "macos")]
fn apple_script_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

#[cfg(target_os = "linux")]
fn show_impl(summary: &str, body: &str) -> Result<(), String> {
    // Prefer notify-send: notify-rust/zbus often fails silently from a short-lived
    // worker thread without a desktop entry / proper session binding.
    let output = std::process::Command::new("notify-send")
        .args([
            "--app-name=TuneUp",
            "--expire-time=5000",
            "--urgency=normal",
            summary,
            body,
        ])
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    // Fall back to notify-rust if notify-send is missing or broken.
    match show_notify_rust(summary, body) {
        Ok(()) => Ok(()),
        Err(fallback) => Err(if stderr.is_empty() {
            fallback
        } else {
            format!("{stderr}; fallback: {fallback}")
        }),
    }
}

#[cfg(all(not(target_os = "macos"), not(target_os = "linux")))]
fn show_impl(summary: &str, body: &str) -> Result<(), String> {
    show_notify_rust(summary, body)
}

#[cfg(not(target_os = "macos"))]
fn show_notify_rust(summary: &str, body: &str) -> Result<(), String> {
    use notify_rust::Notification;

    Notification::new()
        .appname("TuneUp")
        .summary(summary)
        .body(body)
        .timeout(5_000)
        .show()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn macos_notification_uses_osascript_not_use_default() {
        show_impl(
            "Приложение деактивировано",
            "«Visual Studio Code» деактивировано",
        )
        .expect("osascript display notification must succeed");
        show_impl(
            "Приложение активировано",
            "«Visual Studio Code» активировано",
        )
        .expect("wake notification must succeed");
    }

    #[test]
    fn apple_script_escapes_quotes() {
        assert_eq!(apple_script_string(r#"a"b\c"#), r#"a\"b\\c"#);
    }
}
