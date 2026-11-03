/* Warp0 compute runtime: the interface between the firmware images on Warp0 (fw/) and the ARM (rust/).
 * docs/module/warp.md section 6. Addresses are physical: the Warp sees SDRAM at the ARM's physical addresses, and
 * the ARM maps the window through /dev/mem with O_SYNC (uncached, so both sides see every store).
 *
 * The window is the top MB's lower part, outside Linux (mem=31M): the boot stub's place (0x61F00000), free once the
 * kernel runs, up to the Warp1 window (0x61FC0000, wsl.ko). No image change needed.
 *
 * This header is also read by rust/build.rs, which turns every `#define NAME <number>` into a Rust constant: keep
 * values plain numbers (or parenthesised expressions of numbers and earlier names) on one line. */
#ifndef W0ABI_H
#define W0ABI_H

#define W0_BASE 0x61F00000u
#define W0_SIZE 0x000C0000u /* 768 KB, up to the Warp1 window */
#define W0_END  0x61FC0000u
/* The firmware runs from Warp0's local RAM: 48 KB at Warp address 0 (ARM 0xAC000000), where instructions and
 * loads and stores take one cycle each (SDRAM: a load ~19 cycles). Code, tables, keys and stack live there; the
 * ring and the buffers stay in SDRAM, where the ARM can reach them cheaply. Images too big for it run their code
 * from SDRAM through the I-cache, above their buffer area (W0_*_SDRAM). */
#define W0_LOCAL_ARM    0xAC000000u /* local RAM as the ARM sees it */
#define W0_LOCAL_SIZE   0x0000C000u
#define W0_STACK_TOP    0x0000C000u /* the stack: the top 4 KB of the local RAM */
#define W0_LOCAL_MAX    0x0000B000u /* an image's local part, .bss included, ends below the stack */
#define W0_INVAL        0x61F0F000u /* the I-cache invalidate stub (fixed content: runs right even from a stale cache) */
#define W0_CTRL         0x61F10000u /* control block */
#define W0_RING         0x61F10100u /* job ring: W0_RING_N descriptors of 64 bytes */
#define W0_RING_N       32u
#define W0_DATA         0x61F20000u /* buffers, allocated by the ARM, up to the running image's C_DATA_END */
#define W0_AAC_SDRAM    0x61F90000u /* the AAC image's code and data (fw/w0aac.ld), up to W0_END */
#define W0_VORBIS_SDRAM 0x61F60000u /* the Vorbis image's code, data and setup arena (fw/w0vorbis.ld) */

#define W0_HANG_MS 250u /* a stream job not done in this long is a hang (a packet or frame takes 2-8 ms) */

#define W0_MAGIC   0x54523057u /* "W0RT" */
#define W0_VERSION 3u

/* image ids (C_IMAGE) */
#define W0_IMAGE_CRYPTO 0x50595243u /* "CRYP": AirPlay's ALAC packets, decrypted first (fw/main.c) */
#define W0_IMAGE_AAC    0x20434141u /* "AAC ": the AAC-LC decoder (fw/aac_main.c) */
#define W0_IMAGE_VORBIS 0x42524F56u /* "VORB": the Vorbis decoder (fw/vorbis_main.c) */
#define W0_IMAGE_FLAC   0x43414C46u /* "FLAC": the FLAC decoder (fw/flac_main.c) */

/* control block words (W0_CTRL + 4 * n) */
#define C_MAGIC    0  /* Warp: W0_MAGIC once it polls the ring */
#define C_VERSION  1  /* Warp: W0_VERSION */
#define C_BEAT     2  /* Warp: counts idle polls (alive) */
#define C_DONE     3  /* Warp: jobs completed */
#define C_CMD      4  /* ARM: W0_CMD_* */
#define C_OPS      5  /* Warp: bit n = op n supported */
#define C_STATE    6  /* Warp: W0_ST_* */
#define C_HEAD     7  /* Warp: the ring slot it polls next (a client attaching to a running Warp starts there) */
#define C_IMAGE    8  /* Warp: W0_IMAGE_* */
#define C_DATA_END 9  /* Warp: the end of the buffer area it accepts pointers into, [W0_DATA, C_DATA_END) */
#define C_JOB      10 /* Warp: slot << 16 | op of the job it is running, W0_NOJOB between jobs */
#define C_STAGE    11 /* Warp: how far the running job got (the op's own markers: a hang report says where) */
#define C_REFUSED  12 /* Warp: jobs refused with W0_ERANGE / W0_EALIGN */
#define C_WORDS    16

#define W0_NOJOB 0xFFFFFFFFu

#define W0_CMD_RUN  0
#define W0_CMD_STOP 1 /* the Warp leaves the loop and spins in registers (no bus traffic); the ARM then parks it */

#define W0_ST_BOOT    0
#define W0_ST_IDLE    1
#define W0_ST_BUSY    2
#define W0_ST_STOPPED 3

/* job descriptor words. The ARM fills them, then sets D_STATE = SUBMIT, in ring order. The Warp runs the job,
 * writes D_RESULT, then D_STATE = DONE; the ARM reads the outputs and sets D_STATE = FREE. */
#define D_STATE  0
#define D_OP     1
#define D_LEN    2 /* bytes of src */
#define D_SRC    3
#define D_DST    4 /* may equal D_SRC */
#define D_KEY    5 /* 32 bytes (ChaCha20-Poly1305) or 16 (AES-128) */
#define D_NONCE  6 /* 12 bytes (ChaCha20) or a 16-byte IV / counter block (AES) */
#define D_AAD    7
#define D_AADLEN 8
#define D_TAG    9  /* 16 bytes: the Poly1305 tag RTP_AUDIO checks; FLAC_DECODE: the input bytes taken */
#define D_CTR    10 /* RTP_AUDIO: its mode; decoders: their error code back */
#define D_RESULT 11 /* W0_OK, W0_EAUTH, W0_EOP, ... */
#define D_CONF   13 /* RTP_AUDIO: the stream's ALAC config, 6 words (fw/alac.h struct alac_config) */
#define D_OUTLEN 14 /* bytes written to D_DST (Warp) */
#define D_WORDS  16

#define W0_FREE   0u
#define W0_SUBMIT 0x53554221u
#define W0_DONE   0x444F4E45u

/* Every pointer a job uses must be word aligned (W0_EALIGN) and lie, with its length, inside the buffer area
 * [W0_DATA, C_DATA_END) (W0_ERANGE): Warp0 has no memory protection and no exceptions, and an access to the wrong
 * place hangs its bus (docs/module/warp.md section 3), so the firmware checks before it touches anything. */
#define W0_OK      0u
#define W0_EAUTH   1u /* AEAD tag mismatch: D_DST not written */
#define W0_EOP     2u /* unknown op */
#define W0_EALIGN  3u /* a pointer not word aligned */
#define W0_ELEN    4u /* a length over the op's maximum */
#define W0_EFMT    5u /* a stream format the firmware doesn't decode (D_CTR: the decoder's code) */
#define W0_EDECODE 6u /* a decoder op: the frame doesn't decode, dropped (D_CTR: the decoder's code) */
#define W0_ERANGE  7u /* a pointer and length outside the buffer area */

/* ---- the crypto image (W0_IMAGE_CRYPTO). Lengths are bytes. ---- */
#define W0_OP_NOP 0 /* round trip only (every image) */
/* one audio packet's payload: decrypt (D_CTR & W0_RTP_CIPHER) into local RAM, then
 * ALAC-decode it (D_CTR & W0_RTP_ALAC) to interleaved samples at dst (16-bit: S16;
 * 24-bit: a word each, sign-extended), or copy the plaintext there; D_OUTLEN = bytes
 * written. Plain + ALAC: AirPlay 2 buffered ALAC frames. AES-128-CBC: key, nonce = IV,
 * whole blocks, a tail of len % 16 copied as it is (AirPlay 1). ChaCha20-Poly1305
 * (RFC 8439 2.8): key, nonce (12 bytes), aad, tag, the tag checked in constant time
 * first (AirPlay 2 realtime); W0_EAUTH. dst must hold W0_RTP_OUT bytes. */
#define W0_OP_RTP_AUDIO 1
#define W0_CRYPTO_OPS   ((1u << W0_OP_NOP) | (1u << W0_OP_RTP_AUDIO))

#define W0_RTP_CIPHER      0x3u
#define W0_RTP_PLAIN       0x0u
#define W0_RTP_AES_CBC     0x1u
#define W0_RTP_CHACHA_POLY 0x2u
#define W0_RTP_ALAC        0x10u
#define W0_RTP_MAX         2560u /* payload bytes (a 352-sample stereo frame is ~1.4 KB at most) */
#define W0_ALAC_MAX_FRAME  512u  /* samples per ALAC frame (fw/alac.h; AirPlay uses 352) */
#define W0_RTP_OUT         4096u /* PCM bytes at most: 512 samples x 2 channels x 4 (24-bit) */

/* ---- the AAC image (W0_IMAGE_AAC): w0aac_local.bin at Warp 0, w0aac_sdram.bin at W0_AAC_SDRAM, entry there ---- */
#define W0_OP_AAC_INIT 2 /* D_LEN = sample rate, D_CTR = channels (1, 2); W0_EFMT if unsupported */
/* one raw AAC-LC access unit (D_SRC, D_LEN <= W0_AAC_MAX_AU) -> 1024 * channels
 * interleaved S16 at D_DST (W0_AAC_OUT bytes); D_OUTLEN = bytes; W0_EDECODE */
#define W0_OP_AAC_DECODE 3
#define W0_AAC_MAX_AU    2048u
#define W0_AAC_OUT       4096u
#define W0_AAC_OPS       ((1u << W0_OP_NOP) | (1u << W0_OP_AAC_INIT) | (1u << W0_OP_AAC_DECODE))

/* ---- the Vorbis image (W0_IMAGE_VORBIS): w0vorbis_local.bin at Warp 0, w0vorbis_sdram.bin at W0_VORBIS_SDRAM ---- */
/* D_SRC / D_LEN = the identification header, D_AAD / D_AADLEN = the setup header;
 * W0_EFMT (D_CTR: module/codecs/vorbis's code) if the stream isn't one it decodes;
 * D_OUTLEN = setup bytes used in the SDRAM arena */
#define W0_OP_VORBIS_INIT 4
/* one audio packet (D_SRC, D_LEN <= W0_VORBIS_MAX_PACKET) -> interleaved S16 at
 * D_DST (W0_VORBIS_OUT bytes); D_OUTLEN = bytes (0 for the first packet: it primes
 * the overlap); W0_EDECODE (D_CTR: the decoder's code): dropped */
#define W0_OP_VORBIS_DECODE  5
#define W0_OP_VORBIS_RESTART 6 /* forget the overlap (after a seek) */
#define W0_VORBIS_MAX_HEADER 16384u
#define W0_VORBIS_MAX_PACKET 16384u
#define W0_VORBIS_OUT        4096u /* 1024 samples x 2 channels x 2 */
#define W0_VORBIS_OPS                                                                                                  \
    ((1u << W0_OP_NOP) | (1u << W0_OP_VORBIS_INIT) | (1u << W0_OP_VORBIS_DECODE) | (1u << W0_OP_VORBIS_RESTART))

/* ---- the FLAC image (W0_IMAGE_FLAC): w0flac.bin at Warp 0, all in local RAM like the crypto image ---- */
/* D_SRC / D_LEN = the 34-byte STREAMINFO body; W0_EFMT (D_CTR: module/codecs/flac's
 * code) for a stream it doesn't decode (> 2 channels, > 24 bits, blocks > 4608) */
#define W0_OP_FLAC_INIT 7
/* whole frames (D_SRC, D_LEN <= W0_FLAC_MAX_IN) -> interleaved int32, each sample
 * left-justified, at D_DST (W0_FLAC_OUT bytes); D_OUTLEN = bytes, D_TAG = input bytes
 * taken (less than D_LEN when the output is full: submit the rest again);
 * W0_EDECODE (D_CTR: the decoder's code): the frames before the bad one are out */
#define W0_OP_FLAC_DECODE 8
#define W0_FLAC_MAX_IN    32768u
#define W0_FLAC_OUT       65536u /* 8192 stereo frames: one 4608-sample frame, or several of libFLAC's 1152 */
#define W0_FLAC_OPS       ((1u << W0_OP_NOP) | (1u << W0_OP_FLAC_INIT) | (1u << W0_OP_FLAC_DECODE))

/* C_STAGE markers the decoders set (a hang report names the last one) */
#define W0_STAGE_COPY_IN  1
#define W0_STAGE_DECRYPT  2
#define W0_STAGE_SETUP    3
#define W0_STAGE_DECODE   4
#define W0_STAGE_COPY_OUT 5

#endif
