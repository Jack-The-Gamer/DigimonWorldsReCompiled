#!/usr/bin/env sh
# Digimon Worlds Re:Compiled - macOS launcher (builds on first run in a source checkout).
cd "$(dirname "$0")" || exit 1
if [ -x ./DigimonWorldsReCompiled ]; then exec ./DigimonWorldsReCompiled "$@"; fi
if [ ! -x target/release/DigimonWorldsReCompiled ]; then
  if ! command -v cargo >/dev/null 2>&1; then
    if [ -x "$HOME/.cargo/bin/cargo" ]; then PATH="$HOME/.cargo/bin:$PATH"; else
      echo "Install Rust from https://rustup.rs and the Xcode command line tools (xcode-select --install) first."
      read -r _
      exit 1
    fi
  fi
  echo "Building Digimon Worlds Re:Compiled (first run only)..."
  cargo build --release -p dwrc || { read -r _; exit 1; }
fi
exec target/release/DigimonWorldsReCompiled "$@"
