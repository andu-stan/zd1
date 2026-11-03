# Warp and Sol probes

The probes behind `docs/module/warp.md` (sections 3, 5, 7 and 8) and their raw results on the unit. The questions
they answer are settled; the scripts stay so that a result can be repeated. They run from a Mac over the KnOS shell
or our OS's `zd1-memsh` (the debug system) through `tools/zeppelin/warp/warprun.py`, RAM and registers only.

To repeat one: `python3 -I re/netmodule/sol-probes/solprobe.py HOST NAME OUT` (`--list` for the names), then compare
`OUT/NAME.json` with the file of that name below. HOST is `169.254.1.1:23` for `zd1-memsh` on the unit, or
`127.0.0.1:4557` in the emulator (`zemu run --fwd 4557:23 --keep`: the dev boot is the debug system).

| Script | What |
|---|---|
| `solprobe.py` | Sol: the probe sets (first to third pass) and the fourth and fifth pass experiments (`p4-*`, `p5-*`), one registry |
| `warpprobe.py` | Warp0's cache, SDRAM contention, the SuluDMA block moves, both cores' clock |
| `w0probe.py` | a Warp core's loads, stores, multiplier, I-cache and bus timing (`--core 1` for Warp1) |

| Results | What |
|---|---|
| `hw-2026-10-22/`, `hw-2026-10-22b/` | first and second pass, under KnOS (only what warp.md still cites) |
| `hw-2026-10-23/` | third pass under our OS (set names), fourth (`p4-*`) and fifth (`p5-*`), `warpprobe.py` runs |
| `hw-2026-10-24/`, `hw-2026-10-25/` | `w0probe.py` output; `hangprobe-*`: deliberate bus hangs and the park that recovers them |
| `fuzz-*.json` | the Sol fuzzer's ALU traces |
