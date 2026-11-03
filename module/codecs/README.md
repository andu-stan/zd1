# Audio decoders

The decoders our receivers use, in C so they run both on the ARM926 and on Warp0 (the DM870's MIPS core,
`module/warp0`), each with its Rust binding (`rust/`) and one host tool (`check/`) that decodes a stream to PCM, compares it with a
reference decoder's, fuzzes the decoder under ASan/UBSan and times it (the same C builds for the unit):

| | |
|---|---|
| `aac/` | AAC-LC (AirPlay 2 buffered audio, `aac/README.md`) |
| `vorbis/` | Vorbis (Spotify, `vorbis/README.md`) |
| `flac/` | FLAC (Snapcast, `flac/README.md`) |
| `dsp/` | what the AAC and Vorbis decoders share: the fixed-point IMDCT and its tables (`dsp/README.md`) |
