# TuneUp — архитектура и соглашения

Кроссплатформенное (Windows / macOS / Linux) настольное приложение на Rust:
усыпление фоновых приложений, очистка кэша и временных файлов, удаление
программ с остатками. GUI — egui/eframe, работа через системный трей.

> Этот документ описывает **текущую** реализацию. Исторический план
> («заморозка» процессов через `NtSuspendProcess`/`SIGSTOP`, WMI-мониторинг)
> неактуален: сон = **завершение процессов + отключение автозапуска**.

## Workspace

```
tuneup/
├── apps/
│   ├── tuneup/           # GUI: egui, трей, scanner, transitions, maintenance
│   └── tuneup-helper/    # привилегированный helper (Windows named pipe, Linux pkexec)
├── crates/
│   ├── tuneup-core/      # модели, grouping, policy, state (v5), IPC-протокол
│   ├── tuneup-platform/  # трейты платформенных возможностей
│   ├── tuneup-windows/   # реестр, задачи, службы, named-pipe IPC, TerminateProcess
│   ├── tuneup-macos/     # .app, launchd, Login Items, SIGTERM→SIGKILL, `open`
│   └── tuneup-linux/     # DEB/RPM/Flatpak/Snap, XDG, systemd, SIGTERM→SIGKILL
└── .github/workflows/    # CI (linux+windows-cross+macos), Release по тегам vX.Y.Z
```

## Ключевые потоки GUI (`apps/tuneup`)

| Модуль | Роль |
|---|---|
| `main.rs` | single-instance (TCP :47821 c ACK), tray-first окно, платформенные твики winit |
| `app.rs` | состояние UI, политика, применение снапшотов, обработка событий переходов |
| `scanner.rs` | фоновый поток: процессы каждые 2 c, инвентарь каждые 60 c |
| `transition_worker.rs` | фоновый поток сна/пробуждения (graceful kill не блокирует UI) |
| `maintenance_worker.rs` | фоновый поток очистки/удаления |
| `deactivator.rs` | `DeactivationManager`: terminate + disable/restore платформы |
| `single_instance.rs` | вторая копия просит первичную показать окно и выходит |
| `window.rs`, `tray.rs`, `notify.rs`, `logging.rs` | окно/трей/уведомления/логи |

## Режим сна

1. **Усыпить**: расширение группы (`expand_sleep_processes`: потомки,
   Application Support / `.config`, общий JetBrains-демон) → сохранение
   intent в state → переход в фоне: terminate (**SIGTERM → 2 c → SIGKILL** на
   macOS/Linux; TerminateProcess на Windows) → disable автозапуска → persist.
2. **Пробуждение**: restore из `PlatformBackup` (+ опциональный launch).
3. **Автополитика** (`tuneup-core/policy.rs`, опрос каждые 2 c):
   запуск процесса под install root спящей группы → WakeGroup; исчезновение
   процессов под root у активной → SleepGroup.
4. На старте: `reenforce_persisted_sleep` добивает вернувшиеся процессы,
   `heal_orphaned_sleeping_groups` восстанавливает автозапуск групп, чей
   install root исчез/сменён (обновление приложения, удаление) — иначе группа
   осталась бы усыплённой навсегда без UI для пробуждения.

## Состояние

- `state.json` (версия 5) + `state.json.bak`: атомарная запись с fsync;
   `.bak` — предыдущее поколение, используется при повреждении основного.
- `PlatformBackup` (данные отката автозапуска) существует только в state —
  Durability записи критична.
- IPC-ключ Windows хранится только в `%LOCALAPPDATA%\TuneUp\helper.session`.

## Безопасность

- GUI без повышенных прав; helper — фиксированные команды, HMAC-SHA256 IPC
  с nonce, named pipe с ACL (user/SYSTEM/Admins), проверка клиента по
  (`tuneup.exe` в том же каталоге).
- Denylist критических процессов; перед сигналом проверяется
  `(pid, start_time)` от PID reuse.
- Отключение автозапуска только при точном совпадении с install root;
  файлы переносятся в `TuneUpDisabled/`, не удаляются.
- Linux helper — `pkexec` + Polkit policy, stdin JSON без произвольного shell.

## Соглашения

- Rust 1.98.1, edition 2024. Перед коммитом:
  `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
- CI дополнительно гоняет clippy по таргету `x86_64-pc-windows-msvc`
  (весь `#[cfg(windows)]` код) и нативный macOS job — платформенный код
  должен компилироваться всегда.
- Пути нормализуются через `normalize_path` (lowercase только на Windows и
  macOS; Linux регистрочувствителен). Идентификаторы групп —
  `stable_id(normalize_path(install_root))`.
- Интеграционные тесты платформ — `#[ignore]`, запускать в изолированной VM.
- UI-строки — на русском; логи/идентификаторы — на английском.
