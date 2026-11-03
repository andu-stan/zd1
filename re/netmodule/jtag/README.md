# Register dumps over JTAG (unit, 2026-10-23)

`mdw phys` dumps of the DM870's peripheral window with our Linux (os-debug image) running and with BL2 running, same
power cycle, same ranges: `regs-*.txt` (0xE0007000, 0xE0008000-0xE000CFFF, 0xB1002000, 0xFFFF0400, 0xE0010000),
`regs2-*.txt` (0xE0000000-0xE0006FFF, 0xE000D000-0xE000FFFF, 0xE0011000-0xE0016FFF, 0xE0018000-0xE001FFFF,
0xB1000000-0xB1001FFF, 0xB1003000-0xB1007FFF, 0xFFFF0000-0xFFFF007F, 0xFFFF0100-0xFFFF03FF, 0xFFFF0500-0xFFFF0FFF),
`emac-*.txt` (0xE0017000). The diff and what it meant: `docs/module/jtag.md`, `docs/module/memory-map.md`.
No per-unit data: the UART FIFO words and 0xE0011xxx differ between runs (scratch), the MAC address lives elsewhere.
