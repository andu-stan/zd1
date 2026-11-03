/* The firmware's identification. Compiled as "unknown"; the nix build stamps the commit and its date into this block
 * after linking (mcu/stamp.py finds it by its magic, in the .elf, .bin and .hex alike), so a new commit doesn't
 * rebuild the firmware. Only this file sees the initialiser: the others read the strings through the pointers, so
 * nothing folds "unknown" into their code. */
#include "hostif.h"

typedef struct {
    char magic[16];
    char version[41];
    char built[25];
} ident_t;

__attribute__((used)) static const ident_t ident = {"ZD1-FW-IDENT-01", "unknown", ""};

const char *const g_fw_version = ident.version;
const char *const g_fw_built = ident.built;
#ifdef ZD1_EMU
const char *const g_fw_board = "zd1-emu";
#else
const char *const g_fw_board = "zd1";
#endif
