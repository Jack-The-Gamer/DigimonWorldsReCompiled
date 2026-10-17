# Developer tools (`dwrc-cli`)

Build with `cargo build --release -p dwrc-cli`; the tool is `target/release/dwrc-cli`.

| Command | |
|---|---|
| `dwrc-cli inspect <cue>` | Tracks, ISO volume, identification, every file with its LBA and size, `SYSTEM.CNF`, the boot executable header. |
| `dwrc-cli extract <cue> <path> <out>` | Extract a file from the disc's filesystem. |
| `dwrc-cli disasm <exe> <hex-addr> <count>` | Disassemble a PS-X EXE. |
| `dwrc-cli import <data-root> <folder>` | Scan a folder into a library (same code as the launcher). |
| `dwrc-cli run <bin or cue> [--bios <bin>] [options]` | Run a game headlessly (built-in OpenBIOS unless `--bios` is given). |
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
| `--fast` | fast boot (with `--bios` only) |
| `--engine interp\|recomp\|validate` | CPU engine (default interp) |
| `--shots DIR --every K` | save a PNG every K frames, plus `vram.png` at the end |
| `--wav FILE` | record audio |
| `--input "frame:button:len,..."` | scripted input, e.g. `1000:start:10,1200:x:6` (buttons: start select x o t s up down left right l1 r1 l2 r2) |
| `--mod DIR` | enable a mod folder (repeatable) |
| `--tty` | echo the BIOS/game debug console |
| `--card1 FILE.mcd` / `--card2` | use (and afterwards save) memory card images |
| `--trace-bios` | list the BIOS functions the game calls (table, number, count, first frame) |
| `--dump-entry` | print the hardware state when the game's entry point is reached |
| `--pcs` | sample where the CPU spends its time at the end of the run |
| `--dump-ram FILE` | write main RAM (2 MiB from `0x80000000`) at the end of the run |
| `--serve` | keep the console running under commands read from stdin (below) |

**`--serve`** is for exploring a game step by step without rebooting it each
time (to reach a place, then try things there). Each line on stdin is one
command, answered with a line starting `ok` or `err`; `--input` still applies by
frame number, and `--mod` folders run as usual.

| | |
|---|---|
| `press BTN[+BTN…] N` | hold buttons for N frames, e.g. `press left+down 30` |
| `wait N` | run N frames with nothing pressed |
| `shot FILE.png` | save the current picture |
| `ram FILE` | write main RAM |
| `peek ADDR [N]` | print N bytes of memory in hex |
| `poke8` / `poke16` / `poke32 ADDR VALUE` | write memory |
| `frame` | print the frame number |
| `quit` | stop |

Feed it from a named pipe to drive it from a script, e.g.
`mkfifo in; dwrc-cli run game.cue --fast --engine recomp --card1 my.mcd --serve < in > out.log &`.

`DWRC_GITHUB_API=<url>` points update checks at another GitHub API endpoint (for testing against a local server).

Logging uses `RUST_LOG`, e.g. `RUST_LOG=dwrc_psx::cdrom=trace` traces every CD-ROM command, response and register access; `dwrc_psx::bus=trace` traces DMA; `dwrc_psx::sio=trace` controller traffic.

Tests: `cargo test --workspace` (includes the recompiler's differential fuzz test, which needs no game data).
