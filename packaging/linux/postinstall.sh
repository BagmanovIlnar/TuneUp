#!/bin/sh
# Reload polkit rules after installing TuneUp helper policy.
set -e
if command -v systemctl >/dev/null 2>&1; then
  systemctl try-reload-or-restart polkit.service >/dev/null 2>&1 || true
fi
exit 0
