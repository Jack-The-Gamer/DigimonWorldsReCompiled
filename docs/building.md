# Building

Re:Compiled is a standard Cargo workspace. You need **Rust 1.85 or newer** (install from <https://rustup.rs>).

## Windows

1. Run `rustup-init.exe`. When it asks, let it install the Visual Studio C++ build tools.
2. Double-click `Play-Windows.bat` — it builds a release binary (the first build takes a few minutes; later ones only rebuild what changed) and starts the launcher. Or, from a terminal: `cargo build --release`.

## Linux

```sh
# Debian / Ubuntu
sudo apt install build-essential pkg-config libasound2-dev libudev-dev libgtk-3-dev libxkbcommon-dev
# Fedora
sudo dnf install gcc pkg-config alsa-lib-devel systemd-devel gtk3-devel libxkbcommon-devel

./Play-Linux.sh          # or: cargo build --release
```

## macOS

```sh
xcode-select --install
```

Then double-click `Play-Mac.command`, or `cargo build --release`. Unsigned builds downloaded from the internet need right-click → **Open** the first time.

## Outputs

* `target/release/DigimonWorldsReCompiled(.exe)` — the launcher/player.
* `target/release/dwrc-cli(.exe)` — developer tools.

Ship the binary together with the `mods/` folder to include the example mods.

## Continuous integration

`.github/workflows/build.yml` builds and tests on Windows, Linux, macOS (Apple Silicon) and macOS (Intel). Every push uploads build artifacts. Pushing a tag such as `v0.1.0` creates a GitHub release with a zip per platform containing the program, `mods/`, the docs and the launch scripts.
