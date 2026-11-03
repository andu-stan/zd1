"""zemu: boot our module OS (or any CR870 image) in the emulator, drive it, trace it.

  zemu setup                          Renode (pinned, into emu/renode/dist) and the test unit's factory images
  zemu run [IMAGE] [options] [steps]  boot IMAGE, do the steps in order, end (or --keep: stay up in real time)

IMAGE: os (default), stock (KnOS), none (no module: the main board alone),
or a file: a .bcd (installed into the factory NAND, booted through BL1), a NAND image, a stage 2 for 0x60008000.

Steps (in order, all times virtual): --until RX, --wait STREAM RX, --sh CMD, --send TEXT, --sleep S, --spi CMDS,
--host CMD. They start once the image is up ([os] up, ...) unless the first one is a wait or --at-once
is given. Output: emu/run/NAME/.
Exit code 1 if a wait timed out, a --sh or --host command failed, or the guest panicked (unless --allow-panic). Nothing here touches hardware.
"""

import argparse
import os
import re
import sys
import time

from . import images
from .paths import EMU, REPO, RUNS

READY = {
    "os": r"^\r?(\[ *[0-9.]+\] )?\[os\] up",
    "stock": r"System initialisation done",
}
RENODE_VERSION = "1.17.0"


def renode_setup():
    """A Renode build for this host in emu/renode/dist (gitignored): the portable release, pinned."""
    import platform
    import subprocess
    import urllib.request

    dist = os.path.join(EMU, "renode", "dist")
    os.makedirs(dist, exist_ok=True)
    base = "https://github.com/renode/renode/releases/download/v%s/" % RENODE_VERSION
    if sys.platform == "darwin":
        if os.path.exists(os.path.join(dist, "Renode.app")):
            return
        dmg = os.path.join(dist, "renode.dmg")
        print("  fetching Renode %s (macOS arm64)" % RENODE_VERSION, flush=True)
        urllib.request.urlretrieve(base + "renode-%s.osx-arm64-portable.dmg" % RENODE_VERSION, dmg)
        mnt = os.path.join(dist, "mnt")
        images.sh(["hdiutil", "attach", "-nobrowse", "-readonly", "-mountpoint", mnt, dmg], stdout=subprocess.DEVNULL)
        try:
            images.sh(["cp", "-R", os.path.join(mnt, "Renode.app"), dist])
        finally:
            images.sh(["hdiutil", "detach", mnt], stdout=subprocess.DEVNULL)
        os.remove(dmg)
        subprocess.run(["xattr", "-dr", "com.apple.quarantine", os.path.join(dist, "Renode.app")])
    else:
        if os.path.exists(os.path.join(dist, "renode", "renode")):
            return
        arch = "linux-arm64-portable" if platform.machine() in ("aarch64", "arm64") else "linux-portable"
        tgz = os.path.join(dist, "renode.tar.gz")
        print("  fetching Renode %s (%s)" % (RENODE_VERSION, arch), flush=True)
        urllib.request.urlretrieve(base + "renode-%s.%s.tar.gz" % (RENODE_VERSION, arch), tgz)
        images.sh(["tar", "-xzf", tgz, "-C", dist])
        os.remove(tgz)
        for d in os.listdir(dist):
            if d.startswith("renode_") or d.startswith("renode-"):
                os.rename(os.path.join(dist, d), os.path.join(dist, "renode"))


class Step(argparse.Action):
    """Steps keep their command-line order."""

    def __call__(self, parser, ns, values, option_string=None):
        ns.steps = (ns.steps or []) + [(self.dest, values)]


def session_config(a):
    unit = images.factory_unit()
    cfg = {
        "keep": a.keep,
        "quiet": a.quiet,
        "quantum_us": a.quantum_us,
        "renode_cmds": a.renode or [],
        "module": None,
        "mcu": None,
        "allow_panic": a.allow_panic,
    }
    if a.image != "none":
        m = images.module_image(a.image, a.nand)
        if a.nand_image:
            m["nand"] = os.path.abspath(a.nand_image)
        m.update(
            bl1_real=a.real_bl1,
            trace=a.trace,
            wav=a.wav,
            patch=a.patch,
            gdb=a.gdb,
            fwd=[tuple(int(x) for x in f.split(":")) for f in a.fwd or []],
        )
        if a.real_bl1 and m["stage2"]:
            sys.exit("--real-bl1 boots from NAND: add --nand")
        cfg["module"] = m
    if a.mcu or a.image == "none":
        cfg["mcu"] = {
            "flash": images.mcu_image(a.mcu or "ours"),
            "gate_open": (a.mcu or "ours") == "ours",
            "user_page": os.path.join(unit, "user_page.bin"),
            "eeprom": os.path.join(unit, "eeprom.bin"),
        }
    return cfg


def run(a):
    import signal

    from .rig import Session, StepFailed

    signal.signal(signal.SIGTERM, signal.default_int_handler)  # `kill` ends a session like Ctrl-C: state saved
    cfg = session_config(a)
    steps = a.steps or []
    if a.image in READY and not a.at_once and (not steps or steps[0][0] not in ("until", "wait")):
        steps = [("until", READY[a.image])] + steps  # boot to the image's "up" line first
    if not steps and not a.keep:
        sys.exit("nothing to do: give steps (--until, --sh, --sleep, ...) or --keep")
    name = a.name or re.sub(r"[^\w.-]+", "_", os.path.basename(a.image))
    rundir = os.path.join(RUNS, name)
    s = Session(cfg, rundir)
    ok = True
    print(
        "== %s: %s%s -> %s" % (name, a.image, " + MCU" if cfg["mcu"] else "", os.path.relpath(rundir, REPO)), flush=True
    )
    try:
        s.start()
        for kind, v in steps:
            try:
                if kind == "until":
                    s.wait(v, "console", a.timeout)
                elif kind == "wait":
                    s.wait(v[1], v[0], a.timeout)
                elif kind == "sleep":
                    s.sleep(float(v))
                elif kind == "send":
                    s.send(v)
                elif kind == "sh":
                    rc, out = s.sh(v, a.timeout)
                    if a.quiet:
                        print("$ %s\n%s" % (v, out), end="" if out.endswith("\n") or not out else "\n")
                    if rc:
                        print("== exit %d: %s" % (rc, v), flush=True)
                        ok = False
                elif kind == "spi":
                    print(s.spi(v), end="", flush=True)
                elif kind == "host":
                    rc = s.host(v, a.timeout)
                    if rc:
                        print("== host command exit %s: %s (output: host.log)" % (rc, v), flush=True)
                        ok = False
            except StepFailed as ex:
                print("== %s" % ex, flush=True)
                ok = False
                break
        if a.keep:
            s.hold()
    except (RuntimeError, TimeoutError) as ex:
        print("== %s" % ex, flush=True)
        ok = False
    except KeyboardInterrupt:
        print("== interrupted", flush=True)
        ok = a.keep and ok
    finally:
        s.close()
    print(
        "== %s: %.1f s virtual in %.0f s; logs in %s"
        % (
            "done" if ok else "FAILED",
            getattr(s, "vt", 0),
            time.time() - getattr(s, "t0", time.time()),
            os.path.relpath(rundir, REPO),
        ),
        flush=True,
    )
    return ok


def main(argv=None):
    ap = argparse.ArgumentParser(prog="zemu", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("setup", help="fetch Renode, build the factory images of the test unit")
    p = sub.add_parser(
        "run",
        help="boot an image and drive it",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        description=__doc__,
    )
    p.add_argument("image", nargs="?", default="os")
    g = p.add_argument_group("boot")
    g.add_argument(
        "--nand",
        action="store_true",
        help="boot from NAND through BL1 and the preloader like the unit "
        "(default: the image's stage 2 straight into SDRAM)",
    )
    g.add_argument("--nand-image", metavar="FILE", help="this NAND instead of the image's (e.g. emu/run/X/nand.bin)")
    g.add_argument("--real-bl1", action="store_true", help="the decrypted vendor BL1 instead of the stand-in (slow)")
    g.add_argument(
        "--mcu",
        nargs="?",
        const="ours",
        metavar="FW",
        help="the main board too, linked over J2: ours (default), stock, or a 256 KB flash image",
    )
    g = p.add_argument_group("debugging")
    g.add_argument(
        "--trace",
        nargs="?",
        const=True,
        metavar="PREFIXES",
        help="module MMIO trace (module.trace), only addresses starting with these hex prefixes "
        "(e.g. b5006,e0010); bare --trace: every access",
    )
    g.add_argument("--gdb", type=int, metavar="PORT", help="GDB server on the module's ARM926")
    g.add_argument("--wav", metavar="FILE", help="record the AV port audio output (in the run directory)")
    g.add_argument("--patch", metavar="ADDR=WORD,...", help="module RAM pokes at the first MMIO write")
    g.add_argument("--fwd", action="append", metavar="HOST:GUEST", help="TCP port forward to the module")
    g.add_argument(
        "--renode",
        action="append",
        metavar="MACHINE:CMD",
        help='a Renode monitor command after setup, e.g. "cr870:cpu LogCpuInterrupts true", '
        '"zd1:board IR \\"2000:0x0c\\""',
    )
    g.add_argument("--quantum-us", type=int)
    g = p.add_argument_group("steps (in order)")
    g.add_argument("--until", action=Step, metavar="RX", help="run until RX shows up on the module console")
    g.add_argument(
        "--wait",
        action=Step,
        nargs=2,
        metavar=("STREAM", "RX"),
        help="... in a stream: console, mcu, log, trace, mcu-trace, host, net, uart0",
    )
    g.add_argument("--sh", action=Step, metavar="CMD", help="a command in the module's shell, wait for it to end")
    g.add_argument("--send", action=Step, metavar="TEXT", help="type TEXT + CR on the module console (raw: \\xNN)")
    g.add_argument("--sleep", action=Step, metavar="SECS")
    g.add_argument("--spi", action=Step, metavar="CMDS", help='the SPI test host (module alone): "rd 0x11 rdv 0xc0"')
    g.add_argument(
        "--host",
        action=Step,
        metavar="CMD",
        help="a host command while the boards run, e.g. "
        '"zd1 module power" (our MCU firmware: the gate opened in the run\'s EEPROM)',
    )
    g = p.add_argument_group("session")
    g.add_argument(
        "--keep",
        action="store_true",
        help="after the steps, stay up in real time: module console on "
        "127.0.0.1:4555, MCU USB 4620, ports 8080->80, 2323->23, 2324->2323",
    )
    g.add_argument(
        "--at-once",
        action="store_true",
        help='start the steps at power-up (default: after the image\'s "up" line, unless the first step is a wait)',
    )
    g.add_argument("--timeout", type=float, default=300, help="per step, virtual seconds (300)")
    g.add_argument(
        "--allow-panic",
        action="store_true",
        help="a kernel panic doesn't end the run (the module "
        "restarts: panic=N, or the MCU power-cycles a boot that doesn't get ready)",
    )
    g.add_argument("--name", help="run directory emu/run/NAME (default: the image name)")
    g.add_argument("-q", "--quiet", action="store_true", help="no live console; print --sh output instead")
    p.set_defaults(steps=None)
    a = ap.parse_args(argv)
    if a.cmd == "setup":
        renode_setup()
        images.factory_unit()
        images.stock_stage2()
    elif a.cmd == "run":
        sys.exit(0 if run(a) else 1)


if __name__ == "__main__":
    main()
