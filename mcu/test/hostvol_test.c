/* hostvol.h: the computer's -60..+10 dB and the speaker's 1..cap, end to end, both ways, for every cap. */
#include <stdio.h>
#include <stdlib.h>
#include "hostvol.h"

static int failed;
#define CHECK(c, ...)                                                                                                  \
    do {                                                                                                               \
        if (!(c)) {                                                                                                    \
            printf(__VA_ARGS__);                                                                                       \
            putchar('\n');                                                                                             \
            failed = 1;                                                                                                \
        }                                                                                                              \
    } while (0)

int main(void) {
    for (int cap = 1; cap <= 71; cap++) {
        CHECK(hostvol_step(HOSTVOL_DB_MIN, (uint8_t)cap) == 1, "cap %d: the bottom isn't step 1", cap);
        CHECK(hostvol_step(HOSTVOL_DB_MAX, (uint8_t)cap) == cap, "cap %d: the top isn't the cap", cap);
        int last = 0;
        for (int db = HOSTVOL_DB_MIN; db <= HOSTVOL_DB_MAX; db++) {
            int s = hostvol_step(db, (uint8_t)cap);
            CHECK(s >= last && s >= 1 && s <= cap, "cap %d: %d dB gives step %d after %d", cap, db, s, last);
            last = s;
        }
        for (int s = 1; s <= cap; s++)
            CHECK(hostvol_step(hostvol_db((uint8_t)s, (uint8_t)cap), (uint8_t)cap) == s,
                  "cap %d: step %d reads back as %d", cap, s,
                  hostvol_step(hostvol_db((uint8_t)s, (uint8_t)cap), (uint8_t)cap));
    }
    /* no cap (71): one dB per step, as before the cap */
    for (int s = 1; s <= 71; s++)
        CHECK(hostvol_db((uint8_t)s, 71) == s - 61, "cap 71: step %d is %d dB", s, hostvol_db((uint8_t)s, 71));
    puts(failed ? "hostvol: FAILED" : "hostvol: ok");
    return failed ? EXIT_FAILURE : EXIT_SUCCESS;
}
