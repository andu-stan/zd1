# FLAC decoder (fixed point)

Our decoder for what a Snapcast server sends with `codec=flac` (zd1-snapcast): whole FLAC frames in (the stream's
STREAMINFO given once), interleaved int32 out with each sample left-justified, as S32 audio. Written from the FLAC
format specification (RFC 9639). Integer only, no division, no 64-bit library calls, no libc and no allocation, every
read bounded by the input: the same C runs on the ARM926 and on Warp0 (`module/warp0`, the FLAC image), where an access
outside its memory hangs the bus.

| File | |
|---|---|
| `flacdec.c`, `flacdec.h` | the decoder (~450 lines): frame headers (every block-size, rate and bit-depth code, the UTF-8 frame number, CRC-8), CONSTANT / VERBATIM / FIXED (orders 0-4) / LPC (orders 1-32) subframes, wasted bits, Rice and Rice2 residuals with escaped partitions, left/side, right/side and mid/side stereo, the frame's CRC-16 |
| `rust/` | crate `zd1-flac`: the decoder for Rust with FLAC's own limits (zd1-snapcast's ARM decoder; it replaced claxon), and a libFLAC test vector (`tests/data`) |
| `check/` | `flaccheck.c`: `dump` (a .flac file decoded to raw PCM in the stream's width), `chunks` (its frames grouped like a Snapcast server's chunks, for the Warp0 checks), `escape` (frames built bit by bit with escaped partitions: libFLAC never writes them), `fuzz` (mutated frames under ASan/UBSan, the input allocated to exactly what the decoder may read), `bench` (speed on the unit); `corpus.py` (the comparison with libFLAC over a test corpus) |

Limits (FLAC_EUNSUP), per build: Warp0's (the defaults: its 48 KB of local RAM) are 2 channels and 4608-sample
blocks (the streamable subset's limit at 48 kHz and below; libFLAC's levels use 1152 and 4096); the ARM's
(`rust/`: `-DFLAC_MAX_CH=8 -DFLAC_MAX_BLOCK=65535`) are FLAC's own. Both: 24 bits at most.

## How it works
- The input is read as aligned 32-bit words (byte-swapped once: the stream is big-endian), one load per 32 bits:
  on Warp0 the input is in SDRAM, ~19 cycles a load. A read past the input gives zero bits and marks the frame
  overrun (FLAC_EOVERRUN). Unary codes count leading zeros with `dsp/fixed.h`'s `clz32` (the ARM's instruction; a five-step search
  on the Warp, which has none).
- One channel buffer (18 KB): channel 0 is decoded there and stored to the output, channel 1 decoded there and
  combined with what the output holds (the stereo modes), so a frame needs half the working memory; Warp0's local
  RAM is 48 KB. Whether a frame fits the output is known from its header, before anything is written.
- Prediction in 32-bit arithmetic where libFLAC's rule says every sum fits (bits + coefficient precision +
  log2(order) <= 32), else 32x32->64 products summed in 64 bits and shifted by hand (no `__ashrdi3`). The arithmetic
  wraps (unsigned): a valid stream never overflows, a corrupt one isn't undefined behaviour; its CRC-16 refuses it.
- FLAC keeps no state between frames: any frame can be decoded alone, which is what makes the Warp0 fallback trivial
  (`module/warp0/rust/src/flac.rs`).

## Checked [2026-10-29]
- **Against libFLAC** (`check/corpus.py`: `flac` 1.5.0 encoding 15 signals with 14 sets of options: levels 0, 1, 2,
  5, 8, block sizes 192 / 576 / 2048 / 4608, LPC order 32 (`--lax`), exhaustive search with partition orders 0-8,
  fixed subframes only, LPC only, no mid/side; signals: music-like, a full-scale sweep, white noise, silence, -60 dB,
  a clipped square, uncorrelated and identical channels, low bits always zero, mono, 48 kHz, 8 and 24 bits): all
  210 streams the same bytes as `flac -d` (lossless: no tolerance). Exercised: 10,094 CONSTANT, 13,934 VERBATIM,
  35,619 FIXED, 16,339 LPC subframes (4,661 on the 64-bit path), 5,192 with wasted bits, 4,910 Rice2 residuals,
  every stereo mode. The ARM's build (`ARM=1 corpus.py` with flaccheck built with its -D): those and 3, 6 and 8
  channels, 16384- and 65535-sample blocks: all 288 streams the same bytes.
- **Escaped partitions** (`flaccheck escape`): 6-bit, 0-bit and 18-bit raw residuals next to Rice
  partitions, fixed orders 0-4: decoded as built.
- **Robustness**: 162,000 decode calls on mutated frames (bit flips, truncation, random runs, random output room)
  under ASan/UBSan: no fault, no result out of bounds, no signed overflow (the predictors' arithmetic is unsigned).
- **On Warp0**: in the emulator (`module/warp0/tools/emu.py flac`), six streams (level 2, LPC 32, 24-bit noise at
  level 8, mono with 4608-sample blocks, wasted bits, 48 kHz 24-bit) the same samples as libFLAC, no access outside
  Warp0's memory; on the unit, bit-identical to the ARM build, numbers in `module/warp0/README.md`.
- **On the ARM926** (zd1-snapcast with `--no-warp0`, `soak`, 44.1 kHz stereo 16-bit at libFLAC level 2): the
  whole Snapcast client 15.2 % of the CPU with this decoder, 17.0 % with claxon (the Rust decoder it replaced).

## Building and running the checks
```
cc -O2 -Wall -Wextra -I. -I../dsp -o /tmp/flaccheck check/flaccheck.c flacdec.c
python3 -I check/corpus.py /tmp/flaccorpus /tmp/flaccheck $(nix build --inputs-from . nixpkgs#flac.bin --print-out-paths --no-link)/bin/flac
/tmp/flaccheck chunks FILE.flac FILE.chunks 2            # for emu.py flac
```
`check/flaccheck.c` has every command and the fuzzer's and the unit's build lines in its first lines (-DFLAC_STATS
counts the tools a corpus exercises).
