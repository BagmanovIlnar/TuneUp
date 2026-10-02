#!/usr/bin/env bash
# Builds Linux tarball + .deb + .rpm for TuneUp.
#
# Prerequisites: release binaries already built:
#   cargo build --release -p tuneup -p tuneup-helper
#
# Usage:
#   TUNEUP_VERSION=1.0.0 ./scripts/ci/package-linux.sh
#
# Outputs in the repo root:
#   tuneup-${VERSION}-linux-x64.tar.gz
#   tuneup_${VERSION}_amd64.deb
#   tuneup-${VERSION}-1.x86_64.rpm
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
VERSION="${VERSION:?version required}"

for bin in target/release/tuneup target/release/tuneup-helper; do
  if [[ ! -x "${bin}" ]]; then
    echo "error: missing ${bin}; run cargo build --release -p tuneup -p tuneup-helper first" >&2
    exit 1
  fi
done

ARTIFACT="tuneup-${VERSION}-linux-x64"
STAGE="dist/${ARTIFACT}"
rm -rf "${STAGE}"
mkdir -p "${STAGE}"
cp target/release/tuneup target/release/tuneup-helper "${STAGE}/"
tar -C dist -czf "${ARTIFACT}.tar.gz" "${ARTIFACT}"
echo "wrote ${ARTIFACT}.tar.gz"

# Install nfpm if needed
if ! command -v nfpm >/dev/null 2>&1; then
  NFPM_VERSION="${NFPM_VERSION:-2.41.3}"
  echo "==> installing nfpm v${NFPM_VERSION}"
  TMP="$(mktemp -d)"
  curl -fsSL \
    "https://github.com/goreleaser/nfpm/releases/download/v${NFPM_VERSION}/nfpm_${NFPM_VERSION}_Linux_x86_64.tar.gz" \
    | tar -xz -C "${TMP}" nfpm
  install -m 0755 "${TMP}/nfpm" /usr/local/bin/nfpm 2>/dev/null \
    || install -m 0755 "${TMP}/nfpm" "${HOME}/.local/bin/nfpm"
  export PATH="${HOME}/.local/bin:${PATH}"
  rm -rf "${TMP}"
fi

CFG="$(mktemp)"
cleanup() { rm -f "${CFG}"; }
trap cleanup EXIT

# Substitute ${VERSION} in the nfpm template (no other expansion).
sed "s/\${VERSION}/${VERSION}/g" packaging/linux/nfpm.yaml.tpl > "${CFG}"

chmod +x packaging/linux/postinstall.sh

nfpm pkg --packager deb --config "${CFG}" --target "tuneup_${VERSION}_amd64.deb"
nfpm pkg --packager rpm --config "${CFG}" --target "tuneup-${VERSION}-1.x86_64.rpm"

echo "wrote tuneup_${VERSION}_amd64.deb"
echo "wrote tuneup-${VERSION}-1.x86_64.rpm"
ls -lh "${ARTIFACT}.tar.gz" "tuneup_${VERSION}_amd64.deb" "tuneup-${VERSION}-1.x86_64.rpm"
