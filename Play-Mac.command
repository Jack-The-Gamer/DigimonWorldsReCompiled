#!/usr/bin/env sh
# Digimon Worlds Re:Compiled - macOS launcher.
# In a source checkout it rebuilds first (only what changed), so the newest
# code always runs; in a release download it just starts the program.
cd "$(dirname "$0")" || exit 1
if [ ! -f Cargo.toml ] && [ -x ./DigimonWorldsReCompiled ]; then exec ./DigimonWorldsReCompiled "$@"; fi
if ! command -v cargo >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo" ]; then PATH="$HOME/.cargo/bin:$PATH"; fi
if ! command -v cargo >/dev/null 2>&1; then
  if [ -x target/release/DigimonWorldsReCompiled ]; then
    echo "Rust was not found, so the program cannot be updated. Starting the existing build."
    exec target/release/DigimonWorldsReCompiled "$@"
  fi
  echo "Install Rust from https://rustup.rs and the Xcode command line tools (xcode-select --install) first."
  read -r _
  exit 1
fi
echo "Building Digimon Worlds Re:Compiled..."
cargo build --release -p dwrc || { read -r _; exit 1; }
exec target/release/DigimonWorldsReCompiled "$@"
