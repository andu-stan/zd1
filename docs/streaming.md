# Streaming: the receivers, what they cost, how they play

AirPlay 1 and 2, Snapcast and Spotify Connect on our module OS, against what the CR870 can do: an ARM926EJ-S at
240 MHz (no FPU, 16 KB I- and D-cache), 28 MB of RAM for Linux, two Warp MIPS cores and Sol (`module/warp.md`). The
receivers themselves: `module/os/README.md`; our changes to the libraries: `module/os/forks.md`. Numbers are [HW]
from the unit under our OS unless tagged: ARM figures from the bring-up benchmarks (mbedtls 3.6.2, libsodium, FFmpeg
8.1 and Tremor, static musl, 30 s of synthetic stereo 44.1 kHz) and `soak` runs,
Warp0 figures from `module/warp0/`.

## Summary
- **AirPlay 1 and 2 play on the unit** through `zd1-airplay` (shairplay-rust, forked for this CPU): from a Mac's
  Music app and its system audio and from an iPhone, over the wired link and Wi-Fi. Realtime ALAC is decrypted and
  decoded on Warp0, buffered AAC goes through our fixed-point decoder on Warp0: **11-28 % of the ARM per stream**,
  2-3 MB RSS. Buffered streams play on the network timeline, **within 0.6 ms of a Mac's own speakers** in a group.
- **Snapcast plays** through `zd1-snapcast` (ours): PCM or FLAC, FLAC on Warp0, the client **3.8 % of the ARM**.
- **Spotify Connect** (`zd1-spotify`, librespot 0.8 patched) is picked and logs in, but Spotify refuses the test
  account every track's audio key; not fixable here. Spotify plays on the speaker through AirPlay (the app's AirPlay
  output).
- **Everything that touches samples is integer**: float decoders cannot run on a soft-float ARM9 (Symphonia's AAC
  needs 134 %, FFmpeg's float `aac` / `vorbis` 90-100 %, a float resampler 13x real time). Our decoders
  (`module/codecs`: AAC-LC 14.8 %, Vorbis 20.4 %, FLAC) are fixed point and run on Warp0 too.
- **Decoding is the cost; bulk crypto is cheap** (at most ~9 %, AirPlay 1's AES-CBC). The expensive crypto is the
  handshake: RSA-2048 (0.36 s, AirPlay 1), SRP-6a 3072 for each transient AirPlay 2 pairing (its M3 0.52 s).
- **The kernel needed help** (`module/os/README.md`, Constraints): std's blocking waits busy-looped
  (`FUTEX_WAIT_BITSET`), tokio didn't start (`epoll_create1`), `OsRng` blocked forever (no entropy), and the
  kernel's CPU accounting under-reports this kind of load by 2-10x (measure with `soak`).

## AirPlay (zd1-airplay)
Measured with `module/os/debug/soak.c` (load = 1 - the idle spinner's rate / its idle rate); steady playback with
ALSA output, test senders on the Mac [HW 2026-10-24/25]:

| mode (sender) | load | notes |
|---|---|---|
| AirPlay 2 buffered AAC-LC 233 kb/s (`tools/zeppelin/lab/ap2send`, as an iPhone's music), decoded on Warp0 | 11 % | 17 % while the sender streams ahead; our decoder on the ARM (`--no-warp0`) 24 % |
| AirPlay 1 ALAC 848 kb/s (`airplay2-rs --airplay1`), decrypt + decode on Warp0 | 28 % | 41 % with `--no-warp0` |
| AirPlay 2 realtime ALAC (`airplay2-rs --airplay2`), decrypt + decode on Warp0 | 26 % | 44-48 % with `--no-warp0` |
| idle receiver | 0.3 % | |

With real senders `top` samples show `zd1-airplay` at 8-17 % for realtime ALAC [2026-10-24]. With Warp0 decoding, a
realtime stream into a null output still takes ~17 %: the kernel's UDP receive on Ethernet 5-9 % for 126 packets/s
(a bare `recv` loop, same rate), the receiver ~9 %. Warp0 spends ~2 ms per 352-sample stereo packet, ~25 % of its
time; its output is bit-identical to the software decoder's (`ZD1_PCM_HASH`, a million samples, both protocols).
`clock_gettime` is a system call here (4.1 us, no vDSO): ~1,900 a second during an AirPlay 1 stream (~0.8 %).
What bought what: the hot crates (ALAC, the ciphers) at `opt-level = 3` 4-6 %; ALSA writes of four periods ~2 %;
an integer sample path instead of the library's f32 (the conversion alone cost 12 %); decoding at playout instead
of on arrival (compressed frames queued: ~5x less memory at the same CPU).

Where an AAC decoder's time goes (FDK, profiled on the unit before our decoder replaced it; `shairplay-bench
fdkprof`, a SIGPROF sampler): the transform (`dit_fft`, `dct_IV`, `imlt_block`, interleave) ~30 %, Huffman and
inverse quantisation ~25 %, the 32-to-16-bit output conversion 15 %, memset/copies ~8 %; its PCM limiter (on by
default) another 19 %.

**48 kHz** streams play at 48 kHz: snd-sulu takes 32/44.1/48/96 kHz and the speaker's DSP plays a 48 kHz input at
the right pitch (a 17 kHz tone as AAC at 48 kHz and from `ap2send --alac48`: 16999.5-16999.8 Hz at the Mac's
microphone) [2026-10-28]. Nothing is resampled.

**Identity.** The speaker presents itself as certified third-party speakers do (a Sonos Amp: `model=Amp
manufacturer=Sonos serialNumber=... fv=...`, no `osvers`): `model=Zeppelin Air`, `manufacturer=Bowers & Wilkins`, a
serial number from the MAC, `fv` our OS version, the same model in GET /info and `updateInfo` (shairplay
`DeviceIdentity`). A Mac lists it as `type=AirPlay subtype=Default`, a speaker [2026-10-28]. What a non-Apple model
gives up is what Apple keeps for HomePods (Home Theater mode, Atmos, eARC, the room's default speaker). The feature
bits match a Sonos Amp's except MFi authentication (bit 26: Apple's chip; ours is FairPlay, bit 14, as
shairport-sync's) and artwork / now-playing text (bits 15, 17: nothing here shows them). The accessory's identity
(the Ed25519 seed behind the advertised `pk`) and paired controllers are kept in `/data/zd1/airplay`, so the speaker
is the same accessory across restarts; without a PIN, pairing is transient (SRP per connection), as for AirPlay
speakers without HomeKit. Its mDNS services point at `<hostname>-airplay.local`: the plain host name is claimed by
Spotify's and Snapcast's responders too.

**Formats.** GET /info offers realtime ALAC/44100/S16/2 and buffered AAC-LC/44100 and /48000, ALAC/44100/S16/2 and
ALAC/48000/S24/2 (shairport-sync's set with its lossless mode on). A buffered stream's SETUP names one format, but
each frame carries its own (the SSRC), and the sender changes it from item to item; the receiver follows it (a new
decoder from the first frame of another format).
- **An Apple TV** sets every stream up as ALAC/48000/S24 and sends lossless items as ALAC/48000/S24 (a 44.1 kHz
  16-bit ALAC file too: it resamples) and lossy ones as AAC: YouTube at 48 kHz as AAC-LC/48000, a 44.1 kHz item and
  Apple Music's radio as AAC-LC/44100 [HW 2026-10-30, Home Sharing and YouTube on an Apple TV]. With
  AAC-LC/48000 not offered, a 48 kHz lossy item came as frames with no audio (38 bytes: SSRC 0, an empty payload),
  one per play or flush, and was silent: YouTube's second video was silent when the first had 44.1 kHz audio and it
  48 kHz. Every Apple TV stream also carries one or a few of those empty frames; they are counted, not played.
- **The buffer size paces lossless**: the SETUP's `audioBufferSize` is how far ahead the sender streams, and an
  Apple TV holds an ALAC/48000/S24 stream (~1.3 Mb/s) to it: with 256 KB (~1.5 s) it sent a second of audio every
  10 s and dropped the rest (its sequence numbers ran at real time); with 4 MB (what the playout queue holds, ~20 s
  of it) it plays without a gap [HW 2026-10-30].
- **A Mac's Music** sends AAC: AAC-LC/48000 or /44100 from one session to the next with these offers (it took /48000
  for everything when it was offered, resampling 44.1 kHz music); offered only ALAC it sets up no stream and drops
  the speaker after 7 s [2026-10-28].

Buffered ALAC is decoded with Apple's encoder defaults (352 frames a packet, no cookie in the SETUP), on Warp0
(`W0_OP_RTP_AUDIO` on plain frames, 16- and 24-bit, as shairplay's `Decoder` for the stream) or, when Warp0 is taken or
gives up, by shairplay's decoder on the ARM, which matches FFmpeg's bit for bit on 24-bit 48 kHz ALAC [host].
**24-bit samples keep their depth to the DSP**: shairplay's `SampleFormat::Integer` hands a 24-bit stream's session
32-bit samples (16-bit streams stay 16-bit), `zd1-audio` queues them as i32 for that stream, ALSA's S32 and AV port
2's 24-bit I2S carry them, and the DSP's input from the module is 24 bits wide (`docs/hardware/zd1-main-board.md`).
Measured [HW 2026-10-30, in bench mode: `--device null`, no MCU]: Apple's encoder's 24-bit frames
(`module/warp0/vectors/afalac24.py`, 20 s) sent by `ap2send --alac48` came out of `zd1-audio` as the source PCM,
bit for bit (`ZD1_PCM_HASH`), with Warp0 and without; the receiver took ~15 % of the ARM with Warp0 against ~37 %
without (`soak`, while the sender paced), Warp0 2.4 ms per packet (33 % of its time). From an Apple TV (system
e674e93, two lossless Apple Music tracks for a few seconds each [HW 2026-10-30]): `Warp0 decodes this ALAC stream
bits=24 rate=48000`, the session at 32 bits, no xruns, the output on the sender's timeline (±0.4 ms once aligned). An Apple TV's ALAC/48000/S24 plays without a
late frame or an xrun; on the ARM its decode takes 3.4 ms of wall time per 7.3 ms packet [HW 2026-10-30].

**Volume.** A sender asks for the receiver's volume (GET /info `initialVolume`, AirPlay 1 GET_PARAMETER) before it
sets its own: it is told the speaker's level (zd1-linkd's last `volume`, as AirPlay dB: 0-100 % -> -30..0 dB of the
range up to the MCU's cap, step 40 of 71). A Mac's system output starts at it; a Mac's Music sets the volume it
remembers for the speaker at the first play anyway [2026-10-28]. Every volume a sender sets is logged (`sender's
volume`). Samples go out at full scale; the speaker's DSP sets the level.

**Remote control.** The speaker's keys and knob reach an AirPlay 2 sender as MediaRemote commands on the session's
event channel (`POST /command`, `sendMediaRemoteCommand`: 0 play, 1 pause, 4 next, 5 previous; the volume as
`dvlc`, 0.0-1.0; shairport-sync's remote.c), an AirPlay 1 sender over DACP (`play` / `pause`; not tried on the
unit). A Mac's Music acts on all of them: `zd1 key play` pauses and resumes, `next` / `prev` skip (Music
refuses a skip with nothing queued), `zd1 volume 20` moves its slider to 27, and Music doesn't answer `dvlc` with a
volume of its own (no loop) [2026-10-28]. The play/pause key is the sender's toggle; zd1-airplay drops a play to a
playing sender and a pause to one that isn't (the MCU's resume when it switches to the module, and its pause when it
leaves, would otherwise start Music).

The sender says which commands it takes now (`POST /command` `updateMRSupportedCommands`, again whenever they change:
each MediaRemote command enabled or not, the skips 17 / 18 with their preferred interval; Music sends ~30, with 4 / 5
off while nothing is queued and the 30 s skips off). Next and previous go as a skip when the sender takes only that
(`kMRMediaRemoteOptionSkipInterval`, the sender's interval; a podcast app), and aren't sent when it takes neither:
Music with one track logs `the sender takes neither ... not sent` for the next key, with a playlist it skips to the
next track [HW 2026-10-30]. Shuffle and repeat (6 / 7) go as Music's toggles, but no key sends them.

**Senders.** A second sender's stream takes the speaker and the first sender's connection is closed (it sees the
speaker go and pauses, as with Apple's receivers) [2026-10-28]. TCP keepalive on senders' sockets (probes after 15 s
of silence, every 5 s, three) frees the slot of a sender that vanished without closing: with the module's Wi-Fi
address gone under a Music stream, its connections closed 30 s later and the MCU no longer listed AirPlay
(`ip addr del` on wlan0 for 60 s) [HW 2026-10-30].
iOS keeps a remote-control-only connection open after it disconnects the audio: not a sender. A realtime stream's
pause is a stream-only TEARDOWN; the session stays.

**The Mac's system audio** (System Settings > Sound > Output): realtime
ALAC/44100/S16 with PTP; macOS starts at the speaker's volume, its volume and mute reach the speaker, the knob moves
its slider, the speaker's play/pause reaches the Mac's now-playing app [2026-10-28]. A receiver that restarts leaves
macOS stuck ("endpoint is already currently being activated") until it is picked again; Bluetooth headphones coming
into range take the output.

## Synchronised playback
AirPlay 2 groups play in sync by following one PTP clock (the sender's, or the group's) and playing each frame at the
network time its stream's anchor gives it.

**Results** [HW 2026-10-28, `tools/zeppelin/lab/musictest.py`: Music plays chirps at 15.5-17.5 kHz to a group of
"This Computer" and the speaker, the Mac's microphone times both on a shared 2 s grid]:
- Buffered streams (Music): the speaker within 0.6 ms of the Mac's own speakers, over the wired link and over Wi-Fi
  (+0.06..+0.29 ms over six wired sessions, +0.15 / -0.06 over Wi-Fi), the same after pause, seek and the next
  track; on the network timeline to ±0.1 ms from session to session.
- Realtime streams (a Mac's system audio, Spotify's AirPlay from an iPhone): on the timeline too; the Mac's system
  audio is heard 2042.3-2042.8 ms after CoreAudio's output time for it, against the 2000 ms the device reports (what a
  video player syncs to): 42.5 ms late by the Mac's own account (a CoreAudio
  IOProc's output times against the microphone, both in mach time). Whether to trim it
  (`SHAIRPLAY_REALTIME_LATENCY_MS=207.5` matches the Mac) depends on what Apple's receivers do with the same
  packets: not measured.

**The clock**: shairplay's `ptp::follower()` pairs every Sync with its Follow_Up (by clock and sequence number, the
two within a second: a master that restarts numbers from 0 again) and keeps, per master, the fastest
sample of each 2 s window and a least-squares line through the last 32 of them (the crystals' drift; usable after 8
samples, a drift fit once the windows span 8 s; it starts over when a master steps). Measured over 180 s against
`ap2send HOST --ptp-only SECS` (a PTP master on the Mac's CLOCK_MONOTONIC_RAW, 8 Hz like Apple's) with
`SHAIRPLAY_PTP_LOG=FILE` (kernel receive times, SO_TIMESTAMP) [2026-10-24]:

| | wired | Wi-Fi (module on 2.4 GHz, Mac on 5 GHz) |
|---|---|---|
| drift, module vs Mac | -22.4 ppm | -21.7 ppm |
| fastest samples vs the line | 57 us rms, 112 max | **284 us rms, 613 max** |
| all samples above the line | median 0.6 ms, p99 3.6 ms | median 1.7 ms, p90 11 ms, p99 39 ms, max 79 ms |
| the same with the task's own receive time | 184 us rms | 292 us rms |

**The path delay**: the fastest Sync samples still carry the one-way delay (+2.65 ms over Wi-Fi). While a master is
heard, shairplay sends it a Delay_Req once a second (a Mac answers during a session, with gPTP's transportSpecific 1)
and takes half the least round trip of the last 32 (once there are 5) off the offset; the round trip is on our clock
(Delay_Resp's kernel receive time minus the send time), because a Mac's `receiveTimestamp` in Delay_Resp isn't on its
Sync timescale (it saws by ~20 ms a second). Measured: 2.2 ms over Wi-Fi, 0.33 ms over the wired link [2026-10-28].

A Mac's PTP time is its `mach_absolute_time` (`AirPlayXPCHelper` logs `APSNetworkTime` and `upticks` side by side),
the clock that also stamps CoreAudio's input: so the Mac's microphone measures any device against the network
timeline directly (`musictest.py sync`).

**The anchor** (buffered streams; the Mac's `AirPlayXPCHelper` log, shairport-sync): SETRATEANCHORTIME carries
`networkTimeSecs`, `networkTimeFrac` (a 64-bit binary fraction), `networkTimeTimelineID` (the PTP clock), `rtpTime`
and `rate`. The fraction and the clock ID arrive as *signed* plist integers whenever their top bit is set (read as
unsigned they lose the fraction in about half the sessions). The anchor's network time is the moment of the request
and the first frame sent is due 300 ms later (`startupDelay 0.300000` in the Mac's log); a frame plays at exactly its
network time, as shairport-sync does. Music sends the first frames ~40 ms before the anchor.

**Realtime streams** (type 96): the sync packets on the control port (0xd7, about one a second: the RTP time that
plays at a network time on the sender's PTP clock, the 77175-frame latency, the clock's ID) give each packet its play
time, plus the receiver's own latency: the SETUP's `latencyMin` when it is sane (a Mac's: 11025, `latencyMax` 88200),
else 11025 frames (250 ms; a Mac reports a stream latency of 88200 = 77175 + 11025, and shairport-sync adds the same).
A FLUSH with `RTP-Info: seq=N;rtptime=T` (a Mac's: 0.3 s after each stream's SETUP, before its first packet) drops
what is before T; without RTP-Info everything until the next sync packet. An `audioLatencies` entry in GET /info
(what the esp32-airplay2 receivers send) changes nothing a Mac does: not sent. A realtime stream ends with its
TEARDOWN, also at a pause, with its last ~2 s still queued: they are dropped, so a paused video in a Mac's browser
stops 0.2-0.3 s after the player does; a short sound a player ends by itself loses what hadn't played (a 2 s `afplay`
clip is heard for 0.25 s: macOS tears down ~0.5 s after the clip ends).

**Lost packets** (realtime streams and AirPlay 1 over UDP; shairplay `raop/reorder.rs`): packets go out in sequence
order; behind a gap the later ones are held while the gap is asked for on the sender's control port (0x80 0xd5, our
sequence number, first, count: shairport-sync's form), again after 50 ms, and come back on ours behind a 4-byte
header (0x80 0xd6). A gap not filled within 120 ms, or with 48 packets held (AirPlay 1: 24, what its 32-slot buffer
takes at once), plays as silence of the same length: the untimed output keeps ~150 ms past ALSA's (`zd1-audio`
`PREBUFFER_MS`), so it can't wait longer. An AirPlay 2 realtime stream on the network timeline plays ~2 s after its
packets arrive (a Mac's lead: 1.95 s), so there packets are decoded as they arrive (ALAC packets stand alone; their
samples are reordered; only a packet that still has a place is decrypted and decoded) and a gap waits until 400 ms
before its first missing packet plays (at most 2 s), asked for again every 250 ms after the first two asks, four
times in all, a run of missing ones in one request; what was held behind it is decoded by then. A packet Warp0
can't take because its queue is full waits for a slot instead of going to the ARM (`zd1-warp0` `OnWarp`): the ARM
decodes in the receiver's loop, so the burst after a stall would keep the loop off its socket (it overflowed: 746
datagrams in a minute) [HW 2026-11-02]. The latency is the
sender's either way: every packet plays at its stamped time. A gap given up is a hole in the play times: the output
carries the audio before it through a hole of up to 25 ms (mirrored, fading out) and puts silence between fades in
a longer one (`zd1-audio` `fill_gap`). With 120 ms, a Mac's system audio over a weak link (the Mac at -73 dBm) had
10 s windows with 77 packets late and 51 gaps filled, packets that would still have played; with these rules
windows of 86 and 135 missing packets were all recovered but 2 (waits up to 836 ms, no silence played), the module
keeping up (40 socket overflows in all, against 746 in a minute when it still decoded late packets and sent bursts
to the ARM); what remains are bursts where the link delivers almost nothing for a second or more (499 missing, 298
of them past their deadline: the Mac sending steadily, a capture on the Mac showed 125 packets a second and the
resends asked for), after which the output steers back onto the timeline (`repeated`, `jumps`) [HW 2026-11-02]. Over Wi-Fi from `airplay2-rs` (Mac on 5 GHz, the module on 2.4 GHz),
realtime ALAC: 23 of 1229 packets missing in 10 s, 20 resent in time (46 ms on average, 120 at most), 3 as silence;
the longest time without any packet 123 ms [HW 2026-10-30]. AirPlay 1 asks the same way since system 5f66c37
(10 missing, 20 asks in 10 s), but `airplay2-rs --airplay1` never answers (its RAOP path doesn't read its control port): its 10 came 120 ms+
late and played as silence. Not tried with a sender that resends AirPlay 1. Counts every 10 s in the log:
`realtime 10 s`, `AP1 10 s`.

**The local clock**: play times, the PTP follower's samples and the output's estimate are on CLOCK_MONOTONIC (shairplay's
`now_ns`, zd1-audio's `mono_ns`; the kernel's receive times, on the wall clock, moved onto it), which NTP's step at
boot doesn't move: the module boots in 1970 [HW 2026-10-30].

**Playout** (shairplay `raop/buffered_audio.rs`, then `module/os/apps/zd1-audio`'s output): compressed frames queue in
the sender's order with their 23-bit sequence numbers (at most 4 MB). FLUSHBUFFERED without `flushFromSeq` (Music's
stop, seek or new track) drops everything before `flushUntilSeq` and stops until the next anchor; with it (a deferred
flush) it drops that range wherever playout is, and flushes the output too if the range has reached it. Frames go to
the output 500 ms before they are due with their local play time, to Warp0's AAC decoder 80 ms before that; frames
already past their time are dropped undecoded. The first AAC frame after any discontinuity (a flush, frames skipped
as late, a stream's start) is decoded only to prime the decoder and not played: AAC's frames overlap, and that frame
would be the new audio over the old position's uncancelled aliasing. The output then:
- **predicts** when the next sample written leaves the speaker: when the device's frame 0 played (each write's now +
  ALSA's delay - the frames written so far, the least of the last ~1.5 s, only while the device runs and only from
  full-buffer figures) + the frames written + `OUTPUT_OFFSET_US` (-3.6 ms, calibrated below). ALSA's delay moves
  only once per 256-frame period and a write the scheduler holds back reads late; the raw figure is 0-6 ms noisy, the
  least of the figures within ~0.1 ms. Figures from while the buffer fills (after a start or an xrun) read up to
  5.4 ms early: when they aged out of a single window the output slewed for seconds, and waiting for a full buffer
  before the first audio put it 150-170 ms late, so they count only until there are full-buffer ones [HW 2026-10-28].
  zd1-snapcast's player takes its time from the same measurement (`zd1-audio`'s `device.rs`).
- **steers** there: aligns once after a start or a flush (silence until the first frame's time, or skips what is
  already late, then fades in); after that, from 0.5 ms off until within 0.1 ms, one frame more or fewer per
  1024-frame write, stretched over the write by interpolation (0.1 % for 23 ms); a jump only past 10 ms. On the unit:
  0-19 frames a minute, one way (the crystals' drift), within -0.23..+0.53 ms. A jump goes by the error itself, not
  the smoothed one, which lags (skipping by it caught up on audio 196 ms late in ~50 cuts of ~12 ms over a second),
  and the output aligns until a figure after the jump is within 0.5 ms (marked aligned after a skip, a frame's worth
  still off took nine writes and a second jump) [HW 2026-10-28].
- **flushes without a cut**: ALSA's queue past the hardware's pointer is dropped (`snd_pcm_reset`; the device keeps
  running) and written again up to a 5.8 ms fade, placed in the period after the pointer (snd-sulu has already
  copied the pointer's period into SRAM: a fade written into it never played, and the old audio stopped dead at its
  end [HW 2026-10-28]). A timed stream never stops the device: an empty queue is written as
  silence, so new audio starts measured and aligned, each new position on its time to 0.01 ms after 3-5 writes. The
  stop threshold is the buffer + 2 periods (with less, the driver's period interrupt falling between the reset and
  the fade counted as an xrun). Audio also fades (5.8 ms) where the queue runs dry and where it starts again. While
  scrubbing, Music sends a pause, a flush and a new anchor per step, the anchor 0.1-1.1 s in the past (its first
  frames are dropped as late).
- **fills missing frames**: a timed push whose time is a whole number of frames after the queue's end (within
  0.7 ms) gets silence in their place, between fades: after a seek the first frames come with sequence gaps, and
  played as they came each gap was a 23 ms (one AAC frame) step in the timing, which the output then jumped or slewed
  for seconds. Anything else off the queue's end is the clock's mapping moving (a 600 ms Wi-Fi stall moved it by
  -79 / +19 / +55 ms within 70 ms) and the steering rides through it: filling and cutting by it made dropouts
  [HW 2026-10-28].
- **buffers** 186 ms in ALSA; a burst of output on the module's serial console held the output thread for 117 ms once,
  and 93 ms ran dry then (a terminal on the console while playing: read logs over the network instead, `nc`) [HW 2026-10-28].
  An untimed stream that runs dry while under way waits for a prebuffer 250 ms deeper each time, up to 1 s: over
  Wi-Fi a realtime stream's packets stop for ~300 ms at times (the sender or the AP; nothing lost), past what the
  first 250 ms leaves queued beyond ALSA's [HW 2026-10-24]. Timed streams keep their first prebuffer after running
  dry and write silence on: their frames come a set time ahead of when they play, so a deeper prebuffer couldn't
  fill before they are due (it waited while they went past their time, and ran dry again and again for seconds).

**Calibration**: with no output offset the speaker played 3.6-4.0 ms ahead of the Mac's own speakers in three
sessions, hence -3.8 ms with ALSA's delay as the measurement; the prediction above reads 0.17 ms less on average
(1507 writes), so it is -3.6 ms. The speaker's place on the timeline is 44.0-44.8 ms after it with the microphone path
included (`musictest.py sync`). Pitfalls when measuring:
- **The Mac's own speakers are not always on the timeline**: in two of eleven sessions they played 22 and 31 ms early
  while the speaker stayed put. Compare against the network timeline before believing a group result.
- **The microphone must hear the speaker directly**: a chirp is timed by its strongest arrival, and at 16 kHz the
  MacBook's own lid can shadow one of the speaker's tweeters, so a room reflection ~3.6 m longer wins and the speaker
  reads ~11 ms late (it did for an afternoon, through builds and reboots, with the clock right throughout). Face the
  MacBook to the speaker; `musictest.py channels` times each channel over USB, and `musictest.py sync` says when the
  speaker was heard earlier than the arrival it timed.
- **Late at night the Mac's Siri listener** (`corespeechd`) takes the audio session at every play and Music drops the
  speaker (-128 in Music's log): acoustic runs need the Mac in use or Siri's listening off.
- Music applies its loudness gain to the tracks it sends (-18 dB on the chirp tracks); the AAC it sends keeps
  everything up to 20 kHz flat (ZD1_PCM_DUMP of a tone ladder).

**With an Apple TV** [HW, by ear, before 2026-10-29]: the speaker, a MacBook and the Apple TV in one group
played in step. The soundbar on the Apple TV's HDMI was out of step with them: most likely the soundbar's own delay,
which the Apple TV doesn't know (tvOS's Wireless Audio Sync calibrates it). Not measured: realtime streams against the
Apple TV (the 42.5 ms), a HomePod, AirPlay 1 on its (NTP) timeline.

**At 48 kHz** (a Mac's Music sends AAC-LC/48000 at times since it is offered): the speaker is on the network timeline
as at 44.1 kHz, 45.1 ms against 45.6 ms with the microphone path (`synctest.py gen DIR 40 48000`, `ap2send --aac48`,
two runs each, sd 0.1-0.2 ms) [HW 2026-10-30]. In a Music group at 48 kHz the Mac's own speakers play 11.0-11.8 ms
before it (three sessions, sd 0.1 ms; at 44.1 kHz within 0.6 ms): the Mac's own output, not the speaker.

## Snapcast (zd1-snapcast)
`module/os/README.md`, Snapcast, has the receiver: its playout delay (the server's buffer, plus a margin on Wi-Fi
that a stall grows instead of skipping), FLAC on Warp0 with our decoder's ARM build as the fallback. On the unit
[2026-10-29]: the client 3.8 % of the ARM with FLAC on Warp0 (16 % of Warp0 at real time for snapserver's level 2),
15.2 % with our decoder on the ARM; FLAC over Wi-Fi ~970 kbit/s at 103 packets/s, everything included (PCM alone is
1411 kbit/s).

## Spotify Connect (zd1-spotify)
librespot ([librespot-org/librespot](https://github.com/librespot-org/librespot), MIT, upstream `dev`) with rustls + ring
for TLS and our changes (`module/os/forks.md`). What Spotify sends: Ogg Vorbis at 96/160/320 kb/s (librespot asks for
320 first: Connect needs Premium), AES-128-CTR per file, over HTTPS from the CDN; librespot would take MP3 for files
that have no Vorbis (some podcasts), which nothing here decodes (logged, skipped).

| piece | how | [HW 2026-10-25] |
|---|---|---|
| Vorbis | our decoder (`module/codecs/vorbis`) behind librespot's new `DecoderPlugin`, integer samples (`AudioPacket::S16`) to our sink; the Ogg layer is the `ogg` crate | Warp0: 36.9 % of its time for 320 kb/s; the ARM: 20.4 % (Tremor 23.7 %) |
| decode + output | packets 8 ahead on Warp0, ARM fallback seamless; the sink holds the player 500 ms ahead of the speaker | **11.5 % of the ARM with Warp0, 30.3 % without**, no xruns; the output the host decoder's sample for sample, also after a seek, and mono |
| memory | each track through a 1.5 MB window (librespot's fetcher kept the whole file in /tmp = RAM) | receiver idle 1.5 MB RSS; +3.2 MB of image |
| discovery | librespot's zeroconf server (IPv4: the kernel has no IPv6) + libmdns (no SO_REUSEPORT on 2.6.24) | `_spotify-connect._tcp` "Zeppelin Air 000042" seen by the Mac; `getInfo` answers |
| network | DNS, HTTPS to apresolve (HTTP/2, rustls), the access point (DH, Shannon); TLS needs the wall clock (NTP at boot) | the access point answers made-up credentials with "Bad credentials" after ~1 s |
| control | Spotify's volume -> the speaker's level (zd1-linkd); the speaker's keys and `yield` -> Spirc | not tried (needs a session) |

**The audio keys** [HW 2026-10-25, 2026-10-28]: picked in the Spotify app, zeroconf, the login (country US),
Connect and the queue work; then Spotify refuses the audio key of every track, the first one included ("error audio
key 0 1"). Upstream librespot `dev` (99b6088) on the Mac gets the same for this account: code `0x0001` within ~35 ms
for every track, the first request of a session included, at 96, 160 and 320 kb/s, with zeroconf's and OAuth's stored
credentials and a fresh OAuth token, with the desktop, Android and iOS client IDs. Code `0x0002` is the one retries
cure; `0x0001` has never been seen to pass. Reports across librespot-based players since late 2025
(librespot-org/librespot#1649) find that newer accounts don't get the keys through the access point; the test account
is an old one and refused just the same. The other way to the keys is Spotify's DRM: not something this project
works around. Not reached for this reason: metadata, the CDN download through the window, AES-CTR, a whole track,
volume and keys.

**Failing well**: a refused key stops the player on the track (librespot#1799) and Spirc stops on the current track
when a skip fails or five tracks in a row fail; each pick, play and next is one load and one refusal, the app shows
the track paused within ~2 s, and the next pick is answered at once [HW 2026-10-28].

**Spotify Soloist** ([spotify/soloist](https://github.com/spotify/soloist), Spotify's own headless Connect receiver)
plays with any account, but not on the module: its Linux builds are ARMv7, AArch64 and x86_64 with PipeWire or
PulseAudio (besides macOS on Apple Silicon and Windows). It needs a per-account API key from the Spotify for
Developers dashboard (`--api-key`), its builds expire 90 days after their build date (exit code 10), and the binaries
may not be redistributed (never in an image or an upload). On macOS it plays to the default output only. So Soloist
on a host can only bridge: to the speaker over AirPlay (the Mac's output set to "Zeppelin Air"), or on Linux through a
PipeWire sink into a snapserver that `zd1-snapcast` plays.

## ARM926 costs

| per stream, real time | % of the ARM |
|---|---|
| ALAC decode (FFmpeg `alac`, 848 kb/s) | 19 |
| AAC-LC decode, fixed point: ours / FFmpeg `aac_fixed` / FDK (limiter off), 233 kb/s | 14.8 / 25 / 18.5 |
| AAC-LC decode, float (FFmpeg `aac`) | 90 |
| Vorbis decode: ours / Tremor, 160 and 320 kb/s | 17.8, 20.4 / 19.1, 23.7 |
| Vorbis decode, float (FFmpeg `vorbis`) | 96-102 |
| AES-128-CBC at ALAC's 176 KB/s (AirPlay 1): mbedtls / RustCrypto's constant-time AES (the receiver's) | 8 / 15 |
| ChaCha20-Poly1305 at 176 KB/s (AirPlay 2 realtime ALAC): mbedtls -O2 / libsodium / mbedtls -Os / RustCrypto | 5 / 8 / 11 / 4.4 |
| ChaCha20-Poly1305 at 32 KB/s (AirPlay 2 AAC) | 1 |
| AES-128-CTR at 40 KB/s (Spotify 320 kb/s) | 2 |
| ALSA output (`snd-sulu`, `docs/module/linux.md`): woken every period / with `avail_min` 4096 | 5.6 / 1.8 |

FFmpeg's figures include its container parsing and the conversion to 16 bit. Peak RSS: `aac_fixed` 2.8 MB, `alac`
1.7 MB, Tremor 0.4 MB.

| once per connection | ARM time |
|---|---|
| RSA-2048 private operation (CRT), mbedtls: sign or OAEP decrypt / the `rsa` crate's OAEP decrypt (the receiver's) | 0.55 s each / 0.36 s |
| modexp mod a 3072-bit prime: 256 / 512 / 3072-bit exponent | 0.38 / 0.76 / 4.5 s |
| SRP-6a server, 3072 bit: transient pair-setup's M3, with the server's state and `v^u`'s powers made ahead of time; Montgomery arithmetic on 96 words (FIOS) [HW 2026-10-30; with `num-bigint`: 1.0 s, 0.4 of it `v^u`'s divisions] | 0.52 s |
| X25519: libsodium / mbedtls / dalek (keygen + shared secret) | 9.5 / 92 / 12 ms |
| Ed25519 sign / verify: libsodium / dalek | 4.6 / 11 ms; 4 / 10 ms |
| DH mod a 768-bit prime (Spotify) | 93 ms |
| PBKDF2-HMAC-SHA1, 1000 iterations | 37 ms |
| AES-128-GCM (HTTPS) | 436 KB/s |

## Where the Warps fit
- **Warp0** (`module/warp0/`, `module/warp.md` section 6): 160 MIPS with 48 KB of single-cycle code and data RAM; data
  in SDRAM costs ~19 cycles a load. Its ciphers run at about the ARM926's speed (`module/warp0/README.md`). The ARM's side of a hand-off is `memcpy` through the uncached window: ~80-130 MB/s
  writing, ~57-88 MB/s reading (8-word bus bursts; one volatile word at a time manages ~9 MB/s), ~0.03 us per byte in
  and out [2026-10-24]. `recv()` into the window works [2026-10-24].
  Bypassing Linux's network stack (Warp0 reading the receive rings) would save only the kernel's copy, and needs a
  kernel module we can't build without the kernel's source. What runs there:
  - **a whole realtime packet: decrypt + ALAC decode** (AirPlay 1, AirPlay 2 realtime; `W0_OP_RTP_AUDIO`,
    `module/warp0/fw/alac.c`, a port of shairplay-rust's decoder, 16- and 24-bit), as a shairplay-rust
    stream's `Decoder`: ~2 ms (~300 k cycles) per stereo packet, ~25 % of Warp0.
  - **buffered ALAC frames** (the same op on plain frames, the stream's `Decoder`): 24-bit ALAC/48000 2.4 ms per
    stereo packet (33 % of Warp0).
  - **AAC frames** (buffered AirPlay 2; our decoder, code from SDRAM through the I-cache, the working set in local
    RAM): 7.0 ms per stereo frame (30 % of Warp0), submitted 80 ms ahead of playout.
  - **Vorbis packets** (Spotify; code and the setup's tables in SDRAM): 5.6 ms per 320 kb/s packet (37 % of Warp0).
  - **FLAC frames** (Snapcast; all in local RAM): 8.4 ms per 52 ms chunk of snapserver's level 2 (16 % of Warp0).
  - Not done: Spotify's AES-CTR alone (~2 % of the ARM); the buffered stream's ChaCha20-Poly1305 (~4 %).
  - DMA: Warp0 can start SuluDMA block moves (`module/warp.md` section 5), which would spare it its SDRAM loads; at
    audio rates that is <1 % of Warp0.
  - Bignum (pairing): a multiply-accumulate step costs Warp0 ~7-10 cycles against the ARM's ~4: parallel, not faster.
- **Sol** (Warp1's "Audio Engine", BridgeCo's 600 MIPS for decode and sample-rate conversion). Its data memory is
  ten single-ported 1 KB banks (Warp1's whole local RAM: 10 KB), so the datapath can touch several banks per cycle: a
  vector scratchpad that blocks stream through by SuluDMA, not a place for a decoder's code or tables, which stay in
  SDRAM (Warp1's code runs through its 4 KB I-cache). Warp1 is Sol's sequencer. The on-chip SRAM (64 KB, ~6 cycles a
  load for either Warp; ~22 KB unclaimed under Linux, plus ~15 KB of LCD line buffers this speaker never shows) is
  the middle tier.
  - **Kernels**: an IMDCT or FFT per frame costs the ARM little to hand over (coefficients in and samples out at
    ~0.03 us/byte: ~2 % for a 44.1 kHz stereo stream). With the transform about 30 % of an AAC decoder's time, Sol
    would save a few % net [INF]; a 1024-point transform fits the banks per channel. Needs Sol programs written in the
    decoded ISA (`module/warp.md` section 5): not started; with the decoders on Warp0 there is little left to save.
  - **Owning the output**: decoded PCM written once into its reach and moved by SuluDMA straight to AV port 2
    (KnOS's design), replacing the ALSA path, with resampling or other DSP on Sol. That means driving the port and the
    PTU FIFO ourselves instead of snd-sulu: a reverse-engineering project, not started, and it would save little: the
    whole ALSA path is ~1.8 % with few wake-ups, and the output's timing is within ~0.1 ms through ALSA. The speaker's
    own DSP already does EQ and volume.
  - KnOS ran its decoders (AAC, FLAC, MP3, Vorbis, WMA "StreamProcessors") on the ARM; its Warp framework
    ("Warplets") carries framers and the audio-effect program, not codecs [RE: string table].
