#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

MODE="release"
if [[ "${1:-}" == "--dev" ]]; then
  MODE="dev"
elif [[ "${1:-}" == "--release" || -z "${1:-}" ]]; then
  MODE="release"
else
  echo "Usage: $0 [--dev|--release]" >&2
  exit 1
fi

if ! rustup target list --installed | grep -qx 'wasm32-unknown-unknown'; then
  echo "Missing Rust target 'wasm32-unknown-unknown'. Install it with:" >&2
  echo "  rustup target add wasm32-unknown-unknown" >&2
  exit 1
fi

if ! command -v wasm-bindgen >/dev/null 2>&1; then
  WB_VER="$(awk '/^name = "wasm-bindgen"$/{getline; gsub(/"/, "", $3); print $3; exit}' Cargo.lock)"
  echo "Missing 'wasm-bindgen' CLI. Install the matching version with:" >&2
  echo "  cargo install --locked wasm-bindgen-cli --version ${WB_VER}" >&2
  exit 1
fi

export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-zstack-size=4194304"

CARGO_ARGS=(build -p shulker-wasm --target wasm32-unknown-unknown)
PROFILE_DIR="debug"
if [[ "$MODE" == "release" ]]; then
  CARGO_ARGS+=(--release)
  PROFILE_DIR="release"
fi

echo "Building shulker-wasm ($MODE)..."
cargo "${CARGO_ARGS[@]}"

WASM_PATH="target/wasm32-unknown-unknown/${PROFILE_DIR}/shulker_wasm.wasm"
OUT_DIR="web/pkg"
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

echo "Generating JS/WASM bindings into $OUT_DIR..."
wasm-bindgen --target web --no-typescript --out-dir "$OUT_DIR" "$WASM_PATH"

BG_WASM="$OUT_DIR/shulker_wasm_bg.wasm"
if [[ "$MODE" == "release" ]] && command -v wasm-opt >/dev/null 2>&1; then
  echo "Optimizing with wasm-opt -Oz..."
  wasm-opt -Oz "$BG_WASM" -o "$BG_WASM"
fi

RAW_BYTES="$(wc -c < "$BG_WASM")"
GZIP_BYTES="$(gzip -c -9 "$BG_WASM" | wc -c)"
echo "Built $BG_WASM: $((RAW_BYTES / 1024)) KiB raw ($((RAW_BYTES / 1024 / 1024)) MiB), $((GZIP_BYTES / 1024)) KiB gzipped"
