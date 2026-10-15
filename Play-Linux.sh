#!/usr/bin/env sh
# Digimon Worlds Re:Compiled - Linux launcher (builds on first run in a source checkout).
cd "$(dirname "$0")" || exit 1
if [ -x ./DigimonWorldsReCompiled ]; then exec ./DigimonWorldsReCompiled "$@"; fi
if [ ! -x target/release/DigimonWorldsReCompiled ]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Install Rust from https://rustup.rs first (see docs/building.md for system packages)."
    exit 1
  fi
  echo "Building Digimon Worlds Re:Compiled (first run only)..."
  cargo build --release -p dwrc || exit 1
fi
exec target/release/DigimonWorldsReCompiled "$@"
