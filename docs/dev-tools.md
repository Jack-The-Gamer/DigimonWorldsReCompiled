# Developer tools (`dwrc-cli`)

Build with `cargo build --release -p dwrc-cli`; the tool is `target/release/dwrc-cli`.

| Command | |
|---|---|
| `dwrc-cli inspect <cue>` | Tracks, ISO volume, identification, every file with its LBA and size, `SYSTEM.CNF`, the boot executable header. |
| `dwrc-cli extract <cue> <path> <out>` | Extract a file from the disc's filesystem. |
| `dwrc-cli disasm <exe> <hex-addr> <count>` | Disassemble a PS-X EXE. |
| `dwrc-cli import <data-root> <folder>` | Scan a folder into the library under `<data-root>` (same code as the launcher; every disc found is listed, unchanged images are not hashed again). |
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
| `--engine interp\|recomp\|validate` | CPU engine (default interp). Two more for working on the recompiler: `recomp-inline` translates each block before it first runs (as check mode does), `recomp-off` never translates (every block on the interpreter, with translated timing); both end in exactly the same state as `recomp` |
| `--shots DIR --every K` | save a PNG every K frames (with the images mods show), plus `vram.png` at the end |
| `--wav FILE` | record audio |
| `--input "frame:button:len,..."` | scripted input, e.g. `1000:start:10,1200:x:6` (buttons: start select x o t s up down left right l1 r1 l2 r2) |
| `--mod DIR` | enable a mod folder (repeatable) |
| `--tty` | echo the BIOS/game debug console |
| `--card1 FILE.mcd` / `--card2` | use (and afterwards save, also after `--serve`) memory card images |
| `--pal60` | run Digimon World 2003 (Europe) in its own 60 Hz mode, as the launcher does by default |
| `--widescreen 16:9\|16:10\|21:9` | widescreen side strips (pictures from `--shots` and `shot` come out wider, e.g. 426×240); add `--widescreen-ui` for Widescreen UI |
| `--states DIR` | where save states go (made by mods or the `--serve` commands); default: a folder in the temp directory |
| `--trace-bios` | list the BIOS functions the game calls (table, number, count, first frame), also after `--serve`: run the same commands with and without the action you are curious about (saving, say) and compare |
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
| `shot FILE.png` | save the current picture (with the images mods show, `dwrc.show_image`) |
| `ram FILE` | write main RAM |
| `vram FILE` | write video RAM (1024×512 16-bit pixels, little-endian, row by row) |
| `peek ADDR [N]` | print N bytes of memory in hex |
| `poke8` / `poke16` / `poke32 ADDR VALUE` | write memory |
| `key [shift+\|ctrl+\|alt+]KEY` | a key press for mods (`dwrc.on_key`), e.g. `key shift+1` |
| `savestate N` / `loadstate N` | save the console to slot N, or load it |
| `widescreen off\|16:9\|16:10\|21:9 [ui]` | switch the widescreen side strips, with `ui` Widescreen UI (it stays across `loadstate`; the picture fills from the next frames drawn) |
| `gpulog N FILE` | run N frames and write every primitive drawn: frame, DMA packet address, GP0 command, x0 x1 y0 y1 in VRAM, drawing area, CLUT<<16\|texture page, widescreen move (`-`: none); copies as command `80` with the source in the texture field; a `display X Y` line per frame. Watch a packet address to find the code that builds it; group by texture page and CLUT row to tell a game's UI from its scenery |
| `pcs N [LO HI]` | run N frames and list the 40 addresses where the CPU spent most time (sampled every 57 cycles), optionally only between `LO` and `HI`: a quick way to find the code behind what's on screen |
| `prof N [MS] [BTN[+BTN…]]` | run N frames (holding the buttons) and time each on the host: the average, the worst, and every frame slower than `MS` (default 12), with the time spent translating code on the emulation thread in it (only with `recomp-inline` or `validate`; `recomp` translates in the background) |
| `cd` | what the CD-ROM drive is doing: position, mode (double speed, XA-ADPCM, filter), idle/seeking/reading/playing, the XA filter and how many XA-ADPCM samples it has decoded (to tell streamed music from sequenced music) |
| `wav FILE N [BTN[+BTN…]]` | run N frames (holding the buttons) and save their audio as a WAV, e.g. to compare music at normal speed and fast-forward |
| `watch ADDR` … `unwatch` | while on, the console runs one instruction at a time and notes every change of the 32-bit word at `ADDR`, with the address of the instruction that made it; `unwatch` prints them (`frame N: old -> new at PC`). Slow: use it around the moment you are after, with `--engine interp` for exact addresses. The quickest way from a value in RAM to the code that keeps it |
| `frame` | print the frame number |
| `quit` | stop |

Feed it from a named pipe to drive it from a script, e.g.
`mkfifo in; dwrc-cli run game.cue --fast --engine recomp --card1 my.mcd --serve < in > out.log &`.
Any long-lived pipe works: a small script that keeps one `--serve` process
running and passes it commands from a socket lets you send a few commands,
look at the screenshots, decide, and send more, without starting over.

## Researching a game

How the DW3 Auction Notifications mod's facts were found, as a recipe for
finding your own. The aim is always the same: a value in RAM (a flag, a
counter), the code that reads or writes it, and a pattern for that code that
holds no addresses, so the mod also works on the other releases.

**Get there once, then use save states.** Play to the place with `--serve`
(`press`, `wait`, `shot`), and `savestate N` before and after everything
interesting: before a boss, after it, at a door. Later experiments start with
`loadstate N`, a second away from the moment you care about. Save states from
the launcher's Save States mod (`states/<serial>/slotN.state` in its data
folder) load in `dwrc-cli` too (`--states` pointing at a copy of that folder),
so a player can play to a place and hand you the states. Copy memory cards
before `--card1`: the card is written back when the run ends.

**Diff RAM across a story beat.** `ram FILE` before and after the event (beat
a boss, watch a scene, finish an auction), then compare. Most of the noise
goes away if you look only at the part of RAM the game saves: find where the
save's bytes sit in RAM by searching a RAM dump for 32-byte pieces of the
save on the memory card (frame 1's directory gives each file's first block,
8 KiB each). In Digimon World 2003 the save image is 0x80048D34 to
0x8004B434; between two states only a dozen of its bytes change, and the
play time, BIT and the encounter countdown are easy to recognise.

**Poke to test a candidate.** Load a state from just before the event, put
one candidate byte back to its old value (`poke8`), play the event again, and
see whether it still happens. Five candidates for "an auction is on" took
five short runs; only the story stage word stopped it.

**Find the code that writes a value.** `watch ADDR` with `--engine interp`
prints each change and the instruction that made it. Watching is slow, so
save a state a second or two before the change: replay the inputs to find
the second in which it happens (a loop of `wait`, `press`, `peek`), save a
state there, then watch only that second. The writer is often a small shared
function (Digimon World 2003's "set bit n of this byte array", its "add one
of item n"), and its callers tell you which array is which.

**Read the code.** `disasm` takes a PS-X EXE, so put a 2 KiB header in front
of a `ram` dump (`PS-X EXE` at 0, load address 0x80000000 at 0x18, the size
at 0x1C) and disassemble any address, overlays included. To list the
callers of a function, search the dump for its `jal` word (`0x0C000000 |
(addr >> 2)`); for the users of a variable, search for loads and stores with
its low 16 bits as the offset and a `lui` of the high half a few words
before.

**Log what the game checks, with a mod.** A mod of a few lines can hook a
game function with `dwrc.on_exec` and `dwrc.log` its arguments: run with
`RUST_LOG=info` and the log shows every call. Hooking Digimon World 2003's
condition test (`test(id, sense)`) and walking into the auction room listed
the conditions the room checks; that led to the room's table of all sixteen
auctions. Log each distinct value once, and switch the logging on and off
with a key (`dwrc.on_key`, `key L` in `--serve`) to keep the log short. Only
one hook per address is kept: if two mods (or a mod and your probe) hook the
same address, the later one replaces the earlier.

**Find the code behind a screen.** `pcs N [LO HI]` lists where the CPU runs.
Compare two screens (the field and an area title, say) to find code that
runs on one and not the other; a hook on the start of such a function tells
a mod which screen is up.

**Line pictures up with the game's numbers.** To draw at a place in the
game's picture, find where the game keeps its scroll (diff `ram` dumps while
scrolling), then compare screenshots with those values: locate a piece of
the picture in each shot (match a small crop pixel by pixel) and check that
the difference stays the same. Step one frame at a time (`press down 1`,
`shot`) to see the delay: Digimon World 2003's picture shows what it drew
two frames earlier, so a mod's image follows values from two frames back.
Check both video modes where a game has two (Digimon World 2003 at 60 and
50 Hz).

**Check the other releases.** Boot each disc to the title and `--dump-ram`:
the main executable is in RAM, and your patterns either match there or tell
you what differs (Digimon World 3 USA keeps some data a few bytes from where
the European and Japanese releases do, so read addresses out of the
instructions instead of writing them down). Code and data from overlays can
be searched for in the disc images themselves: a raw `.bin` has 2352-byte
sectors with 2048 bytes of data at offset 24, so search with short patterns
or join the sectors' data first. Read the dumps; never write them.

`DWRC_GITHUB_API=<url>` points update checks at another GitHub API endpoint (for testing against a local server).

Logging uses `RUST_LOG`, e.g. `RUST_LOG=dwrc_psx::cdrom=trace` traces every CD-ROM command, response and register access; `dwrc_psx::bus=trace` traces DMA; `dwrc_psx::sio=trace` controller traffic.

Tests: `cargo test --workspace` (includes the recompiler's differential fuzz test, which needs no game data).
