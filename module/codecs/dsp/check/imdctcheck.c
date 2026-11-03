/* imdctcheck: the fixed-point IMDCT against the standard's formula in double precision: worst errors and where.
 *   cc -O1 -I. -o /tmp/imdctcheck check/imdctcheck.c imdct.c imdct_tables.c -lm && /tmp/imdctcheck */
#include "imdct.h"
#include <stdint.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>

static int32_t Z[1024];

static void run(const char *name, int n, double (*gen)(int k, int n2), double scale) {
    int n2 = n / 2;
    static int32_t x[1024], out[2048];
    static double ref[2048];
    for (int k = 0; k < n2; k++)
        x[k] = (int32_t)lrint(gen(k, n2) * scale * 4); /* Q2 */
    imdct(Z, x, n, out);
    double n0 = (n / 2.0 + 1) / 2, maxe = 0;
    int at = 0;
    for (int i = 0; i < n; i++) {
        double s = 0;
        for (int k = 0; k < n2; k++)
            s += (x[k] / 4.0) * cos(2 * M_PI / n * (i + n0) * (k + 0.5));
        ref[i] = 2.0 / n * s * 512; /* PCM Q9 */
        double e = fabs(out[i] - ref[i]);
        if (e > maxe) {
            maxe = e;
            at = i;
        }
    }
    printf("%-28s N=%4d max error %.2f LSB at %d (ref %.1f, got %.1f LSB)\n", name, n, maxe / 512, at, ref[at] / 512,
           out[at] / 512.0);
}

static double low(int k, int n2) {
    (void)n2;
    return k < 6 ? (k & 1 ? -1.0 : 1.0) : 0;
}
static double rnd(int k, int n2) {
    (void)k;
    (void)n2;
    return rand() / (double)RAND_MAX - 0.5;
}
static double dc(int k, int n2) {
    (void)n2;
    return k == 0 ? 1.0 : 0;
}

int main(void) {
    run("low bins, loud", 2048, low, 2.5e7);
    run("bin 0 only, loud", 2048, dc, 3e7);
    run("random, moderate", 2048, rnd, 1e5);
    run("random, loud", 2048, rnd, 2e6);
    run("low bins, loud", 256, low, 3e6);
    run("random, short", 256, rnd, 1e5);
    return 0;
}
