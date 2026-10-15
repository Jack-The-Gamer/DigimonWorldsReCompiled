# Accuracy notes

"Exactly as on the original PlayStation" is the bar. This page records what is verified, what is modelled from documentation, and what is still approximated, so nothing is overstated.

## Verified against the games

* All three supported releases boot through the real BIOS (or fast boot), play their FMVs (MDEC + XA audio) and reach gameplay with scripted input; memory cards are detected, written and read.
* The recompiler matches the reference interpreter: zero mismatches across ≈224,000 validated block executions (≈30,000 distinct blocks), and pixel-identical frames between engines over minutes of play.
* A full BIOS boot needed one fix: on a DICR write, interrupt flags of channels whose enable bit is written as 0 are cleared (Mednafen behaviour). Without it, DMA completion flags left by the BIOS shell raise a spurious interrupt in Digimon World's FMV streaming code.

## Modelled from hardware documentation

The SPU's 4-point interpolation uses the console's exact 512-entry table; a unit test checks the table's defining property (every 4-tap set sums to 0x7F80 ±1).

These follow the nocash PSX specification and Mednafen's hardware-tested behaviour, and match it as far as tested here: CPU pipeline effects, COP0 and exceptions, instruction cache, memory timing, GTE (bit-exact), GPU rasterisation rules, DMA, timers, MDEC decoding, SPU envelopes/ADPCM/reverb, CD-ROM command behaviour, controller and memory-card protocols.

## Known approximations (to be replaced)

| Area | Current state | Effect |
|---|---|---|
| XA-ADPCM resampling coefficients | Same 7-phase × 29-tap structure as the hardware's "zigzag" tables, windowed-sinc coefficients | Very slightly different high-frequency response in streamed music and FMV audio; inaudible in practice, but not bit-exact. |
| MDEC IDCT rounding | nocash formulation with 16-bit-per-pass rounding | Occasional ±1 differences in FMV pixel values. |
| GPU drawing time | Drawing completes instantly (no GPU busy time) | Games are never GPU-bound here, so scenes that slowed down on hardware because of fill rate may run slightly smoother. |
| CD-ROM seek time | Distance-based model (Mednafen formula) | Loading pauses are close to hardware but not identical. |
| Interrupt latency (recompiled engine) | Interrupts are taken between translated blocks (at most one block — up to 96 instructions — later than hardware) | No observable effect in testing (frames identical to the interpreter). |
| Controllers | Digital pad (SCPH-1080) | Correct for DW1/DW2/DW2003 (they need no analog input). DualShock vibration is not emulated yet. |

## How to check accuracy yourself

* Run any game with **Settings → CPU engine → Validate** to check every translated block against the interpreter while you play (slow).
* `dwrc-cli run <cue> --bios <bin> --engine interp|recomp --frames N --shots dir --every K` produces comparable screenshots from either engine.
