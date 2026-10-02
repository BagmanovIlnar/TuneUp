# TuneUp

**TuneUp** — кроссплатформенное настольное приложение на Rust для ускорения
системы: усыпление фоновых приложений (модель AVG Sleep Mode), очистка кэша и
временных файлов, удаление программ с остатками.

Версия workspace: **0.2.0** · Rust **1.98+** · лицензия **MIT**

Приложение работает в **системном трее**. Главное окно открывается кликом по
иконке или пунктом **«Открыть»**. Закрытие окна скрывает его в трей; полный выход —
только через **«Выйти»** (при этом восстанавливаются отключённые точки автозапуска
спящих групп).

---

## Оглавление

1. [Что умеет](#что-умеет)
2. [Разделы интерфейса](#разделы-интерфейса)
3. [Режим сна (Sleep Mode)](#режим-сна-sleep-mode)
4. [Платформы](#платформы)
5. [Требования](#требования)
6. [Быстрый старт на Ubuntu](#быстрый-старт-на-ubuntu)
7. [Установка Rust](#установка-rust)
8. [Получение исходников](#получение-исходников)
9. [Релиз через GitHub Actions](#релиз-через-github-actions)
10. [Сборка](#сборка)
11. [Запуск](#запуск)
12. [Тесты и проверка качества](#тесты-и-проверка-качества)
13. [Структура проекта](#структура-проекта)
14. [Состояние и журналы](#состояние-и-журналы)
15. [Безопасность](#безопасность)
16. [Типичные проблемы](#типичные-проблемы)
17. [Ограничения](#ограничения)

---

## Что умеет

| Возможность | Описание |
|---|---|
| **Сон приложений** | Принудительно завершает процессы группы и отключает автозапуск; пробуждение восстанавливает автозапуск и может сразу запустить приложение |
| **Группировка** | Связывает процессы, install root (`.app` / каталог установки), точки автозапуска |
| **Нагрузка** | Оценка по CPU, памяти, числу процессов и автозапусков |
| **Очистка** | Сканирование и удаление кэша, временных файлов, корзины (с предпросмотром) |
| **Удаление** | Штатный uninstall ОС + выбор связанных остатков (профили — только явно) |
| **Настройки** | Автозапуск самого TuneUp при входе в систему (все ОС) |
| **Состояние** | `state.json`; спящие группы **сохраняются** между запусками TuneUp |
| **Журналы** | Ежедневные файловые логи, уровень через `TUNEUP_LOG` |

---

## Разделы интерфейса

Боковое меню:

| Раздел | Назначение |
|---|---|
| **Сон** | Список приложений, усыпление / пробуждение, фильтры, детали процессов |
| **Очистка** | Скан безопасных кандидатов → выбор → удаление |
| **Удаление** | Установленные программы → остатки → uninstall |
| **Настройки** | Запуск TuneUp при входе в систему |

Список процессов обновляется примерно каждые **2 секунды**.

---

## Режим сна (Sleep Mode)

Поведение как у **AVG Sleep Mode**: не «заморозка» в RAM (`SIGSTOP`), а
**завершение процессов** + **отключение автозапуска**.

### Усыпить

1. Раздел **Сон** → приложение → **Усыпить**.
2. Подтвердите: процессы будут **завершены**, автозапуск — **отключён**.
3. Опционально: **«Автоматически пробуждать и повторно усыплять»**.

TuneUp:

- расширяет группу (дочерние процессы, каталоги Application Support / AppData /
  `.config`, общий `jetbrainsd` при безопасных условиях);
- отклоняет системные / denylist-процессы;
- шлёт **SIGKILL** (macOS/Linux) или **TerminateProcess** (Windows);
- отключает только точно сопоставленные LaunchAgents, Login Items, Run/Startup,
  tasks, services, XDG, systemd units;
- сохраняет backup для отката.

### Разбудить (кнопка)

1. Восстанавливает точки автозапуска из backup.
2. **Запускает** приложение (`open` / `ShellExecute` / `xdg-open`).

### Автоматическая политика (`AutoSleepWake`)

| Событие | Действие |
|---|---|
| Появился процесс **внутри install root** спящей группы | Восстановить автозапуск (**без** повторного launch) + уведомление |
| Не осталось процессов под install root | Снова усыпить: terminate связанных (включая хелперы вне root) + disable |
| Группа `Ignored` | Не трогается автоматически |

«Приложение запущено» определяется по путям **под install root** (например
`RustRover.app`). Общий daemon вроде `jetbrainsd` вне бандла **не** держит
группу активной, но при усыплении всё равно может быть завершён как связанный.

### Ручная политика (`Manual`)

Усыпление то же (kill + disable). Пока статус «усыплено», повторный ручной запуск
приложения **сам** не останавливается и не будит группу — нужен **Разбудить**
или включённый авто-режим.

### Выход из TuneUp

Пункт трея **Выйти** сохраняет state и завершает процесс. **Спящие приложения
остаются усыплёнными** (автозапуск по-прежнему отключён), пока вы не нажмёте
**Разбудить**. При следующем старте TuneUp снова покажет их как Sleeping и при
необходимости повторно завершит процессы, которые успели запуститься.

---

## Платформы

| | Windows 10/11 | macOS | Linux |
|---|:---:|:---:|:---:|
| GUI + трей | Да | Да | Да |
| Sleep / Wake | Да | Да | Да |
| Очистка | Да | Да | Да |
| Удаление | Win32/MSI/Store* | `.app` / Homebrew Cask | DEB/RPM/Flatpak/Snap |
| Автозапуск приложений | Run, Startup, Tasks, Services | LaunchAgents/Daemons, Login Items | XDG, systemd |
| Привилегии | UAC + `tuneup-helper` | Admin dialog | `pkexec` / Polkit |

\* Store — при наличии штатного идентификатора удаления.

Современные macOS Background Items без публичного API Apple только
отображаются, не изменяются.

---

## Требования

**Общее:** графическая сессия, Rust **1.98.1**, Cargo, Git (для клона).

### Windows

- Windows 10/11 x64
- Visual Studio 2022 Build Tools + **Desktop development with C++**
- Windows SDK
- target `x86_64-pc-windows-msvc`
- для release helper нужен Windows-хост (`rc.exe` встраивает UAC manifest)

### macOS

```bash
xcode-select --install
```

Для Login Items может понадобиться разрешение **System Events**
(System Settings → Privacy & Security → Automation).

### Linux (пример Ubuntu/Debian)

Нужна **графическая сессия** (X11 или Wayland). Пакеты:

```bash
sudo apt update
sudo apt install -y \
  build-essential pkg-config \
  polkitd pkexec systemd
```

| Пакет | Зачем |
|---|---|
| `build-essential`, `pkg-config` | компилятор C / систем tools |
| `polkitd`, `pkexec` | привилегии для `tuneup-helper` |

Трей на Linux идёт через **StatusNotifierItem** (`tray-icon` feature `ksni`, D-Bus) —
отдельные `libgtk-3-dev` / `libayatana-appindicator3-dev` **не нужны**.
На рабочем столе должен быть SNI-хост (GNOME с расширением AppIndicator/SNI,
KDE, Cosmic и т.п.).

На **Ubuntu 24.04 и новее** пакет `policykit-1` убран — ставьте `polkitd pkexec`.
На старых Ubuntu/Debian вместо них ещё встречается metapackage `policykit-1`.

---

## Быстрый старт на Ubuntu

Полный путь «с нуля» до работающего приложения (Ubuntu 22.04–26.04, x86_64).

### 1. Системные зависимости

```bash
sudo apt update
sudo apt install -y \
  build-essential pkg-config curl git \
  polkitd pkexec systemd
```

Если `apt` пишет `Could not get lock .../lists/lock` — подождите окончания другого
`apt`/`unattended-upgrades` и повторите (не удаляйте lock-файл вручную).

### 2. Rust 1.98.1

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup toolchain install 1.98.1
```

### 3. Исходники и toolchain проекта

```bash
git clone <repository-url> tuneup
cd tuneup
rustup override set 1.98.1
rustc --version   # ожидается rustc 1.98.1 ...
```

### 4. Сборка и запуск

```bash
# debug (разработка)
cargo build -p tuneup -p tuneup-helper
cargo run -p tuneup

# или release
cargo build --release -p tuneup -p tuneup-helper
./target/release/tuneup
```

После старта окно **скрыто**: откройте его кликом по иконке в трее (или пунктом
**«Открыть»**). `tuneup-helper` вручную не запускайте — GUI вызовет его через
`pkexec`, когда понадобятся права.

Подробные логи:

```bash
TUNEUP_LOG=debug cargo run -p tuneup
```

Логи и state на Linux: `~/.local/state/TuneUp/` (или `$XDG_STATE_HOME/TuneUp/`).

---

## Установка Rust

```bash
# https://rustup.rs/
rustup toolchain install 1.98.1
cd /path/to/tuneup
rustup override set 1.98.1
rustc --version   # rustc 1.98.1 (48a229cea 2026-09-01)
cargo --version
```

---

## Получение исходников

```bash
git clone <repository-url> tuneup
cd tuneup
rustup override set 1.98.1
```

Или уже локально:

```bash
cd /path/to/tuneup
```

---

## Релиз через GitHub Actions

Версия релиза берётся из **git-тега** `vX.Y.Z` (например `v1.0.0` → version `1.0.0`).

1. Убедитесь, что изменения запушены в GitHub.
2. Создайте и отправьте тег:

```bash
git tag v1.0.0
git push origin v1.0.0
```

3. Workflow **Release** (`.github/workflows/release.yml`):
   - выставит `[workspace.package].version` в `Cargo.toml` на CI;
   - соберёт артефакты:
     - macOS: `tuneup-1.0.0-macos.zip` (`TuneUp.app`)
     - Windows: `tuneup-1.0.0-windows-x64.zip` (`tuneup.exe` + `tuneup-helper.exe`)
     - Linux: `tuneup-1.0.0-linux-x64.tar.gz`, **`tuneup_1.0.0_amd64.deb`**, **`tuneup-1.0.0-1.x86_64.rpm`**
       (в пакеты входят `/usr/bin/tuneup`, `/usr/libexec/tuneup-helper`, Polkit policy)
   - опубликует GitHub Release с этими файлами.

Локально версию в репозитории можно не менять заранее — для CI достаточно тега.
Для локальной сборки `.app` с нужной версией:

```bash
./scripts/ci/set-workspace-version.sh 1.0.0
./scripts/macos/package-app.sh
```

Локально Linux `.deb` / `.rpm` (нужен `curl`, x86_64 Linux):

```bash
./scripts/ci/set-workspace-version.sh 1.0.0
cargo build --release -p tuneup -p tuneup-helper
TUNEUP_VERSION=1.0.0 ./scripts/ci/package-linux.sh
# → tuneup-1.0.0-linux-x64.tar.gz
# → tuneup_1.0.0_amd64.deb
# → tuneup-1.0.0-1.x86_64.rpm
```

Установка:

```bash
sudo dpkg -i tuneup_1.0.0_amd64.deb
# или
sudo rpm -Uvh tuneup-1.0.0-1.x86_64.rpm
```

Workflow **CI** (`.github/workflows/ci.yml`) на push/PR: `fmt`, `test`, `clippy`.

---

## Сборка

Все команды — из **корня workspace**.

### Релизная сборка (основная)

**macOS / Linux (бинарник):**

```bash
cargo build --release -p tuneup
```

Бинарник: `target/release/tuneup`

Запуск из терминала:

```bash
./target/release/tuneup
```

**macOS — пакет `.app` (рекомендуется, без окна Terminal):**

```bash
./scripts/macos/package-app.sh
```

Скрипт соберёт release и создаст `target/release/TuneUp.app`, затем запустит его.
Иконка появляется **только в строке меню** (трей); главное окно — по клику на иконку.

Повторный запуск:

```bash
open target/release/TuneUp.app
```

**Windows** (GUI + helper в одном каталоге):

```powershell
cargo build --release -p tuneup -p tuneup-helper
```

Бинарники: `target\release\tuneup.exe`, `target\release\tuneup-helper.exe`

```powershell
.\target\release\tuneup.exe
```

### Быстрая проверка типов

```bash
cargo check --workspace
```

Только GUI:

```bash
cargo check -p tuneup
```

### Debug

```bash
cargo build --workspace
# или только приложение:
cargo build -p tuneup
```

Артефакты:

| ОС | Путь |
|---|---|
| macOS / Linux | `target/debug/tuneup` |
| Windows | `target\debug\tuneup.exe`, `target\debug\tuneup-helper.exe` |

### Release (подробно)

**macOS** (helper не нужен):

```bash
cargo build --release -p tuneup
# → target/release/tuneup

# Рекомендуемый GUI-пакет без Terminal:
./scripts/macos/package-app.sh
# → target/release/TuneUp.app
```

**Linux** (пользовательские операции — без helper; system — через helper + pkexec):

```bash
cargo build --release -p tuneup
# при необходимости также:
cargo build --release -p tuneup-helper
```

**Windows** (helper рядом с GUI обязателен):

```powershell
cargo build --release -p tuneup -p tuneup-helper
# → target\release\tuneup.exe
# → target\release\tuneup-helper.exe
```

`tuneup-helper.exe` должен лежать **в том же каталоге**, что и `tuneup.exe`.

### Очистка артефактов

```bash
cargo clean
```

### Cross-check Windows с macOS/Linux (типы, не готовый exe)

```bash
rustup target add x86_64-pc-windows-msvc
cargo check --workspace --all-targets --target x86_64-pc-windows-msvc
```

Полноценный Windows-бинарник и UAC-manifest helper — только на Windows-хосте.

---

## Запуск

### Рекомендуемый способ разработки

```bash
cargo run -p tuneup
```

Release без переустановки:

```bash
cargo run --release -p tuneup
```

### Готовый бинарник

```bash
# Debug
./target/debug/tuneup

# Release
./target/release/tuneup
```

Windows PowerShell:

```powershell
.\target\debug\tuneup.exe
.\target\release\tuneup.exe
```

После старта окно **скрыто**: работают процесс и иконка в трее / строке меню.
Откройте окно кликом по иконке или пунктом **«Открыть»**. Если создать трей не
удалось, приложение само покажет главное окно.

Повторный запуск не создаёт второй процесс: уже работающий экземпляр получает
команду показать окно, а новый процесс сразу завершается.

Helper **не** запускайте вручную: на Windows он поднимается через UAC при
нужде; на Linux — через `pkexec`.

### Подробные логи при запуске

```bash
TUNEUP_LOG=debug cargo run -p tuneup
```

```powershell
$env:TUNEUP_LOG = "debug"
cargo run -p tuneup
```

---

## Использование (кратко)

### Сон

См. [Режим сна](#режим-сна-sleep-mode).

### Очистка

1. **Очистка** → **Сканировать**
2. Проверьте категории (системные пути по умолчанию сняты)
3. **Очистить выбранное**

### Удаление

1. **Удаление** → выберите программу → остатки
2. При необходимости **Удалить профили и настройки**
3. Подтвердите: сначала штатный uninstall ОС, затем отмеченные пути

### Настройки

Включите **запуск при входе**, если TuneUp должен стартовать вместе с сессией.

---

## Тесты и проверка качества

```bash
# Форматирование
cargo fmt --all --check

# Unit-тесты всего workspace
cargo test --workspace

# Только ядро
cargo test -p tuneup-core

# Clippy (строго)
cargo clippy --workspace --all-targets -- -D warnings

# Полный локальный цикл
cargo fmt --all --check \
  && cargo test --workspace \
  && cargo clippy --workspace --all-targets -- -D warnings
```

### Integration-тесты Windows (`#[ignore]`)

Только в изолированной VM — меняют реестр / Startup / tasks / services:

```powershell
cargo test -p tuneup-windows `
  --test windows_integration `
  -- --ignored --nocapture
```

Опционально:

```powershell
$env:TUNEUP_TEST_TASK = "\TuneUp\TestTask"
$env:TUNEUP_TEST_SERVICE = "TuneUpTestService"
$env:TUNEUP_TEST_SERVICE_START = "3"
```

### Integration macOS (часть `#[ignore]`)

```bash
cargo test -p tuneup-macos -- --ignored --nocapture
```

---

## Структура проекта

```text
tuneup/
├── apps/
│   ├── tuneup/           # GUI (eframe/egui), трей, scanner, Sleep/Cleanup/Uninstall
│   └── tuneup-helper/    # привилегированный helper (Windows / Linux pkexec)
├── crates/
│   ├── tuneup-core/      # модели, grouping, policy, state, IPC
│   ├── tuneup-platform/  # traits: ProcessControl, AppLauncher, PlatformMutator, …
│   ├── tuneup-windows/   # inventory, terminate, ShellExecute, registry/tasks
│   ├── tuneup-macos/     # .app, launchd, SIGKILL, open
│   └── tuneup-linux/     # пакеты, XDG, systemd, SIGKILL, xdg-open
├── Cargo.toml
└── README.md
```

| Крейт | Роль |
|---|---|
| `tuneup-core` | Группы, `SleepPolicyEngine`, `StateStore` (version 5), ошибки |
| `tuneup-platform` | `ProcessControl::terminate`, `AppLauncher`, mutators, cleanup/uninstall traits |
| `tuneup-windows` / `-macos` / `-linux` | Платформенные реализации |
| `tuneup` | Оркестрация UI и deactivator |
| `tuneup-helper` | Elevated операции по фиксированному IPC |

---

## Состояние и журналы

Формат state: JSON, текущая версия **5** (Sleep = terminate, не freeze).

### Windows

```text
%LOCALAPPDATA%\TuneUp\state.json
%LOCALAPPDATA%\TuneUp\helper.session
%LOCALAPPDATA%\TuneUp\logs\tuneup.log.YYYY-MM-DD
%LOCALAPPDATA%\TuneUp\logs\helper.log.YYYY-MM-DD
```

### macOS

```text
~/Library/Application Support/TuneUp/state.json
~/Library/Logs/TuneUp/logs/tuneup.log.YYYY-MM-DD
```

### Linux

```text
$XDG_STATE_HOME/TuneUp/state.json          # или ~/.local/state/TuneUp/
$XDG_STATE_HOME/TuneUp/logs/...            # или ~/TuneUp/logs/
/var/log/TuneUp/helper.log.YYYY-MM-DD      # system helper
```

При повреждении основного файла загружается `state.json.bak`. Перед ручным
удалением state завершите TuneUp через трей.

Секреты IPC в логи не пишутся.

---

## Безопасность

- GUI без повышенных прав.
- UAC / admin dialog / Polkit — только для системных объектов.
- Denylist критических процессов (launchd, csrss, explorer, …).
- Перед kill — проверка `(pid, start_time)` против reuse PID.
- Отключение автозапуска только при точном совпадении с install root.
- Startup-файлы переносятся в `TuneUpDisabled`, не удаляются навсегда.
- Helper: фиксированные команды, HMAC IPC, без произвольного shell.

**Не усыпляйте** системную оболочку, антивирус, драйверы и критичные сервисы.

---

## Типичные проблемы

| Симптом | Что делать |
|---|---|
| Нет окна после запуска | Откройте иконку в трее / menu bar |
| После сна остаётся «Running in Background» / daemon | Убедитесь, что сборка свежая; при Auto после закрытия IDE группа должна усыпиться повторно и добить хелперы |
| Повторный запуск без уведомления (Manual) | Без галочки авто группа не будится сама — нажмите **Разбудить** или включите авто |
| Helper не найден (Windows) | Положите `tuneup-helper.exe` рядом с `tuneup.exe` |
| Manifest helper без UAC | Собирайте helper на Windows-хосте |
| `Package gio-2.0 was not found` | Устаревшая подсказка для GTK-трея; текущая сборка использует `ksni` и GTK не требует |
| `policykit-1 has no installation candidate` | Ставьте `polkitd pkexec` (Ubuntu 24.04+) |
| `Could not get lock ... apt` | Дождитесь другого `apt`; lock не удалять |
| `winit`: platform not supported (Linux) | В `apps/tuneup` для Linux уже включены фичи `x11`/`wayland` у `winit`; обновите код и пересоберите |
| `Glutin ... native window is not supported` | Нужны фичи `wayland`/`x11`/`glx` у `glutin` (уже в `apps/tuneup` для Linux); пересоберите |
| `GTK has not been initialized` / AppIndicator CRITICAL | Старый GTK-бэкенд трея; нужна сборка с `tray-icon` feature `ksni` (уже в репо) |
| Окно сразу видно / не закрывается в трей (Linux Wayland) | Нужен `DISPLAY` (XWayland): приложение само форсит X11. Проверьте `echo $DISPLAY` |
| Нет иконки в трее (GNOME) | Нужен SNI/AppIndicator host в DE (расширение «AppIndicator and KStatusNotifierItem») |
| Запрос пароля админа | Нормально (`pkexec`); отмена → rollback |
| Нужен разбор бага | `TUNEUP_LOG=debug` и смотрите лог за день |

---

## Ограничения

- Готовых installer / notarization / auto-update пока нет.
- Privileged Windows/Linux сценарии лучше проверять в VM.
- macOS Background Items ограничены публичными API Apple.
- Полнота «всех следов» при uninstall не гарантируется — удаляются только
  обнаруженные и подтверждённые пути.

---

## Лицензия

MIT (см. `license` в workspace `Cargo.toml`).
