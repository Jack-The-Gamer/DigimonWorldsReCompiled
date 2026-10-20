# Architecture

Re:Compiled has one goal above all others: **the games behave exactly as they did on a PlayStation.** Every design choice follows from that.

## Why recompilation (and not a rewrite)

Gen1Recomp, the project that inspired this one, re-implements Pokémon Red/Blue's engine by hand and reads data from the player's ROM. That works beautifully for a small 2D game. For three large 3D games, a hand-written engine would take years per game before it matched the originals frame for frame. Re:Compiled instead keeps the games' *own code* as the source of truth:

* the game code is read from the player's disc and translated to native code on their machine;
* it runs against a precise model of the PlayStation hardware;
* behaviour is checked against a reference interpreter.

Gen1Recomp's *product* ideas are kept: one launcher, hash-verified player-supplied images, a generated per-user cache, portable mode, and a first-class mod platform: a Mods page with `.zip` import, mod indexes on GitHub (ours built in), update checks and version rollback from each mod's GitHub Releases, and an in-game manager. Hand-written Rust replacements of individual game functions can be layered on top later (see *Native overrides* below), which is the path toward a full decompilation-style port without ever giving up exactness.

## Crates

| Crate | Role |
|---|---|
| `dwrc-disc` | BIN/CUE parsing, raw 2352-byte sector access, ISO9660, PS-X EXE parsing, game identification (serial + SHA-1), sector overlay used by mods. |
| `dwrc-mips` | R3000A instruction decoding, control-flow classification, register usage, disassembly. Shared by everything that looks at code. |
| `dwrc-psx` | The console: bus, CPU + COP0 + reference interpreter, GTE, GPU, SPU, CD-ROM, MDEC, DMA, timers, interrupt controller, SIO (pads, memory cards). |
| `dwrc-recomp` | Static recompiler (Cranelift), block cache, invalidation, validation. |
| `dwrc-mods` | Mod manifests, discovery and load planning (games, dependencies, conflicts), `.zip` install/uninstall/pack, virtual-disc file replacement, memory patches, sandboxed Lua scripting. |
| `dwrc-modhub` | Mod indexes (GitHub-hosted feeds), update checks against mods' GitHub Releases, downloads; caching and offline fallback. |
| `dwrc-runtime` | Paths/portable mode, library import + verification, BIOS identification, settings, play sessions. |
| `dwrc` | Launcher and player (egui/eframe, cpal audio, gilrs gamepads). |
| `dwrc-cli` | Developer tools (inspect discs, run headless, screenshots, validation, disassembly). |

## The hardware model (`dwrc-psx`)

### Time

Everything is measured in CPU cycles (33.8688 MHz). The bus keeps a global cycle counter and a small scheduler: each device reports the cycle of its next interesting event (end of a scanline, a timer reaching its target, a CD-ROM response or sector, a controller byte, an SPU sample batch). The CPU engine runs until the earliest event, the bus services it, and so on. Devices are also synchronised lazily whenever the CPU touches their registers, so register reads always see an up-to-date device.

Video timing uses the real video clocks (53.693175 MHz NTSC, 53.203425 MHz PAL), 3413/3406 GPU cycles per line and 263/314 lines per frame, which drives VBLANK, the timers' hblank/dot-clock sources and the interlace field.

### CPU

`cpu.rs` is the reference interpreter. It models branch delay slots, load delay slots (including LWL/LWR merging with an in-flight load, and cancellation when the delay-slot instruction writes the same register), all exceptions, COP0, the 4 KiB instruction cache (contents and timing, cache isolation used by the BIOS to flush it), memory access timing derived from the memory-control registers, multiply/divide latency and GTE command latency. When an interrupt arrives while a GTE command is next, the command executes first, as on hardware (the BIOS handler expects this).

The interpreter is generic over a `CpuBus` trait. The real bus implements it; the recompiler's validator implements it with a replay bus.

### GTE

Bit-exact implementation of all 22 commands with 44-bit MAC overflow detection, all saturation flags, the UNR reciprocal table and division used for perspective projection, and the documented quirks (RTP IR3 flag computed from `MAC3 >> 12`, MVMVA with the far-colour vector, the "garbage" matrix, sign-extended reads of H).

### GPU

GP0/GP1 processing, 1 MiB VRAM, CPU↔VRAM and VRAM↔VRAM transfers, fill, and a software rasteriser. Polygons use Mednafen's hardware-verified scheme: 32.32 fixed-point edge stepping with its sub-pixel bias, attribute gradients from the triangle's "core" vertex, top-left fill rule, quads split into two triangles. Also: gouraud shading, texture mapping (4/8/15-bit, CLUTs, texture windows), dithering, all four semi-transparency modes, mask bit set/check, drawing area/offset, rectangles with flipping, lines and polylines, 480i field skipping. Display output honours the display area, horizontal/vertical ranges and 15/24-bit colour.

### CD-ROM

The controller's command set with response FIFO/interrupt queueing and timing (acknowledge delay, seek time by distance, 75/150 sectors per second), sector buffering and the data FIFO (2048 or 2340-byte modes), XA-ADPCM real-time audio with file/channel filtering, the CD audio volume matrix, GetID/region strings, TOC queries. XA audio is decoded and resampled to 44.1 kHz using the hardware's 6→7 phase structure.

### SPU

24 voices with 4-bit ADPCM decoding, loop flags and ENDX, 4-point interpolation, ADSR envelopes and volume sweeps (the nocash envelope algorithm), noise, pitch modulation, the reverb unit (the full nocash algorithm with its 39-tap resampling FIR), capture buffers, sound RAM transfer (manual, DMA read/write), SPU IRQ, and CD audio mixing.

### MDEC, DMA, timers, SIO

MDEC run-length/quantisation decoding, IDCT using the game-supplied scale table, YUV→RGB in 4/8/15/24-bit output. DMA channels 0–6 in all sync modes including linked lists and OTC, DICR interrupt logic. Root counters with all clock sources, sync modes and IRQ modes. SIO0 with the digital pad protocol and the memory-card protocol (read/write/ID, flags, checksums), with ACK timing and IRQ7.

## The recompiler (`dwrc-recomp`)

### Translation

A *block* is a run of instructions ending after a branch and its delay slot, after an instruction that always traps (SYSCALL/BREAK/reserved), after MTC0/RFE (so interrupts are re-checked), or at a size limit. Each block becomes one native function `fn(*mut JitCtx) -> next_pc`.

Inside a block, guest registers are SSA variables (loaded on entry, written back at every exit). Load delay slots are resolved at translation time with exactly the interpreter's rules; a load still in flight at the end of a block is handed back to the CPU state so the next block observes it correctly. Branch conditions and jump targets are evaluated before the delay slot executes; link registers are written by the branch itself. Overflow and address errors leave the block through the same exception entry code the interpreter uses.

RAM and scratchpad loads/stores have inline fast paths. Everything else goes through `extern "C"` helpers that call the *same* `Bus`/`Cpu` code as the interpreter: I/O, BIOS ROM, stores while the cache is isolated, and stores into pages that hold translated code. That shared code is what keeps the two engines in agreement.

### Static discovery ("compile-ahead")

When execution first reaches an address without a translation, the recompiler translates that block and every block statically reachable from it through direct branches, calls and return sites, in one pass (bounded per pass). In practice the boot executable and each overlay are translated as soon as they start running.

### Timing parity

Before a translated block runs, the dispatcher charges its instruction fetches through the interpreter's instruction-cache model, and memory, multiply/divide and GTE timing come from the same helpers. Interrupts are checked between blocks. As a result recompiled and interpreted runs stay in lock-step: identical frames over minutes of play, with total instruction counts within 0.03%.

### Self-modifying code and overlays

Every 4 KiB RAM page holding translated code is flagged on the bus. Stores and DMA into flagged pages record an invalidation range; the engine drops exactly the blocks overlapping it and retranslates on next execution. This is how overlays loaded from disc (DW1's `*_REL.BIN`, DW2/DW2003's raw-LBA overlays) are handled.

### Validation

`EngineKind::Validate` (Settings → CPU engine, or `dwrc-cli run --engine validate`) records every memory access a translated block makes, then replays the block through the reference interpreter from the starting CPU state against that log. Registers, HI/LO, PC, the pending load, COP0, the GTE state, the sequence of reads and the exact writes must all match. A mismatch is reported with a disassembly and the block falls back to the interpreter. The `differential` test in `crates/dwrc-recomp/tests` does the same with random programs and needs no game data, so it runs in CI.

### Save states

`Psx::save_state` writes the whole console to bytes and `Psx::load_state` reads it back (`crates/dwrc-psx/src/state.rs`). Each device lists its fields once, in a `sync` function that either writes or reads them, so the two directions cannot drift apart. Left out on purpose: the BIOS ROM and the disc (a state only loads with the release and BIOS it was saved with; `dwrc-runtime::savestate` checks), code hooks (they belong to the tools and mods running now), memory card contents (loading a state never takes a player's saves back) and output already handed to the host (audio, debug text). A failed load leaves the console as it was. After a load the execution engine forgets its translations (`ExecEngine::flush`). The format carries a version, bumped whenever a device's fields change. A test in `crates/dwrc-runtime/tests/save_state.rs` checks that the frames after a load are bit-identical to the frames after the save, on both engines.

Mods drive them: `dwrc.save_state` / `dwrc.load_state` queue a request that the session carries out between frames, writing `states/<serial>/slot<N>.state` (deflate-compressed, with a JSON header naming the release and BIOS).

### Native overrides (planned)

The hook mechanism (`Bus::add_hook`) already lets Rust code or Lua scripts take over at any guest address and return to the caller with `ctx:ret()`. The intended path toward a decompilation-style port is to replace game functions one at a time with hand-written Rust, each verified against the original by running both and comparing results, exactly like the recompiler's validator.

## The runtime and launcher

`Library` stores where the player's discs and BIOS are, their identification and the full-image SHA-1 (checked once at import; later launches, and later scans of the same folder, only check size and mtime). A game can have several discs (other regions, other copies of one release); one of them is chosen on the Games page and is the one that plays. A disc is known by its image file, so a `.cue` and the `.bin` it names are one disc, and a `.cue` whose `.bin` was renamed falls back to the `.bin` with the cue's own name. A `Session` builds the virtual disc (with mod overlays), the console, memory cards and the chosen engine, picks the BIOS (the player's dump for the disc's region, otherwise the built-in OpenBIOS from `assets/openbios/`), and applies fast boot if enabled (console BIOS only; OpenBIOS is built to boot the disc directly). Fast boot side-loads the executable when the BIOS reaches its shell entry point, after the kernel has initialised, and leaves the CD-ROM controller as the shell would.

The player runs the session on its own thread, paced by the console's own frame timing (NTSC ≈59.8 Hz, PAL ≈49.8 Hz). Audio goes through a ring buffer with gentle drift correction to the device rate. The UI shows the latest frame scaled to 4:3 (or integer/stretch).
