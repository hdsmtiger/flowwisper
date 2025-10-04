#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export FLOWWISPER_ROOT="$ROOT_DIR"

section() {
  echo -e "\n==== $1 ====\n"
}

section "Rust core: cargo test"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"
export CARGO_TARGET_DIR
cargo test --manifest-path "$ROOT_DIR/core/Cargo.toml"

section "Desktop shell: npm install & build"
pushd "$ROOT_DIR/apps/desktop" >/dev/null
npm install
npm run build
if [[ "${RUN_TAURI_BUNDLE:-0}" == "1" ]]; then
  if npx --yes tauri --help >/dev/null 2>&1; then
    npx --yes tauri build
  else
    echo "(skip) tauri CLI not available; skipping native bundle"
  fi
else
  echo "(skip) RUN_TAURI_BUNDLE!=1; skipping native bundling"
fi
popd >/dev/null

section "Admin console: npm install & next build"
pushd "$ROOT_DIR/services/admin_console" >/dev/null
npm install
npm run build
popd >/dev/null

section "Hybrid router: go test"
pushd "$ROOT_DIR/services/hybrid_router" >/dev/null
go test ./...
popd >/dev/null

section "API gateway: venv install & pytest"
pushd "$ROOT_DIR/services/api_gateway" >/dev/null
PYTHON_BIN="${PYTHON:-python3}"
VENV_DIR=".venv"
$PYTHON_BIN -m venv "$VENV_DIR"
source "$VENV_DIR/bin/activate"
pip install --upgrade pip
pip install -e .[dev]
pytest
deactivate
popd >/dev/null

section "Build finished"
