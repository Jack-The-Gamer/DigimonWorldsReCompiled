# Digimon Worlds Re:Compiled

**Digimon World, Digimon World 2 and Digimon World 3 / 2003, running natively on Windows, Linux and macOS — exactly as they ran on the original PlayStation — from one launcher, with mod support.**

Re:Compiled translates the games' own MIPS code (read from *your* discs) into native machine code and runs it against a cycle-timed model of the PlayStation hardware written in Rust. Nothing from the games is included in this repository or in its releases: you supply your own disc images and console BIOS, Re:Compiled verifies them, and everything else is generated on your machine.

| | |
|---|---|
| Games | Digimon World (USA), Digimon World 2 (USA), Digimon World 2003 (Europe), Digimon World 3 (USA, unverified) |
| Platforms | Windows 10/11, Linux (X11/Wayland), macOS 11+ (Intel and Apple Silicon) |
| Language | 100% Rust (plus Lua 5.4 for mod scripts) |
| You provide | Disc images (`.cue` + `.bin`) and a PlayStation BIOS dump for the disc's region |

---

## Playing

1. **Get the program.** Download a release zip for your OS from [Releases](https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled/releases), or build it yourself (see [Building](#building)). On Windows you can also double-click `Play-Windows.bat` in a source checkout: it builds whatever changed (if Rust is installed) and starts the program.
2. **Point it at your files.** On first launch, click **Scan a folder for discs & BIOS…** and pick the folder holding your `.cue`/`.bin` images and BIOS file. Each disc is identified and checked against known-good dumps (SHA-1). You can also import discs and BIOS files individually.
3. **Press Play.**

Your discs are never copied or modified. Saves (memory cards), settings, screenshots and generated data live in your user data folder:

| OS | Location |
|---|---|
| Windows | `%APPDATA%\DigimonWorldsReCompiled\data` |
| Linux | `~/.local/share/digimonworldsrecompiled` |
| macOS | `~/Library/Application Support/DigimonWorldsReCompiled` |

**Portable mode:** put an empty file named `portable.txt` next to the program and everything is kept in a `data` folder beside it instead.

### Controls

| PlayStation | Keyboard (default) | Gamepad |
|---|---|---|
| D-pad | Arrow keys | D-pad / left stick |
| ✕ / ○ / □ / △ | X / C / Z / S | South / East / West / North |
| Start / Select | Enter / Backspace | Start / Back |
| L1 / R1 / L2 / R2 | Q / E / A / D | Bumpers / triggers |

Keys can be rebound in **Settings → Controls**. While playing: **Esc** pause menu · **F10** mods · **F11** fullscreen · **F12** screenshot · hold **Tab** to fast-forward.

Memory cards are standard 128 KiB raw images (`card1.mcd`, `card2.mcd` in `saves/<game>/`), interchangeable with other emulators.

### Supported dumps

These images are verified end-to-end (full-image SHA-1):

| Game | Serial | File | SHA-1 |
|---|---|---|---|
| Digimon World (USA) | SLUS-01032 | `Digimon World (USA).bin` | `5611645da66183e30d11fb6c11a2784ff47e1fdb` |
| Digimon World 2 (USA) | SLUS-01193 | `Digimon World 2 (USA).bin` | `d9cb009bc0006e901d8ab1365f764ab71da02115` |
| Digimon World 2003 (Europe) | SLES-03936 | `Digimon World 2003 (Europe) (En,Fr,De,Es,It).bin` | `457cb233349ba841e03b33d8060f8fbcadd45cb3` |

Other dumps of these releases are accepted with an "unverified" note. BIOS images verified: SCPH-5501 (USA, v3.0) and SCPH-5502/5552 (Europe, v3.0); other genuine BIOS dumps are accepted for their region.

## Mods

Mods work the way they do in Gen1Recomp. The launcher has a **Mods** page next to **Games** and **Settings**:

* **Installed** — every mod in one shared load order, switched on per game (DW1 / DW2 / DW3), with options, update checks, a version picker (update or roll back), uninstall and a safe mode.
* **Find mods** — browse mod indexes hosted on GitHub and install with one click. The [Re:Compiled index](https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled-mod-index) is built in; add others with **Indexes…** (`owner/repo` or a link).
* **Import .zip…** — install a mod you downloaded, or just drop the `.zip` on the window.

Mods can replace files on the (virtual) disc, patch memory, and run Lua scripts with hooks on any code address, every frame and on controller input, with their own options and save data. Scripts run in a sandbox with no file, network or OS access. Authors publish versions as GitHub Releases, and players are offered updates automatically. **F10** in game switches the running game's mods on and off.

Bundled examples: **Turbo Text** (hold R2 to race through dialogue), **Play Time Tracker**, and a documented **template**. See **[docs/modding.md](docs/modding.md)** for using, writing and publishing mods.

## How it works

```
 your disc ──► dwrc-disc ──► identification, ISO9660, virtual disc (mod overlays)
                                   │
 your BIOS ─────────────────┐      ▼
                            ▼  dwrc-psx: the PlayStation
                     ┌──────────────────────────────────────────────┐
  dwrc-recomp ──────►│ CPU (R3000A)  GTE  GPU  SPU  CD-ROM  MDEC    │
  MIPS → native code │ DMA  timers  IRQs  pads  memory cards        │
  (Cranelift)        └──────────────────────────────────────────────┘
                                   │ frames, audio, saves
                                   ▼
                     dwrc (launcher + player)  ◄── dwrc-mods (Lua, patches, files)
```

* **`dwrc-psx`** models the console: the R3000A (with a reference interpreter that handles branch/load delay slots, exceptions, the instruction cache and multiply/divide/GTE stalls), the GTE, the GPU (software rasteriser following Mednafen's hardware-verified rules), the SPU (24 ADPCM voices, ADSR, reverb), the CD-ROM controller (command timing, XA-ADPCM streaming), MDEC (FMV decoding), DMA, timers, controllers and memory cards.
* **`dwrc-recomp`** is the static recompiler. When execution reaches a code region, every block statically reachable from it is translated to native code with Cranelift and run against the same hardware model. Overlays loaded later are translated when reached, and translations are dropped automatically when the game overwrites their memory. A **validation mode** replays every translated block through the reference interpreter and compares registers, memory traffic and COP0/GTE state.
* **`dwrc-mods`** implements the mod platform and **`dwrc-modhub`** the mod indexes and updates; **`dwrc-runtime`** the library, import/verification, settings and sessions; **`dwrc`** the launcher/player UI; **`dwrc-cli`** developer tools.

Details: **[docs/architecture.md](docs/architecture.md)** · accuracy notes: **[docs/accuracy.md](docs/accuracy.md)** · tools: **[docs/dev-tools.md](docs/dev-tools.md)**.

### Verification so far

* All three games boot through the real BIOS (or fast boot), play their FMVs with XA audio, and reach gameplay.
* The recompiler validates with **zero mismatches** against the interpreter (≈224,000 checked block executions over ≈30,000 distinct blocks across the three games), and recompiled runs produce **pixel-identical** frames to the interpreter over minutes of scripted play.
* A disc-free differential fuzz test (`cargo test -p dwrc-recomp`) compares random programs on both engines in CI.

## Building

Requirements: [Rust](https://rustup.rs) 1.84+ (stable).

* **Windows:** install Rust with `rustup-init.exe` (it offers to install the Visual Studio C++ build tools), then double-click `Play-Windows.bat` or run `cargo build --release`.
* **Linux:** `sudo apt install build-essential pkg-config libasound2-dev libudev-dev libgtk-3-dev libxkbcommon-dev` (or your distribution's equivalents), then `./Play-Linux.sh` or `cargo build --release`.
* **macOS:** install the Xcode command line tools (`xcode-select --install`), then double-click `Play-Mac.command` or run `cargo build --release`.

The program is `target/release/DigimonWorldsReCompiled(.exe)`. The CI workflow in `.github/workflows/build.yml` builds and tests all three platforms and attaches ready-to-run zips to tagged releases. More in **[docs/building.md](docs/building.md)**.

## Project layout

```
crates/dwrc-disc      disc images, ISO9660, PS-X EXE, game identification
crates/dwrc-mips      R3000A instruction decoding / disassembly
crates/dwrc-psx       the PlayStation hardware model + reference interpreter
crates/dwrc-recomp    static recompiler (Cranelift) + validation
crates/dwrc-mods      mod platform (manifests, .zip install, virtual disc files, patches, Lua)
crates/dwrc-modhub    mod indexes, GitHub release updates, downloads
crates/dwrc-runtime   library, import, settings, sessions
apps/dwrc             the launcher / player (egui)
apps/dwrc-cli         developer command-line tools
mods/                 bundled example mods
mod-index/            the mod index (its own repository: Jack-The-Gamer/DigimonWorldsReCompiled-mod-index)
docs/                 documentation
```

## Legal

Digimon is a trademark of Bandai Namco / Toei Animation. PlayStation is a trademark of Sony Interactive Entertainment. This project is not affiliated with or endorsed by either. It contains no game or BIOS data; use it only with discs and a BIOS you own.

Re:Compiled's own code is dual-licensed under MIT or Apache-2.0, at your option.
