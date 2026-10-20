# Digimon Worlds Re:Compiled

**Digimon World, Digimon World 2 and Digimon World 3 / 2003, running natively on Windows, Linux and macOS — exactly as they ran on the original PlayStation — from one launcher, with mod support.**

Re:Compiled translates the games' own MIPS code (read from *your* discs) into native machine code and runs it against a cycle-timed model of the PlayStation hardware written in Rust. Nothing from the games is included in this repository or in its releases: you supply your own disc images, Re:Compiled verifies them, and everything else is generated on your machine. A BIOS dump from your console is optional; without one, the built-in open-source [OpenBIOS](assets/openbios/README.md) starts the games.

| | |
|---|---|
| Games | Digimon World (USA), Digimon World 2 (USA), Digimon World 2003 (Europe); also recognised, not yet verified: Digimon World (Europe, Japan), Digimon World 2 (Japan), Digimon World 3 (USA, Japan) |
| Platforms | Windows 10/11, Linux (X11/Wayland), macOS 11+ (Intel and Apple Silicon) |
| Language | Rust (plus Lua 5.4 for mod scripts; the optional built-in BIOS is OpenBIOS, MIT-licensed C) |
| You provide | Your disc images (the `.bin`; a `.cue` is optional). A BIOS dump is optional |

---

## Playing

1. **Get the program.** Download a release zip for your OS from [Releases](https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled/releases), or build it yourself (see [Building](#building)). On Windows you can also double-click `Play-Windows.bat` in a source checkout: it builds whatever changed (if Rust is installed) and starts the program.
2. **Point it at your discs.** On first launch, click **Scan a folder for discs…** and pick the folder holding your `.bin` images (`.cue` files are used if present but not needed). Each disc is identified and checked against known-good dumps (SHA-1). BIOS files in the folder are picked up too. You can also import discs individually, add a BIOS in **Settings → BIOS**, or drop either on the window.

   If you have more than one disc for a game (another region, or another copy), all of them are kept: click the release name under the game's title (for example *Digimon World (USA) · SLUS-01032*) to unroll the list and pick the one that plays.
3. **Press Play.**

Your discs are never copied or modified. Saves (memory cards), settings, screenshots and generated data live in your user data folder:

| OS | Location |
|---|---|
| Windows | `%APPDATA%\DigimonWorldsReCompiled\data` |
| Linux | `~/.local/share/digimonworldsrecompiled` |
| macOS | `~/Library/Application Support/DigimonWorldsReCompiled` |

**Portable mode:** put an empty file named `portable.txt` next to the program and everything is kept in a `data` folder beside it instead.

**Video:** **Settings → Video** offers sharp original pixels, smoothed scaling, or a **CRT** filter that draws the picture like a CRT television (scanlines, phosphor mask and screen curvature, each adjustable), plus 4:3, integer-scaled or stretched output. **Digimon World 2003 (Europe)** plays at **60 Hz** by default: the European release has a 60 Hz mode of its own, and Re:Compiled switches it on, so it plays at the speed of the USA release with the same music tempo. Untick **Digimon World 2003 at 60 Hz** there for the original 50 Hz, which plays about 17% slower.

**Updates:** when a new version is released, an **Update** button appears at the bottom of the Games page. It shows what's new, downloads the new version and replaces the program where it is; your games, saves, settings and mods are kept, and a restart finishes it. **Settings → About** can turn the check off. (A copy built from source updates by pulling and rebuilding instead.)

**Community:** [Discord](https://discord.gg/zyUF5mUt84) · [Ko-fi](https://ko-fi.com/jackthegamer7) (also on the Games page).

### Controls

| PlayStation | Keyboard (default) | Gamepad |
|---|---|---|
| D-pad | Arrow keys | D-pad / left stick |
| ✕ / ○ / □ / △ | X / C / Z / S | South / East / West / North |
| Start / Select | Enter / Backspace | Start / Back |
| L1 / L2 / R1 / R2 | A / Q / D / E | Bumpers / triggers |

Keys can be rebound in **Settings → Controls**. With the **Save States** mod (in the built-in index), **1** to **5** save the game to five slots and **Shift + 1** to **5** load them. **F11** switches fullscreen on and off anywhere in the launcher. While playing: **Esc** pause menu · **F10** mods · **F12** screenshot · hold **Tab** to fast-forward.

Memory cards are standard 128 KiB raw images (`card1.mcd`, `card2.mcd` in `saves/<game>/`), interchangeable with other emulators.

### Supported dumps

These images are verified end-to-end (full-image SHA-1):

| Game | Serial | File | SHA-1 |
|---|---|---|---|
| Digimon World (USA) | SLUS-01032 | `Digimon World (USA).bin` | `5611645da66183e30d11fb6c11a2784ff47e1fdb` |
| Digimon World 2 (USA) | SLUS-01193 | `Digimon World 2 (USA).bin` | `d9cb009bc0006e901d8ab1365f764ab71da02115` |
| Digimon World 2003 (Europe) | SLES-03936 | `Digimon World 2003 (Europe) (En,Fr,De,Es,It).bin` | `457cb233349ba841e03b33d8060f8fbcadd45cb3` |

These releases are also recognised and run, but no dump of them is on the verified list yet, so they show an "unverified" note:

| Game | Serial |
|---|---|
| Digimon World (Europe) | SLES-02914 |
| Digimon World (Japan) | SLPS-01797 |
| Digimon World 2 (Japan) | SLPS-02844 |
| Digimon World 3 (USA) | SLUS-01436 |
| Digimon World 3: Aratanaru Bouken no Tobira (Japan) | SLPS-03446 |

Other dumps of the verified releases are accepted with the same note. A `.cue` file is optional; one that names a `.bin` that has since been renamed is worked around by using the `.bin` with the cue's own name.

### BIOS (optional)

Without a BIOS dump, games start with the built-in **OpenBIOS** (open source, MIT, by the PCSX-Redux authors). All three games boot, play and save with it, and a memory card it writes is byte-for-byte the same as one written through Sony's BIOS, so saves move freely between the two. The games' own code runs identically either way; adding a BIOS dumped from your console also makes the BIOS's own routines (boot sequence, kernel calls) exact, and enables **fast boot**. BIOS images verified: SCPH-5501 (USA, v3.0), SCPH-5502/5552 (Europe, v3.0) and SCPH-5500 (Japan, v3.0); other genuine BIOS dumps are accepted for their region. Each game uses the BIOS of its disc's region. **Settings → BIOS** lists the BIOS files you have added (add or remove them there) and chooses between yours and the built-in one.

## Mods

The built-in mod index, with the mods made for the project, lives in its own repository: [Jack-The-Gamer/DigimonWorldsReCompiled-mod-index](https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled-mod-index).


The launcher has a **Mods** page next to **Games** and **Settings**:

* **Installed** — every mod in one shared load order (drag to reorder), switched on per game (DW1 / DW2 / DW3), with options, update checks, a version picker (update or roll back), delete and a safe mode.
* **Find mods** — browse mod indexes hosted on GitHub and install with one click. The [Re:Compiled index](https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled-mod-index) is built in; add others with **Indexes…** (`owner/repo` or a link).
* **Import .zip…** — install a mod you downloaded, or just drop the `.zip` on the window.

Mods can replace files on the (virtual) disc, patch memory, and run Lua scripts with hooks on any code address, every frame and on controller input, with their own options and save data. Scripts run in a sandbox with no file, network or OS access. Authors publish versions as GitHub Releases; the launcher checks for mod updates when it starts and puts an **Update** button on each mod that has one. **F10** in game switches the running game's mods on and off.

Re:Compiled ships only a documented **template**; mods themselves come from mod indexes. The built-in one has, for example [**Turbo**](https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled-mod-index/tree/HEAD/mods/Jack-The-Gamer@turbo) (hold R2, then any one or two buttons to turbo them) and [**Fast-Forward**](https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled-mod-index/tree/HEAD/mods/Jack-The-Gamer@fast-forward) (L2 + R2 for double or triple speed, with the music left at normal speed). Mod authors publish their mods through a mod index of their own, which players add with **Indexes…**. See **[docs/modding.md](docs/modding.md)** for using, writing and publishing mods.

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
* **`dwrc-mods`** implements the mod platform and **`dwrc-modhub`** the mod indexes, mod updates and the program's own updates; **`dwrc-runtime`** the library, import/verification, settings and sessions; **`dwrc`** the launcher/player UI; **`dwrc-cli`** developer tools.

Details: **[docs/architecture.md](docs/architecture.md)** · accuracy notes: **[docs/accuracy.md](docs/accuracy.md)** · tools: **[docs/dev-tools.md](docs/dev-tools.md)**.

### Verification so far

* All three games boot through the real BIOS (or fast boot), play their FMVs with XA audio, and reach gameplay.
* The recompiler validates with **zero mismatches** against the interpreter (≈224,000 checked block executions over ≈30,000 distinct blocks across the three games), and recompiled runs produce **pixel-identical** frames to the interpreter over minutes of scripted play.
* The other releases (Digimon World Europe and Japan, Digimon World 2 Japan, Digimon World 3 USA and Japan) boot through their region's console BIOS and through OpenBIOS with zero validation mismatches, to their intro FMV or, for Digimon World 3, the title screen and load menu; they have not been played further yet.
* A disc-free differential fuzz test (`cargo test -p dwrc-recomp`) compares random programs on both engines in CI.

## Building

Requirements: [Rust](https://rustup.rs) 1.85+ (stable).

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
mods/                 the documented mod template
assets/openbios/      the built-in BIOS (OpenBIOS, MIT) and how it is built
docs/                 documentation
```

## Legal

Digimon is a trademark of Bandai Namco / Toei Animation. PlayStation is a trademark of Sony Interactive Entertainment. This project is not affiliated with or endorsed by either. It contains no game data and no Sony BIOS; use it only with discs (and, optionally, a BIOS) you own.

Re:Compiled's own code is dual-licensed under MIT or Apache-2.0, at your option.
