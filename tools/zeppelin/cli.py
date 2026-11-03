"""zeppelin: tools for the B&W Zeppelin Air and the BridgeCo CR870 module. `zeppelin <tool> --help` for each.

Each tool is also a plain script (python3 -I tools/zeppelin/<path>.py ...); this only dispatches by name.
Tools marked [hw] talk to real hardware; everything else works on files.

The speaker over USB (our MCU firmware's host link: state, controls, settings, updates of both boards, DFU) is `zd1`
(host/cli, on the PATH in `nix develop`): `zd1 --help`. The same flows run in the browser: `zeppelin web`.
"""

import os
import runpy
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))

TOOLS = [  # (name, script relative to the repo, summary)
    ("The speaker on USB (beside `zd1`)", None, None),
    (
        "web",
        "tools/zeppelin/web.py",
        "the web page (updates and settings over WebUSB) on localhost, with this build's images",
    ),
    ("MCU (main board)", None, None),
    ("stock-patch", "stock-patch/patch.py", "the stock MCU firmware patched to put the module's KnOS shell on telnet"),
    ("Network module (CR870 / KnOS)", None, None),
    ("knos", "tools/zeppelin/knos/knos.py", "[hw] commands on the KnOS shell over telnet (the client every tool uses)"),
    ("fburn", "tools/zeppelin/knos/fburn.py", "[hw] NAND over the shell: rd, wr (refuses BL1; BL2 needs a flag)"),
    ("bl2web", "tools/zeppelin/knos/bl2web.py", "[hw] install a .bcd through BL2's web updater"),
    ("kmsg", "tools/zeppelin/jtag/kmsg.py", "[hw] the running kernel's log over JTAG (J-Link, OpenOCD)"),
    ("kallsyms", "tools/zeppelin/jtag/kallsyms.py", "the Nokia kernel's symbols, from kernel.bin's own tables"),
    (
        "warprun",
        "tools/zeppelin/warp/warprun.py",
        "[hw] jobs on a Warp core through the shell (runner check, a Sol op)",
    ),
    ("warpblob", "tools/zeppelin/warp/warpblob.py", "Nokia's Warp1 program out of their app's ELF"),
    ("Images and formats", None, None),
    ("bcd", "tools/zeppelin/fw/bcd.py", "a .bcd module image: header and sections checked, extracted"),
    ("mkbcd", "tools/zeppelin/fw/mkbcd.py", "a custom stage 2 wrapped into a .bcd (stock preloader, CnE, resources)"),
    ("nand", "tools/zeppelin/fw/nand_parse.py", "a NAND image: its MetaInfo partitions and PersParam (MAC masked)"),
    ("ecc", "tools/zeppelin/nand/ecc.py", "the NAND's ECC checked on raw pages (data + spare)"),
    ("cne", "tools/zeppelin/fw/cne2txt.py", "the factory CnE settings as text"),
    ("cneu", "tools/zeppelin/fw/cneu_dump.py", "the user CnE settings, secrets redacted"),
    ("ffsres", "tools/zeppelin/fw/ffsres_extract.py", "the resources partition's files (the stock web UI)"),
    ("Recovery", None, None),
    ("mkunit", "tools/zeppelin/recover/mkunit.py", "factory images of one unit from its MAC (both boards)"),
    ("persparam", "tools/zeppelin/recover/persparam.py", "the module's PersParam block: parse, check, build"),
    ("Downloads", None, None),
    ("downloads", "tools/zeppelin/downloads.py", "list/fetch/verify the vendor files kept on archive.org"),
    ("Lab (checks without someone at the speaker)", None, None),
    (
        "usprobe",
        "tools/zeppelin/lab/usprobe.py",
        '[hw] inaudible 19 kHz "does the speaker play?" probe; gen|listen: over AirPlay',
    ),
    (
        "musictest",
        "tools/zeppelin/lab/musictest.py",
        "[hw] AirPlay from the Music app: sync with the Mac, controls, stress",
    ),
    (
        "synctest",
        "tools/zeppelin/lab/synctest.py",
        "[hw] AirPlay 2 playback time against the Mac's clock (ap2send, chirps)",
    ),
    ("raop_send", "tools/zeppelin/lab/raop_send.py", "[hw] a WAV over AirPlay 1 (RAOP), as iTunes sent it"),
    ("netprobe", "tools/zeppelin/lab/netprobe.py", "[hw] UDP loss on the module's links, with its own counters"),
    ("udpprobe", "tools/zeppelin/lab/udpprobe.py", "a paced UDP stream and what arrived (netprobe's Mac side)"),
]


def usage():
    print(__doc__)
    for name, script, summary in TOOLS:
        print("\n%s" % name if script is None else "  %-15s %s" % (name, summary))
    print("\nEmulator: python3 emu/zemu --help")


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if not argv or argv[0] in ("-h", "--help", "help"):
        return usage()
    table = {n: s for n, s, _ in TOOLS if s}
    if argv[0] not in table:
        sys.exit("unknown tool %r (zeppelin --help lists them)" % argv[0])
    script = os.path.join(REPO, table[argv[0]])
    sys.argv = [script] + argv[1:]
    sys.path.insert(0, os.path.dirname(script))
    runpy.run_path(script, run_name="__main__")


if __name__ == "__main__":
    main()
