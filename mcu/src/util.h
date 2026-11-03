/* Small helpers more than one module uses. */
#ifndef UTIL_H
#define UTIL_H
#include <stdint.h>

/* src's first n bytes into d (cap bytes, the NUL included), stopping at a NUL in src; always terminated */
static inline void copy_str(char *d, unsigned cap, const void *src, unsigned n) {
    const uint8_t *s = src;
    unsigned i = 0;
    for (; i < n && i + 1 < cap && s[i]; i++)
        d[i] = (char)s[i];
    d[i] = 0;
}
#define STR_ALL (~0u) /* copy_str's n for a NUL-terminated src */

#endif
