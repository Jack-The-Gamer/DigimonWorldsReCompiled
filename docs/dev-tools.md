# Developer tools (`dwrc-cli`)

Build with `cargo build --release -p dwrc-cli`; the tool is `target/release/dwrc-cli`.

| Command | |
|---|---|
| `dwrc-cli inspect <cue>` | Tracks, ISO volume, identification, every file with its LBA and size, `SYSTEM.CNF`, the boot executable header. |
| `dwrc-cli extract <cue> <path> <out>` | Extract a file from the disc's filesystem. |
| `dwrc-cli disasm <exe> <hex-addr> <count>` | Disassemble a PS-X EXE. |
| `dwrc-cli import <data-root> <folder>` | Scan a folder into a library (same code as the launcher). |
| `dwrc-cli run <cue> --bios <bin> [options]` | Run a game headlessly. |
| `dwrc-cli mod validate <folder>` | Check a mod folder (manifest, files, versions, dependencies). |
| `dwrc-cli mod pack <folder> [--out DIR]` | Build `<id>-<version>.zip`, the asset a GitHub release should carry. |
| `dwrc-cli mod install <zip> [--data DIR] [--replace]` | Install a mod `.zip` (same code as the launcher). |
| `dwrc-cli mod list [--data DIR]` | Installed mods in load order, with the games each is switched on for. |
| `dwrc-cli mod index <owner/repo or URL>` | Read a mod index feed and list its mods. |
| `dwrc-cli mod releases <owner/repo> <id>` | List a mod's installable GitHub releases. |

`run` options:

| | |
|---|---|
| `--frames N` | frames to run (default 600) |
| `--fast` | fast boot |
| `--engine interp\|recomp\|validate` | CPU engine (default interp) |
| `--shots DIR --every K` | save a PNG every K frames, plus `vram.png` at the end |
| `--wav FILE` | record audio |
| `--input "frame:button:len,..."` | scripted input, e.g. `1000:start:10,1200:x:6` (buttons: start select x o t s up down left right l1 r1 l2 r2) |
| `--mod DIR` | enable a mod folder (repeatable) |
| `--tty` | echo the BIOS/game debug console |
| `--dump-entry` | print the hardware state when the game's entry point is reached |
| `--pcs` | sample where the CPU spends its time at the end of the run |

`DWRC_GITHUB_API=<url>` points update checks at another GitHub API endpoint (for testing against a local server).

Logging uses `RUST_LOG`, e.g. `RUST_LOG=dwrc_psx::cdrom=trace` traces every CD-ROM command, response and register access; `dwrc_psx::bus=trace` traces DMA; `dwrc_psx::sio=trace` controller traffic.

Tests: `cargo test --workspace` (includes the recompiler's differential fuzz test, which needs no game data).
