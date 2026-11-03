/* Warp0 firmware runtime (rt.c), shared by the images: the job loop over the ring (w0abi.h), the checks every op
 * makes on the pointers it is given, and the progress markers a hang report reads.
 *
 * The core (docs/module/warp.md section 3): MIPS I, no divider (no `/` or `%`), mult/multu (result after ~4
 * cycles), no data cache, no exceptions: an unaligned store drops the low address bits, an access to an unmapped
 * place hangs the bus. So nothing here trusts a pointer it was handed. */
#ifndef W0_RT_H
#define W0_RT_H
#include "../w0abi.h"

typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned char u8;
typedef unsigned long long u64;

#define V32(a) (*(volatile u32 *)(a))

struct w0_image {
    u32 id;                      /* W0_IMAGE_* */
    u32 ops;                     /* C_OPS */
    u32 data_end;                /* C_DATA_END */
    void (*init)(void);          /* once, before the ring is served (tables), or 0 */
    u32 (*run)(volatile u32 *d); /* one job: its W0_* result */
};

/* Serve the ring until the ARM says stop; never returns */
void w0_serve(const struct w0_image *img) __attribute__((noreturn));

/* Is [addr, addr + len) a buffer the ARM gave us: word aligned, inside [W0_DATA, data_end)? len may be 0. */
u32 w0_check(u32 addr, u32 len);

/* the W0_EALIGN / W0_ERANGE a bad pointer gets (0 if it is fine) */
#define W0_CHECK(addr, len)                                                                                            \
    do {                                                                                                               \
        u32 e_ = w0_check((addr), (len));                                                                              \
        if (e_)                                                                                                        \
            return e_;                                                                                                 \
    } while (0)

/* how far the running job got (C_STAGE): a store to SDRAM, ~7 cycles */
static inline void w0_stage(u32 s) { V32(W0_CTRL + 4 * C_STAGE) = s; }

static inline void copy_words(u32 *dst, const u32 *src, u32 nwords) {
    while (nwords--)
        *dst++ = *src++;
}

#endif
