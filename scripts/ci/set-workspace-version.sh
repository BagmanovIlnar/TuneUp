#!/usr/bin/env bash
# Sets [workspace.package].version in the root Cargo.toml.
# Usage: ./scripts/ci/set-workspace-version.sh 1.0.0
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VERSION="${1:-}"

if [[ -z "${VERSION}" ]]; then
  echo "usage: $0 <semver>" >&2
  exit 1
fi

if [[ ! "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
  echo "error: invalid semver '${VERSION}'" >&2
  exit 1
fi

python3 - "${ROOT}/Cargo.toml" "${VERSION}" <<'PY'
import pathlib, re, sys

path = pathlib.Path(sys.argv[1])
version = sys.argv[2]
text = path.read_text(encoding="utf-8")
pattern = re.compile(
    r"(?m)(^\[workspace\.package\]\s*\n(?:^(?!\[).*\n)*?^version\s*=\s*\")[^\"]+(\")",
)
updated, count = pattern.subn(rf"\g<1>{version}\g<2>", text, count=1)
if count != 1:
    raise SystemExit("error: could not find [workspace.package] version in Cargo.toml")
path.write_text(updated, encoding="utf-8")
print(f"workspace.package.version = {version}")
PY
