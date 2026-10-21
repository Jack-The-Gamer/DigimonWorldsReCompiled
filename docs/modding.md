# Modding guide

Mods are handled the way Gen1Recomp handles them: one **Mods** page in the
launcher (next to Games and Settings), mods installed from `.zip` files or
from **mod indexes** on GitHub, and updates read from each mod's own GitHub
Releases.

## Using mods

* **Installed** lists every mod in one shared **load order** (top loads first;
  when two mods change the same thing, the lower one wins). Each mod has a
  switch per game (**DW1 / DW2 / DW3**) for the games it supports. Click
  anywhere on a mod's card for its details and options (click it again to
  close them). **Reset options** above a mod's options puts them back to
  their defaults. At the bottom of an open card, **Version History** lists
  the mod's releases (update, or go back to an older one), **Open Folder**
  shows its files, and **Delete** removes it after asking; its saved data
  and option values are kept for if you install it again. To change the
  load order, drag a card up or down the list (a line shows where it will
  land; the list scrolls when you drag to its top or bottom edge), or use
  **Up** and **Down**.
* **Find mods** browses the mod indexes: the built-in
  [Re:Compiled index](https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled-mod-index)
  plus any you add with **Indexes…** (paste `owner/repo`, the repository's
  GitHub link, or a feed URL). Filter by game and category, sort by name,
  downloads, trending or last update, open **Details** for the full
  description, and **Install**. Listings are refreshed every time the
  launcher starts (and every six hours while it stays open); **Refresh**
  forces it. Offline, the last copy is shown.
* **Import .zip…** (or drop a `.zip` on the window) installs a mod you
  downloaded. Installing switches the mod on for every game it supports.
* **Updates:** mods that name their GitHub repository are checked every time
  the launcher starts, and every six hours while it stays open (turn this off
  under **Options**). Mods from one repository, such as those hosted in an
  index, are checked with a single request. The **Mods** tab shows how many
  mods have an update, and each of those mods gets an **Update to vX**
  button (hover it for what's new). **Update all** installs every update;
  **Version History** installs any release, including older ones (roll back).
* **Options → Safe mode** starts games with no mods at all, without losing
  your choices — useful when tracking down a problem.
* In game, **F10** shows the running game's mods: switch them on or off and
  change options; changes apply when the game restarts.

Mods are installed into `mods/<id>/` in your data folder (**Open mods
folder**); one copy serves every game it supports. Mods shipped with the
program live in the `mods/` folder next to it, and an installed mod with the
same `id` overrides a shipped one. Older per-game folders (`mods/dw1/…`,
`mods/any/…`) are still read.

### Safety

Mod scripts run in a sandbox: only Lua's `string`, `table`, `math`, `utf8`
and `coroutine` libraries plus `os.time`/`os.clock`/`os.difftime`, no file,
network or OS access, no binary chunks, and `require` only loads `.lua` files
from the mod's own folder. A mod can change the emulated game and its own
saved data, nothing else. Archives are checked before anything is copied (one
mod per `.zip`, no paths that escape the mod folder, size limits), and an
install that fails leaves the previous version untouched.

## `mod.toml`

```toml
[mod]
id = "my-mod"                # letters, digits, '-' and '_'; names the install folder
name = "My Mod"
version = "1.0.0"            # semver
author = "you"
description = "What it does."
game = "dw1"                 # dw1 | dw2 | dw3 | any — or games = ["dw1", "dw2"]
serials = ["SLUS-01032"]     # optional: only these releases (see Finding addresses)
script = "main.lua"          # optional
files_dir = "files"          # optional, default "files"
github = "you/my-mod"        # optional: Releases carry updates (see Publishing)
homepage = "https://…"       # optional
categories = ["QOL"]         # 1-4 of GAMEPLAY CONTENT BALANCE ART AUDIO UI QOL
                             # TRANSLATION TOTAL_CONVERSION LIBRARY TOOL OTHER
tags = ["text", "speed"]     # optional
license = "MIT"              # optional
engine = ">=1.0"             # optional: Re:Compiled versions it works with
dependencies = ["core-lib@>=1.2"]   # optional: mods that must also be on
conflicts = ["other-mod"]           # optional: mods it cannot run with
experimental = false         # optional: marks unfinished work

[[option]]                   # shown in the mod manager
id = "multiplier"
name = "EXP multiplier"
type = "int"                 # bool | int | float | choice
default = 2
min = 1
max = 10
description = "optional help text"

[[patch]]                    # memory patch
address = 0x80012345
bytes = "00 00 00 00"        # or u8 / u16 / u32 = value
when = "always"              # re-applied every frame, or "once"
if_address = 0x80012340      # optional condition: only while the bytes
if_bytes = "27 BD FF E8"     # here match (target a specific overlay)

[[lba]]                      # raw sector replacement
lba = 123456
file = "data/thing.bin"
max_sectors = 40             # refuse if the data would overflow this
```

When a game starts, the switched-on mods load in load order. A mod is skipped
(and the Mods page says why) when it has errors, is not for this game or
release, needs a newer Re:Compiled (`engine`), lacks a dependency that is
installed and switched on, or conflicts with a mod earlier in the order.
Dependencies are loaded before the mods that need them.

## Publishing a mod

Players get mods from **mod indexes**: GitHub repositories that publish a
list of mods, which the launcher reads in **Find mods**. The
[Re:Compiled index](https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled-mod-index)
is built in and lists the mods made for the project; it does not take
submissions. To share your mods, publish them through **an index of your
own**, which players add in **Find mods → Indexes…** with its `owner/repo`.
Your mods are then listed next to the built-in ones, with one-click installs,
update checks and version history.

### How an index works

An index is a repository laid out like the Re:Compiled index, with one folder
per mod under `mods/`. On every push to `main`, and nightly, its **Build and
publish** workflow checks the mods with `dwrc-cli mod validate`, releases any
new versions, builds the feed (`data/index.json`) with the index's build tool,
and publishes it to the repository's `gh-pages` branch. The launcher reads the
feed from GitHub Pages (`https://<owner>.github.io/<repo>/data/index.json`).
Installing a listed mod downloads its release `.zip` and installs it exactly
as **Import .zip…** does, and updates come from the GitHub Releases of the
repository named by `github` in the mod's `mod.toml`.

### Setting up your own index

1. Create a public GitHub repository (say `you/my-dwrc-mods`) and push a copy
   of the [Re:Compiled index](https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled-mod-index)
   to its `main` branch, without the folders inside `mods/` (those are the
   Re:Compiled mods).
2. Add your mods to `mods/`, one folder each (below), and push. The workflow
   runs under **Actions** and creates the `gh-pages` branch.
3. Turn on GitHub Pages: **Settings → Pages**, *Deploy from a branch*,
   `gh-pages`, `/ (root)`.
4. Tell players to add `you/my-dwrc-mods` in **Find mods → Indexes…**.

The index's README, "Your own mod index", has the details.

### Adding a mod to your index

A mod can live in the index itself or in a repository of its own; the
launcher treats both the same.

**Hosted in the index.** Put the mod in `mods/<You>@<id>/mod/` (`mod.toml`
at its top, with `github = "you/my-dwrc-mods"`, the index's repository), plus
`description.md` and optionally `thumbnail.png`. The listing's details come
from `mod.toml`, so there is no `meta.json` to keep in step. To release a
version, bump `version` in `mod.toml` and push: the index checks the mod,
publishes the release `<id>-v<version>` with `<id>-<version>.zip` (its notes
are the message of the commit that changed the mod) and lists it. The
Re:Compiled index publishes its own mods, such as
[Turbo](https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled-mod-index/tree/HEAD/mods/Jack-The-Gamer@turbo),
this way.

**In the mod's own repository.**

1. Put the mod in a GitHub repository with `mod.toml` at its root and set
   `github = "you/your-mod"`.
2. For each version, bump `version` and publish a **GitHub Release** tagged
   `v<version>` with **`<id>-<version>.zip`** attached (an `<id>/` folder with
   the mod inside). `dwrc-cli mod pack <folder>` builds that file, and
   [`templates/release-mod.yml`](templates/release-mod.yml) is a GitHub
   Actions workflow that does all of it for you: bump `version` in
   `mod.toml`, push (GitHub Desktop is fine), and the release appears.
3. List it in your index with a folder `mods/<You>@<id>/` holding
   `meta.json` (copy the index's `examples/` entry) and `description.md`.
   The index follows the mod's releases nightly, so new versions need no
   change to the index.

Players who installed your mod are offered each new release in the launcher.
Release tags may also be `<id>-v1.2.0` when one repository holds several mods.
Assets are chosen as `<id>-<version>.zip`, then a `.zip` starting with the id,
then the first `.zip`; the newest stable release is preferred over a newer
pre-release. `dwrc-cli mod validate <folder>` checks a mod before you publish.

A mod doesn't have to be in an index at all: a `.zip` from
`dwrc-cli mod pack` installs with **Import .zip…**, and if its `mod.toml` names
its GitHub repository, update checks work the same.

## Replacing files

### Digimon World (ISO filesystem)

Everything in `files/` replaces the file with the same path on the disc, e.g. `files/ETCNA/TITLE256.TIM` replaces `\ETCNA\TITLE256.TIM`. Files can be smaller or **larger** than the originals: a larger file is placed after the end of the disc. Re:Compiled then updates the ISO directory record, and also patches the game's own file table. Digimon World keeps a `(LBA, size, name)` table inside `SLUS_010.32` and reads files from it directly, so every matching `(LBA, size)` pair in the boot executable is rewritten.

### Digimon World 2 and 2003 (raw LBAs)

These discs contain only the executable and a `DUMMY` file in the filesystem; all game data is read by sector number. Use `[[lba]]` entries to overwrite data in place, with `max_sectors` as a safety check. (Relocating larger data needs the game's LBA table, which can be patched with `[[patch]]` or a script once its address is known.)

The player's image is never modified; replacements live in a virtual disc built when the game starts.

## Lua scripts

Scripts run in an isolated, sandboxed Lua 5.4 state per mod (see *Safety*). The `dwrc` table:

| | |
|---|---|
| `dwrc.game` | `"dw1"`, `"dw2"` or `"dw3"` |
| `dwrc.serial` | the disc's serial, e.g. `"SLUS-01032"` |
| `dwrc.version` | Re:Compiled version |
| `dwrc.options` | this mod's option values, by id |
| `dwrc.data` | a table saved per mod (in `mod-data/<game>/<mod>/data.json`) when the game closes or on `dwrc.save()` |
| `dwrc.buttons` | bit masks: `dwrc.buttons.Cross`, `.Start`, `.Up`, … |
| `dwrc.log(text)` | write to the log (shown in the F10 mod log); `print` does the same |
| `dwrc.on_frame(fn(ctx))` | after every video frame |
| `dwrc.on_input(fn(buttons) -> buttons)` | before every frame; return a modified controller state |
| `dwrc.on_exec(addr, fn(ctx) -> bool)` | when the CPU reaches `addr`; return `true` if you changed the PC. May also be called later (from `on_frame`, say, once the code is in memory): the hook goes in before the next frame |
| `dwrc.save()` | write `dwrc.data` now |
| `dwrc.set_speed(n)`, `dwrc.speed()` | run the console `n` times faster than real time (1 to 4; 1 is normal). The launcher paces itself to match and still plays sound in real time |
| `dwrc.set_realtime_voices(mask)` | sound chip voices (bit 0 = voice 0 … bit 23) that keep real-time pitch and timing while sped up; the others play faster like the rest of the game |
| `dwrc.on_key(fn(ctx, key, mods))` | a key pressed on the keyboard while the game runs: `key` is its name (`"1"`, `"A"`, `"F5"`, `"Space"`…, the key's place on the keyboard, so Shift + 1 is `"1"` with `mods.shift`), `mods` has `shift`, `ctrl` and `alt`. Not called for Esc, Tab and F10 to F12, which the launcher uses, or while the game is paused (1.3+) |
| `dwrc.save_state(slot)`, `dwrc.load_state(slot)` | save the whole console to slot 1 to 99, or load it, once the current frame is done. Slots belong to the release (kept in the data folder's `states/<serial>/`), only load with the BIOS they were saved with, and never include memory cards. The player sees "Saved state 1", "Slot 3 is empty" and the like (1.3+) |
| `dwrc.notify(text)` | show a short message over the game picture (1.3+) |
| `dwrc.show_image(name, file [, opts])` | show a PNG from the mod's folder over the game picture until `dwrc.hide_image(name)`; showing the same `name` again moves or replaces it. `opts`: `corner` (`"top_left"`, the default, `"top_right"`, `"bottom_left"` or `"bottom_right"`), `x` and `y` (distance from that corner's edges, default 4) and `scale` (default 1). Positions and sizes are in pixels of a 320×240 grid laid over the picture, whatever resolution the game uses, so an image keeps its place and size; a 64×64 image at `scale = 0.375` covers 24×24 of the grid. Images up to 256×256 and 1 MiB, 16 per mod (1.4+) |
| `dwrc.hide_image(name)` | take that image away again (1.4+) |

The `ctx` passed to callbacks:

| | |
|---|---|
| `ctx:read8/16/32(addr)`, `ctx:read_s8/s16/s32(addr)` | read RAM / scratchpad / BIOS |
| `ctx:write8/16/32(addr, v)` | write RAM / scratchpad |
| `ctx:read_bytes(addr, n)`, `ctx:write_bytes(addr, str)` | bulk access |
| `ctx:reg(n or "a0")`, `ctx:set_reg(n, v)` | CPU registers |
| `ctx:pc()`, `ctx:set_pc(addr)` | program counter |
| `ctx:ret(value)` | return from the current function (at its first instruction) with `v0 = value` |
| `ctx:frame()` | frame counter (in `on_frame`) |
| `ctx:buttons()`, `ctx:pressed("Cross")` | controller state |
| `ctx:find(pattern [, from, to])` | the first word-aligned address in RAM (from `0x80000000` to `0x80200000` by default) where `pattern` occurs, or `nil`. The pattern is 32-bit words in hex with `?` for any digit: `"27bdffc8 3c02???? 8c42????"`. Handy for finding code that sits at different addresses in each game or release |

Writes that hit translated code automatically invalidate it, so scripts can patch code safely.

### Example: replacing a function

```lua
-- Make the function at 0x80045678 always return 1 (illustrative address).
dwrc.on_exec(0x80045678, function(ctx)
  return ctx:ret(1)
end)
```

### Example: changing the controller (a simple turbo)

While the turbo button is held, Cross is pressed for two frames and released
for two. The [Turbo mod](https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled-mod-index/tree/HEAD/mods/Jack-The-Gamer@turbo) in the index is the full version: hold R2,
then any one or two buttons turbo for as long as they are held.

```lua
local hold, tap = dwrc.buttons[dwrc.options.hold], dwrc.buttons.Cross
local n = 0
dwrc.on_input(function(b)
  if (b & hold) ~= 0 then
    n = n + 1
    b = b & ~hold
    if n % 4 < 2 then b = b | tap else b = b & ~tap end
  else
    n = 0
  end
  return b
end)
```

### Example: save states

The [Save States mod](https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled-mod-index/tree/HEAD/mods/Jack-The-Gamer@save-states)
is this, give or take its comments: keys 1 to 5 save, Shift + 1 to 5 load.

```lua
dwrc.on_key(function(ctx, key, mods)
  local slot = tonumber(key)
  if not slot or slot < 1 or slot > 5 or mods.ctrl or mods.alt then return end
  if mods.shift then dwrc.load_state(slot) else dwrc.save_state(slot) end
end)
```

### Example: a switch with a sign on screen

The [DW3 Disable Random Encounters mod](https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled-mod-index/tree/HEAD/mods/Jack-The-Gamer@dw3-disable-random-encounters)
switches random battles off and on with the R key, and shows
`encounters-off.png` (a file in the mod's folder) in the top right corner
while they are off:

```lua
local off = false
dwrc.on_key(function(ctx, key, mods)
  if key ~= "R" or mods.ctrl or mods.alt or mods.shift then return end
  off = not off
  if off then
    dwrc.show_image("off", "encounters-off.png", { corner = "top_right", x = 6, y = 6, scale = 0.375 })
  else
    dwrc.hide_image("off")
  end
end)
```

### Example: running faster

`dwrc.set_speed` makes the whole console run faster. Sound would speed up
with it, so a mod that wants the music left alone also has to slow the game's
music driver back down and tell the sound chip which voices play music. The
[Fast-Forward mod](https://github.com/Jack-The-Gamer/DigimonWorldsReCompiled-mod-index/tree/HEAD/mods/Jack-The-Gamer@fast-forward)
does exactly that for all three games: it finds Sony's sound library with
`ctx:find`, lets the sequencer play on one tick in two (or three), and marks
the music's voices with `dwrc.set_realtime_voices`. The bare speed change is
one line:

```lua
dwrc.on_input(function(b)
  if (b & dwrc.buttons.L2) ~= 0 then dwrc.set_speed(2) else dwrc.set_speed(1) end
  return b
end)
```

## Finding addresses

The developer CLI (`dwrc-cli`, see [dev-tools.md](dev-tools.md)) can extract files from discs, disassemble the executable, run games headlessly with scripted input and dump VRAM/screenshots. Combined with a script that logs memory from `on_frame`, that is enough to locate game variables. Addresses differ between releases, so use `serials` to restrict mods to the release you studied, or find code by pattern with `ctx:find` and check it before patching. One game can be several releases, and players can switch between them on the Games page:

| `game` | Releases (`dwrc.serial`) |
|---|---|
| `dw1` | `SLUS-01032` (USA), `SLES-02914` (Europe), `SLPS-01797` (Japan) |
| `dw2` | `SLUS-01193` (USA), `SLPS-02844` (Japan) |
| `dw3` | `SLES-03936` (Digimon World 2003, Europe), `SLPS-03446` (Japan), `SLUS-01436` (USA) |

## Tips

* Use `if_address`/`if_bytes` on patches that touch overlay code, so they only apply while that overlay is loaded.
* Keep mods small and focused. Players can combine them.
* Never redistribute game data. Ship patches, scripts or tools that *transform* the player's own files instead.
