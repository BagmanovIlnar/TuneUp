#!/usr/bin/env bash
# Builds a release TuneUp.app for macOS (menu-bar / tray app, no Terminal).
#
# Usage (from repo root or any cwd):
#   ./scripts/macos/package-app.sh
#
# Result:
#   target/release/TuneUp.app
#   open target/release/TuneUp.app   # tray icon only (LSUIElement)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

VERSION="${TUNEUP_VERSION:-}"
if [[ -z "${VERSION}" ]]; then
  VERSION="$(
    grep -A20 '^\[workspace.package\]' Cargo.toml \
      | grep '^version' \
      | head -1 \
      | sed -E 's/.*"([^"]+)".*/\1/'
  )"
fi
VERSION="${VERSION:-0.2.0}"

echo "==> cargo build --release -p tuneup (v${VERSION})"
cargo build --release -p tuneup

BIN="${ROOT}/target/release/tuneup"
if [[ ! -x "${BIN}" ]]; then
  echo "error: missing binary ${BIN}" >&2
  exit 1
fi

APP_DIR="${ROOT}/target/release/TuneUp.app"
CONTENTS="${APP_DIR}/Contents"
MACOS="${CONTENTS}/MacOS"
RESOURCES="${CONTENTS}/Resources"

echo "==> packaging ${APP_DIR}"
rm -rf "${APP_DIR}"
mkdir -p "${MACOS}" "${RESOURCES}"

cp "${BIN}" "${MACOS}/tuneup"
chmod +x "${MACOS}/tuneup"

ICON_PLIST=""
TMP="$(mktemp -d)"
cleanup() { rm -rf "${TMP}"; }
trap cleanup EXIT

if command -v python3 >/dev/null 2>&1 \
  && command -v sips >/dev/null 2>&1 \
  && command -v iconutil >/dev/null 2>&1; then
  ICONSET="${TMP}/AppIcon.iconset"
  mkdir -p "${ICONSET}"
  BASE_PNG="${TMP}/base.png"
  python3 - "${BASE_PNG}" <<'PY'
import struct, zlib, sys

path = sys.argv[1]
w = h = 1024

def chunk(tag: bytes, data: bytes) -> bytes:
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )

rows = bytearray()
for y in range(h):
    rows.append(0)
    for x in range(w):
        cx, cy = w / 2, h / 2
        dx = (x + 0.5 - cx) / (w * 0.38)
        dy = (y + 0.5 - cy) / (h * 0.38)
        if dx * dx + dy * dy <= 1.0:
            rows.extend((40, 120, 220, 255))
        else:
            rows.extend((0, 0, 0, 0))

png = b"\x89PNG\r\n\x1a\n"
png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
png += chunk(b"IDAT", zlib.compress(bytes(rows), 9))
png += chunk(b"IEND", b"")
open(path, "wb").write(png)
PY
  # Apple iconset names / pixel sizes
  declare -a PAIRS=(
    "icon_16x16.png:16"
    "icon_16x16@2x.png:32"
    "icon_32x32.png:32"
    "icon_32x32@2x.png:64"
    "icon_128x128.png:128"
    "icon_128x128@2x.png:256"
    "icon_256x256.png:256"
    "icon_256x256@2x.png:512"
    "icon_512x512.png:512"
    "icon_512x512@2x.png:1024"
  )
  for pair in "${PAIRS[@]}"; do
    name="${pair%%:*}"
    size="${pair##*:}"
    sips -z "${size}" "${size}" "${BASE_PNG}" --out "${ICONSET}/${name}" >/dev/null
  done
  if iconutil -c icns "${ICONSET}" -o "${RESOURCES}/AppIcon.icns" 2>/dev/null; then
    ICON_PLIST="
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>"
  else
    echo "warning: iconutil failed, packaging without AppIcon.icns" >&2
  fi
fi

cat > "${CONTENTS}/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleExecutable</key>
  <string>tuneup</string>
  <key>CFBundleIdentifier</key>
  <string>com.tuneup.app</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>TuneUp</string>
  <key>CFBundleDisplayName</key>
  <string>TuneUp</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${VERSION}</string>
  <key>CFBundleVersion</key>
  <string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key>
  <string>12.0</string>
  <key>LSUIElement</key>
  <true/>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSAppleEventsUsageDescription</key>
  <string>TuneUp управляет Login Items и уведомлениями через System Events.</string>${ICON_PLIST}
</dict>
</plist>
EOF

if command -v codesign >/dev/null 2>&1; then
  codesign --force --deep --sign - "${APP_DIR}" >/dev/null 2>&1 || true
fi

echo
echo "Готово: ${APP_DIR}"
echo "Запуск:  open \"${APP_DIR}\""
echo "Иконка в строке меню (трей). Окно — по клику на иконку."
echo
if [[ "${SKIP_OPEN:-0}" == "1" ]]; then
  echo "SKIP_OPEN=1 — приложение не запускается (CI)."
else
  open "${APP_DIR}"
fi
