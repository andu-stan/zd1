# Warp0 compute runtime

Our code on the DM870's first MIPS core (Warp0, 160 MHz, `docs/module/warp.md` sections 3 and 6), driven from our
Linux through `/dev/mem`: the receivers' decoders. Four firmware images, one job loop:

| Image | What | Where it runs |
|---|---|---|
| crypto (`w0fw.bin`) | a whole AirPlay packet (`W0_OP_RTP_AUDIO`): decrypted (AES-128-CBC for AirPlay 1, ChaCha20-Poly1305 for AirPlay 2 realtime) and ALAC-decoded, 16- or 24-bit; buffered AirPlay 2's ALAC frames come plain | all in local RAM (bss to 0x7BF0 of 0xB000) |
| AAC (`w0aac_*.bin`) | the AAC-LC decoder (module/codecs/aac): buffered AirPlay 2 | code from SDRAM (I-cache), working set in local RAM |
| Vorbis (`w0vorbis_*.bin`) | the Vorbis decoder (module/codecs/vorbis): Spotify Connect | code and setup in SDRAM, working set in local RAM |
| FLAC (`w0flac.bin`) | the FLAC decoder (module/codecs/flac): Snapcast | all in local RAM (14 KB of code and data, an 18 KB channel buffer); the frames read in place from SDRAM |

| | |
|---|---|
| `w0abi.h` | the interface: the window, the control block, the job descriptor, the ops, the result codes; `rust/build.rs` turns it into Rust constants, so the two sides can't drift |
| `fw/rt.c`, `fw/rt.h` | the runtime every image runs: the job loop, the pointer checks, the progress markers |
| `fw/main.c`, `alac.c`, `poly.S` | the crypto image (`main.c`: ChaCha20, AES-128 decryption with T-tables built at start, the RTP op; `poly.S`: Poly1305 by hand) |
| `fw/aac_main.c`, `fw/vorbis_main.c`, `fw/flac_main.c` | the decoder images: their ops around module/codecs/aac, module/codecs/vorbis and module/codecs/flac |
| `fw/start.S`, `w0.ld`, `w0aac.ld`, `w0vorbis.ld` | one entry for all images (cache mode and the two .bss ranges from the linker script); the layouts |
| `fw/inval.S` | the I-cache invalidate stub |
| `rust/` | crate `zd1-warp0`, the ARM side: the client with the images built in (`warp.rs`: boot, park, buffers, the ring, the hang diagnosis and log, over `dm870::DevMem`; the debug block's sequences are `dm870::warp`'s), one in-order job pipeline (`queue.rs`) and its users: realtime ALAC, buffered AAC and buffered ALAC for zd1-airplay (`rtp.rs`, `aac.rs`, `alac.rs`), Vorbis for zd1-spotify (`vorbis.rs`), FLAC for zd1-snapcast (`flac.rs`) |
| `tools/emu.py` | the images under Unicorn on the host, with only Warp0's real memory mapped: the host check |
| `mips.py` | the MIPS I toolchain (the flake's LLVM, and the passes that make clang's output safe on the Warp: below), also `tools/zeppelin/warp/warprun.py`'s |
| `build.py` | the images with the flake's LLVM (`nix develop`; output `emu/img/warp0/`, or `nix build .#warp0-fw`, which the receivers' build embeds) |
| `vectors/` | ALAC test vectors in shairplay-rust's golden-vector format (ALACGV01, the PCM as the oracle): 352-sample 16-bit ones from Apple's encoder as the `alac-encoder` crate (0.3) ports it, 24-bit ones from `afconvert` (`afalac24.py`) |

```
python3 module/warp0/build.py              # the images (in nix develop) -> emu/img/warp0
python3 -I module/warp0/tools/emu.py checks | vorbis FILE.pkts [N] | aac FILE [N] |
    flac FILE.chunks REF.raw [N] | alac FILE.alac [N] [plain|aes|chacha]
```
On the unit the receivers are the check: each falls back to the ARM's copy of its decoder, so a stream decoded with
Warp0 and with it off (`--no-warp0`) can be compared sample for sample (`ZD1_PCM_HASH`, `docs/streaming.md`).

## How it works
- **Local RAM** (48 KB at Warp address 0, ARM 0xAC000000) runs code and serves loads and stores at one cycle each
  (section 3); SDRAM costs ~19 cycles a load. The crypto image lives there entirely; the decoders keep their working
  set and the tables they read in full there, and run their code from SDRAM through the 4 KB I-cache. The stack is
  the top 4 KB.
- **Window** (SDRAM): 0x61F00000-0x61FBFFFF, outside Linux (`mem=31M`), below the Warp1 window: the invalidate stub
  at 0x61F0F000, the control block 0x61F10000, a ring of 32 job descriptors at 0x61F10100, buffers from 0x61F20000
  up to the running image's `C_DATA_END` (the decoder images' SDRAM parts sit above it). The ARM maps it uncached
  (`O_SYNC`); the Warp has no data cache, so neither side flushes anything.
- **Boot** (`Warp0::open(image)`): power gate 2 on if needed; halt; run the invalidate stub (the I-cache survives a Warp
  reset); halt; copy the image; start it at its entry; wait for the control block's magic and check `C_IMAGE`.
  Each step that fails says which, and the core is parked again.
- **Jobs**: the ARM fills a descriptor and sets it SUBMIT, in ring order; the Warp runs it and sets DONE with a
  result. While it runs, `C_JOB` says which job and `C_STAGE` how far it got (the decoders mark their stages, the
  Vorbis decoder down to floor / residue / IMDCT per channel). Between polls the idle Warp spins ~5 us in registers.
- **Pointer checks**: Warp0 has no exceptions and no memory protection: an unaligned store silently drops the low
  address bits, an access outside its memory hangs the bus. So every op checks every pointer it will touch, with its
  length, before touching anything: word aligned (`W0_EALIGN`) and inside the buffer area (`W0_ERANGE`), lengths
  that would wrap included; `C_REFUSED` counts refusals. The decoders check every index that depends on the stream.
- **Waiting**: the kernel ticks at 100 Hz, so a sleeping wait costs 10 ms and `sched_yield` ~21 us; the stream users
  keep several jobs in flight and collect them later. One process at a time (`/tmp/zd1-warp0.lock`).

## When it stops answering
A job not done in its time (`W0_HANG_MS`, 250 ms for every stream op; a packet takes 2-7 ms) is a hang, and it is written down,
not just survived:
- **Diagnosis** (`Warp0::hang`): the control block twice, 20 ms apart (is it moving at all: beat, done
  count, stage), then a halt to read the PC through debug mode, named by the image's symbols (`function+0xoffset`,
  from the `.sym` file build.py writes). No debug mode means the core is stuck on a bus access: no PC, but `C_JOB`
  and `C_STAGE` still say what it was doing.
- **Record**: one line in `/var/log/zd1-warp0.log` (what, image, state, job, stage, PC or "no debug mode"), a count
  for this boot in `/tmp/zd1-warp0.hangs`. A boot that fails is counted the same way.
- **Replay**: the Vorbis user writes the stream's headers and its last packets to `/tmp/zd1-warp0-hang.pkts`, which
  `tools/emu.py vorbis` replays on the host, where an access that would hang the bus stops with its PC.
- **After**: the core is parked; the stream goes on, on the ARM, without a gap (the Vorbis user primes the ARM's
  decoder with the packet before the first one Warp0 didn't finish). After `ZD1_WARP0_MAX_HANGS` (2) hangs in one
  boot nothing uses Warp0 until the next one: after several bus hangs in one power cycle Warp0 has hung on correct
  code (`docs/module/warp.md` section 7).
- **Testing it** without a real hang: `ZD1_WARP0_HANG_MS=4` makes a healthy Warp0 "hang" on its next busy packet.
  [HW 2026-10-25, `zd1-spotify --play-file`]: `image VORB v2 busy, job slot 3 op 15, stage 0x50 (moving), ...; pc
  61f67bdc = vorbis_decode+0x1a2c` (busy, not stuck: the stage and done count moving), logged and counted, the
  replay file written (it replays in `emu.py`); the ARM took over 7 packets in flight and the stream's output was
  still the host decoder's, sample for sample. (Remove `/tmp/zd1-warp0.hangs` after such a test: it counts.)

## Results on the unit
All [HW], our OS, Warp0 at 160 MHz.
- **Ciphers** (inside `W0_OP_RTP_AUDIO`: AES-128-CBC decryption, ChaCha20-Poly1305 open): RFC 8439, FIPS-197 and
  SP 800-38A vectors, a rejected bad tag, and thousands of random jobs against reference ciphers on the ARM, also
  under an ARM memory hog [2026-10-24/25]. On the host, `emu.py alac FILE N aes|chacha` encrypts the vectors as the
  senders do and decodes them on the image (Python's `cryptography` as the reference).
- **Pointer checks**: every pointer of a job below the buffer area or unaligned: refused and counted [2026-10-25].
  Pointers beyond the area, null, into local RAM, and lengths that wrap: in the emulator only (`emu.py checks`: every
  op of every image, all refused, no stray access), since on the unit a missing check there would be a bus hang.
- **Speed**: from local RAM the ciphers run at about the ARM926's speed (ChaCha20-Poly1305 open 2.6 MB/s, AES-128-CBC
  2.0 MB/s, mbedtls on the ARM 3.8 / 2.1), far above a stream's ~0.2 MB/s, and a job costs ~8 us of overhead: what
  pays is the whole packet on Warp0 (decrypted and decoded in one job), not a cipher alone [2026-10-24].
- **The hand-off is cheap if the ARM copies in bursts**: `memcpy` through the uncached window (LDM/STM, 8 words a
  bus burst) writes it at ~80-130 MB/s and reads it at ~57-88 MB/s; one volatile word at a time managed ~9 MB/s. In
  and out costs the ARM ~0.03 us per byte. **`recv()` writes a datagram straight into the
  window** [2026-10-24].
- **Realtime packets** (`W0_OP_RTP_AUDIO`: AES-128-CBC for AirPlay 1, ChaCha20-Poly1305 for AirPlay 2 realtime, then
  ALAC, `fw/alac.c`, a port of shairplay-rust's decoder): the vectors (`vectors/`) both ways encrypted against the
  ARM's decoder, a tampered tag refused; ~2.0-2.2 ms of Warp0 per
  352-sample stereo packet (~25-27 % of its time), ~0.7-0.9 ms mono. Through `zd1-airplay`, AirPlay 1 and AirPlay 2
  realtime from `airplay2-rs` came out sample for sample the source WAV (ALAC is lossless) [2026-10-25].
- **24-bit ALAC** (buffered AirPlay 2: the op on plain frames). The decoder writes its output a word at a time
  straight into the job's buffer (16-bit samples two to a word, 24-bit ones a word each, sign-extended), and reads a
  24-bit frame's shifted-off low bytes again at the output instead of holding them, so 24-bit costs no local RAM
  (the image's .bss ends 2 KB lower than with the 16-bit decoder and its PCM buffer). Vectors from Apple's own
  encoder (`vectors/afalac24.py`, afconvert: 352-sample ALAC/48000/S24, the low byte shifted off, escape frames, a
  partial last frame): `fw/alac_test.c` on the host and `emu.py alac` on the built image identical to the PCM;
  on the unit bit-identical to the PCM, 2.4 ms of Warp0 per stereo packet (33 % of its time at 48 kHz), 1.2 ms
  mono; 16-bit unchanged (2.1 ms). Through `zd1-airplay` (`ap2send --alac48` sending the vector's frames, 20 s) the
  stream hashed as its source PCM, the ARM's share ~15 % against ~37 % decoding there [2026-10-30].
- **AAC**: bit-identical to the ARM build on 1,293 frames (the IMDCT alone and the spectra after each decoding stage
  too), 7.0 ms per stereo frame (30 % of Warp0); through `zd1-airplay`, the buffered
  stream from `ap2send` hashes the same with Warp0 and without [2026-10-25].
- **Vorbis**: bit-identical to the ARM build on every packet of 320 and 160 kb/s stereo and a mono stream (1,965 /
  1,839 / 515 packets), 36.9 / 32.5 / 14.8 % of Warp0's time at real time; setup (codebooks into the SDRAM arena)
  70-80 ms on Warp0. Through `zd1-spotify --play-file` the output is the host decoder's, sample for sample, with
  Warp0 [2026-10-25].
- **FLAC**: chunks of two frames each (`flaccheck chunks`): bit-identical to the ARM build on
  every chunk of three streams; Warp0's time at real time 16.2 % for libFLAC level 2 at 44.1 kHz stereo 16-bit (what
  snapserver sends: fixed predictors, 1152-sample blocks; 8.4 ms per 52 ms chunk), 29.9 % for LPC order 32 with
  4096-sample blocks, 21.6 % for 48 kHz 24-bit [2026-10-29]. A chunk over `W0_FLAC_MAX_IN` (32 KB: two 4096-sample
  frames of 24-bit noise; one 4608-sample frame of raw 24-bit stereo is 27.7 KB) is refused (`W0_ELEN`) and decoded
  on the ARM. In the emulator the image matches libFLAC on six streams and refuses every bad pointer (`checks`).
- **Hangs**: none in the runs above. Earlier, in one power cycle that had seen several bus hangs (an unaligned word
  store while developing the AAC image), the core stopped answering mid-job, at different points in different
  firmware; after a module power cycle none came back [2026-10-24]. Cause of the first one not found; the
  diagnosis above is there so the next one says more.

## Building the images: what the Warp can't run
build.py compiles C with clang 21 through `mips.py` (`-march=mips1`) and patches the assembly: a nop after every load (the core has
no load interlock), two nops around HI/LO hazards, no delay-slot filling at all (`-disable-mips-delay-filler`: a load
clang moves into a branch's delay slot is not protected at the branch target), and then checks that no load sits in
a delay slot anyway. It rejects `div`/`divu` (no divider: they leave HI/LO zero), `mul`, `movz`/`movn`, `lwl`/`lwr`/
`swl`/`swr` (MIPS II/32 or not working), and calls to libc or to compiler helpers (`memcpy`, `memset`, `__udivsi3`,
64-bit shifts and divisions). C that needs any of these gets rewritten (`dsp/fixed.h` has soft division, the
decoders zero memory by words).

## Next
- ALAC faster on Warp0: 32-bit predictor sums (Apple's decoder uses them), a word-wise bit reader.
- Vorbis faster on Warp0: a profile (the residue loop's Huffman tables are in SDRAM).
- Inputs into local RAM by SuluDMA; ChaCha20 by hand; Spotify's AES-CTR on Warp0 (2 % of the ARM now: an op of
  its own, beside the AES-128 decryption the crypto image has).
