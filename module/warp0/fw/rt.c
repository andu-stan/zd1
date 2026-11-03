/* Warp0 firmware runtime (rt.h): the job loop every image runs.
 *
 * Idle: one ring poll and a ~5 us register loop between polls, so an idle Warp0 costs the bus ~200 k loads/s. */
#include "rt.h"

static u32 data_end;

u32 w0_check(u32 addr, u32 len) {
    if (addr & 3)
        return W0_EALIGN;
    if (addr < W0_DATA || addr >= data_end || len > data_end - addr)
        return W0_ERANGE;
    return W0_OK;
}

void w0_serve(const struct w0_image *img) {
    volatile u32 *ctrl = (volatile u32 *)W0_CTRL;
    u32 beat = 0, done = 0, refused = 0, i = 0;
    data_end = img->data_end;
    if (img->init)
        img->init();
    ctrl[C_VERSION] = W0_VERSION;
    ctrl[C_OPS] = img->ops;
    ctrl[C_IMAGE] = img->id;
    ctrl[C_DATA_END] = img->data_end;
    ctrl[C_DONE] = 0;
    ctrl[C_HEAD] = 0;
    ctrl[C_JOB] = W0_NOJOB;
    ctrl[C_STAGE] = 0;
    ctrl[C_REFUSED] = 0;
    ctrl[C_STATE] = W0_ST_IDLE;
    ctrl[C_MAGIC] = W0_MAGIC;
    for (;;) {
        volatile u32 *d = (volatile u32 *)(W0_RING + i * D_WORDS * 4);
        if (d[D_STATE] == W0_SUBMIT) {
            u32 op = d[D_OP], r;
            ctrl[C_STATE] = W0_ST_BUSY;
            ctrl[C_JOB] = i << 16 | (op & 0xFFFF);
            ctrl[C_STAGE] = 0;
            r = op < 32 && (img->ops >> op & 1) ? img->run(d) : W0_EOP;
            if (r == W0_EALIGN || r == W0_ERANGE)
                ctrl[C_REFUSED] = ++refused;
            d[D_RESULT] = r;
            d[D_STATE] = W0_DONE;
            i = (i + 1) & (W0_RING_N - 1);
            ctrl[C_HEAD] = i;
            ctrl[C_DONE] = ++done;
            ctrl[C_JOB] = W0_NOJOB;
            ctrl[C_STATE] = W0_ST_IDLE;
            continue;
        }
        if (ctrl[C_CMD] == W0_CMD_STOP)
            break;
        ctrl[C_BEAT] = ++beat;
        for (u32 n = 256; n; n--)
            __asm__ volatile(""); /* ~5 us in registers */
    }
    ctrl[C_STATE] = W0_ST_STOPPED;
    for (;;)
        __asm__ volatile("");
}
