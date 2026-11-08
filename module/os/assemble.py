#!/usr/bin/env python3
"""The module OS images (our userspace on Nokia's 2.6.24 kernel) from module/os/os.toml and the compiled parts. Nothing
here touches hardware.

    nix build -o emu/img/os .#os-image          the images (nix/os-image.nix runs `assemble.py assemble` on
                                                `nix build .#module-os-parts` and the Nokia kernel and modules in
                                                blobs/); `zemu run` builds them when it needs them
    python3 module/os/assemble.py mini TAG      the minimal test image printing TAG: emu/img/mini-TAG.bin

Nix builds the files git knows: `git add` a new source file first. No image carries a Wi-Fi network: the speaker's
settings do (`zd1 wifi`, docs/module/settings.md).

The images (emu/img/os/; docs/module/storage.md):
    system.cpio.gz          a system slot's root (busybox, the full zd1, /etc, Nokia's modules and firmware, ALSA,
                            Wi-Fi): what `zd1 update` installs (deterministic: sorted, root-owned, mtime 0)
    system-debug.cpio.gz    the same with zd1's debug build (os.toml [image.debug])
    rescue.cpio(.gz)        the rescue, the app partition's initramfs: its busybox, the core programs, /init
    payload_dev.bin         emulator dev boot (`zemu run os`): the debug system as one plain initramfs at
                            0x61000000, zd1.selftest zd1.debug
    payload.bin, os.bcd     the flashable app image (stock preloader + our stub/kernel/gzip rescue; the relay, BL2):
                            the system goes into a slot with `zd1 update`
    nand_os.bin             the emulator's test unit with os.bcd installed, slots empty (`zemu run os --nand --mcu`:
                            the rescue; then `--host 'zd1 update --uart'`)
    os-debug.bcd            os.bcd with the debug services on by default (os.toml [image.debug]), for the unit
    os-zd1l.bcd             (with --zd1l-fw) the Zeppelin Air with Lightning, B&W's MCU firmware kept ("stock-MCU
                            mode", zd1.mcu=stock): wrapped like the Lightning's stock app (HWID 0xBE, its CnE and
                            resources) for BL2's web updater, the system inside the initramfs (the rescue installs it
                            into slot A and switches to it: zd1-init, stock.rs)
    nand_os_zd1l.bin        the test unit with os-zd1l.bcd installed, slots empty (the emulator, --mcu with the
                            Lightning's MCU app)
"""

import argparse
import gzip
import hashlib
import os
import struct
import shutil
import subprocess
import sys
import tempfile
import tomllib

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, os.path.join(REPO, "tools"))
from zeppelin.fw import bcd, mkbcd  # noqa: E402

IMG = os.path.join(REPO, "emu", "img")
PY = [sys.executable, "-I"]


def log(msg):
    print(msg, flush=True)


def sh(cmd, **kw):
    log("  $ " + " ".join(cmd))
    return subprocess.run(cmd, cwd=REPO, check=True, **kw)


def copy_tree(src, dst):
    """copytree out of the nix store, made writable (the store is read-only)."""
    shutil.copytree(src, dst, symlinks=True, dirs_exist_ok=True)
    for dp, dns, fs in os.walk(dst):
        for n in dns + fs:
            q = os.path.join(dp, n)
            if not os.path.islink(q):
                os.chmod(q, os.stat(q).st_mode | 0o200)
    os.chmod(dst, os.stat(dst).st_mode | 0o200)


# ---- assembly ----------------------------------------------------------------------------------------------------
def os_release(version, built):
    """The system's identification, /usr/lib/os-release (init adds the variant: the image's command line decides; the
    rescue has its own, make_rescue)."""
    return 'NAME="zd1 module OS"\nID=zd1\nVERSION_ID="%s"\nBUILD_DATE="%s"\n' % (version, built)


def content_id(path):
    """12 hex digits of what Nix hashes a path by (its NAR's SHA-256: every name, kind, mode and content): an
    identity that changes only when what is in it does."""
    out = subprocess.run(["nix-hash", "--type", "sha256", path], check=True, capture_output=True, text=True).stdout
    return out.strip()[:12]


def link_programs(root, target, names):
    """A link to the multicall per program in root/bin, `target` relative to it (busybox's /sbin is a link to /bin:
    /sbin/init, the kernel's choice, is /bin/init). Relative, so a root mounted elsewhere resolves into itself:
    zd1-slot checks a new slot's /sbin/init that way."""
    for b in names:
        os.symlink(target, os.path.join(root, "bin", b))


def make_rescue(cfg, parts, root):
    """The rescue, the app partition's initramfs (docs/module/storage.md): the rescue's busybox, the core build of zd1
    (init, the MCU link, settings, the slot writer, the system image's reader); /init is init (its rescue profile:
    /etc/os-release says VARIANT_ID=rescue). Its version is its content (content_id), not the commit or the build date:
    the core build holds only what the rescue runs, so a commit that doesn't change that builds the same rescue, and
    the app partition only needs rewriting when it does."""
    a = cfg["apps"]
    copy_tree(os.path.join(parts, "busybox-rescue"), root)
    for d in ("dev", "proc", "sys", "tmp", "var", "run", "data", "etc"):
        os.makedirs(os.path.join(root, d), exist_ok=True)
    mc = os.path.join(root, "bin", a["multicall"])
    shutil.copyfile(os.path.join(parts, "apps-core", "bin", a["multicall"]), mc)
    os.chmod(mc, 0o755)
    link_programs(root, a["multicall"], a["rescue"])
    os.symlink("bin/" + a["multicall"], os.path.join(root, "init"))
    open(os.path.join(root, "etc/os-release"), "w").write(
        'NAME="zd1 module OS"\nID=zd1\nVERSION_ID="%s"\nVARIANT_ID=rescue\n' % content_id(root)
    )


def make_system(cfg, parts, root, version, built, debug=False):
    """A system slot's tree, the whole root the kernel mounts (read-only): busybox, the full zd1 (/usr/bin/zd1: the
    core programs and the rest, a link each; /sbin/init), /etc, Nokia's kernel modules and Wi-Fi firmware,
    wpa_supplicant, ALSA's configuration and tools; mount points for what init mounts (/dev, /run, /tmp, /var: tmpfs;
    /data its partition). `debug`: zd1's debug build, with its programs' links."""
    v, a = cfg["vendor"], cfg["apps"]
    vroot = os.path.join(REPO, v["root"])
    copy_tree(os.path.join(parts, "busybox"), root)
    for d in (
        "dev",
        "proc",
        "sys",
        "tmp",
        "var",
        "mnt",
        "root",
        "run",
        "data",
        "etc",
        "lib",
        "usr/bin",
        "usr/sbin",
        "usr/share/zd1",
        "usr/lib",
    ):
        os.makedirs(os.path.join(root, d), exist_ok=True)
    usr = os.path.join(root, "usr")
    for d in ("modules", "firmware"):
        os.symlink("/usr/lib/" + d, os.path.join(root, "lib", d))
    kver = "2.6.24.7"
    for m in v["modules"]:
        dst = os.path.join(usr, "lib/modules", kver, "kernel", m + ".ko")
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.copyfile(os.path.join(vroot, "lib/modules", kver, "kernel", m + ".ko"), dst)
    for f in v["firmware"]:  # lib/firmware/... -> usr/lib/firmware/...
        src, dst = os.path.join(vroot, f), os.path.join(usr, f)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        if os.path.islink(src):
            os.symlink(os.readlink(src), dst)
        else:
            shutil.copyfile(src, dst)
    copy_tree(os.path.join(parts, "alsa-lib", "share", "alsa"), os.path.join(usr, "share/alsa"))  # alsa.conf
    copy_tree(os.path.join(parts, "alsa-utils", "bin"), os.path.join(usr, "bin"))  # aplay, ...
    copy_tree(os.path.join(parts, "wpa_supplicant", "bin"), os.path.join(usr, "sbin"))  # wpa_supplicant, wpa_cli
    mc = os.path.join(usr, "bin", a["multicall"])
    shutil.copyfile(os.path.join(parts, "apps-debug" if debug else "apps", "bin", a["multicall"]), mc)
    os.chmod(mc, 0o755)
    link_programs(root, "../usr/bin/" + a["multicall"], a["core"])
    link_programs(usr, a["multicall"], a["bins"] + (a["debug"] if debug else []))
    shutil.copytree(os.path.join(REPO, cfg["image"]["overlay"]), root, symlinks=True, dirs_exist_ok=True)
    open(os.path.join(root, "etc/modules"), "w").write("\n".join(v["load"]) + "\n")
    open(os.path.join(usr, "lib/os-release"), "w").write(os_release(version, built))
    for f in ("os-release", "resolv.conf"):  # written at run time: / is read-only
        os.symlink("/run/" + f, os.path.join(root, "etc", f))


def cpio(root, out, name, slot=False):
    """`root` as out/NAME.cpio and out/NAME.cpio.gz (slot: a system slot's root, /dev/console and /dev/null its only
    device nodes)."""
    c = os.path.join(out, name + ".cpio")
    sh(PY + [os.path.join(HERE, "mkcpio.py")] + (["--slot"] if slot else []) + [root, c])
    raw = open(c, "rb").read()
    open(c + ".gz", "wb").write(gzip.compress(raw, 9, mtime=0))
    log("  %s %d B (%d B gzip)" % (name, len(raw), os.path.getsize(c + ".gz")))


def payload(cfg, parts, initrd, out, cmdline_extra="", dev=False, initrd_at=None):
    """The stub + kernel + initramfs as one app payload (mkpayload.py): the rescue's, the stub starting
    a system slot unless the MCU says otherwise (--slots); dev = the emulator's direct boot, the initramfs the system.
    The kernel goes in deflated (half the size): the stub inflates it. The command line ends in `zd1.boot=ID`, the boot
    image's identity: the content_id of what goes in (stub, kernel, initramfs, how, the command line before it), so a
    commit that changes none of them builds the same one. The OS reports it (the State record's `boot`), `zd1 image`
    reads it from a .bcd, and the page rewrites the app partition only when they differ. `initrd_at`: a lower home for
    a big initramfs (the stub's default 0x61800000 leaves 7 MB below it)."""
    k = cfg["kernel"]
    args = (["--initrd-at", "0x61000000"] if dev else ["--slots"]) + ["--deflate-kernel"]
    if initrd_at is not None and not dev:
        args += ["--initrd-at", "0x%08x" % initrd_at]
    files = [os.path.join(parts, "boot", "linuxboot.bin"), os.path.join(REPO, k["image"]), initrd]
    cmdline = (k["cmdline"] + " " + cmdline_extra).strip()
    with tempfile.TemporaryDirectory() as d:
        for n, p in zip(("stub", "kernel", "initramfs"), files):
            shutil.copyfile(p, os.path.join(d, n))
        open(os.path.join(d, "how"), "w").write(" ".join(args) + "\n" + cmdline + "\n")
        cmdline += " zd1.boot=" + content_id(d)
    sh(PY + [os.path.join(HERE, "mkpayload.py")] + files + [out] + args + ["--cmdline", cmdline])


def wrap(payload_bin, out):
    """The payload wrapped like the stock app (stock preloader, CnE, resources) for the relay and BL2. The system
    isn't in it: `zd1 update` puts it into a slot (docs/module/updater.md)."""
    stock = open(os.path.join(REPO, "blobs/module/app_DMP.bcd"), "rb").read()
    open(out, "wb").write(mkbcd.build(stock, open(payload_bin, "rb").read()))
    log("  %s %d B" % (os.path.basename(out), os.path.getsize(out)))


ZD1L_CMDLINE = "zd1.mcu=stock"  # stock-MCU mode: init (stock.rs), zd1-slot, zd1-linkd read it
ZD1L_INITRD_AT = 0x61000000  # the rescue with the system (~6 MB) doesn't fit below the stub at the default


def zd1l_template(fw_path):
    """The Lightning model's stock module image out of B&W's OTA `.fw` (zd1lv20435.fw: BridgeCo's wrapper, 32 bytes:
    version 2, the .bcd's length, encryption mode 0, the MCU part's length; then the .bcd, then the MCU part;
    docs/mcu/stock-firmware.md). Its header's HWID is 0xBE, the Lightning's, which BL2's web updater checks against
    PersParam (docs/module/boot-chain.md)."""
    f = open(fw_path, "rb").read()
    ver, mlen, enc = struct.unpack_from("<3I", f, 0)
    if ver != 2 or enc != 0 or f[32:36] != b"bCoD":
        raise SystemExit("%s: not an unencrypted B&W .fw with a .bcd inside" % fw_path)
    t = f[32 : 32 + mlen]
    hwid = bcd.parse(t)[0]["hwid"] & 0xFFFF
    if hwid != 0xBE:
        raise SystemExit("%s: HWID 0x%x, not the Lightning's 0xBE" % (fw_path, hwid))
    return t


def lightning(cfg, parts, out, fw_path, base_nand):
    """os-zd1l.bcd: the Zeppelin Air with Lightning, B&W's MCU firmware kept (docs/stock-mcu.md). Its MCU bootloader
    takes no image over USB, so the module is installed through BL2's web updater alone, and the system comes along:
    the initramfs is the rescue plus system.cpio.gz and its stamp (system.id). On a stock MCU the stub gets no slot
    answer and boots the rescue; with zd1.mcu=stock the rescue installs the system into slot A when slot A holds
    another one, then switches its root to slot A (zd1-init, stock.rs). The initramfs goes in uncompressed: the system
    image is gzip already."""
    log("rescue-zd1l")
    root = os.path.join(out, ".rescue-zd1l")
    make_rescue(cfg, parts, root)
    system = os.path.join(out, "system.cpio.gz")
    shutil.copyfile(system, os.path.join(root, "system.cpio.gz"))
    stamp = "zd1l-" + hashlib.sha256(open(system, "rb").read()).hexdigest()[:16]
    open(os.path.join(root, "system.id"), "w").write(stamp + "\n")
    cpio(root, out, "rescue-zd1l")
    shutil.rmtree(root)
    log("  system stamp %s" % stamp)
    p = os.path.join(out, "payload_zd1l.bin")
    payload(cfg, parts, os.path.join(out, "rescue-zd1l.cpio"), p, ZD1L_CMDLINE, initrd_at=ZD1L_INITRD_AT)
    image = os.path.join(out, "os-zd1l.bcd")
    open(image, "wb").write(mkbcd.build(zd1l_template(fw_path), open(p, "rb").read()))
    log("  os-zd1l.bcd %d B" % os.path.getsize(image))
    install(base_nand, image, os.path.join(out, "nand_os_zd1l.bin"))


def install(base_nand, image, out):
    """The test unit's NAND with `image` installed the way BL2 leaves it."""
    nand = bytearray(open(base_nand, "rb").read())
    bcd.install(nand, open(image, "rb").read())
    open(out, "wb").write(nand)


def assemble(cfg, parts, out, version, built, base_nand, zd1l_fw=None):
    """Every image of os.toml into `out` (the nix build runs this); with `zd1l_fw` (B&W's Lightning .fw) the
    Lightning's stock-MCU image too."""
    os.makedirs(out, exist_ok=True)
    log("system")
    sysroot = os.path.join(out, ".system")
    make_system(cfg, parts, sysroot, version, built)
    cpio(sysroot, out, "system", slot=True)
    shutil.rmtree(sysroot)
    log("system-debug")
    make_system(cfg, parts, sysroot, version, built, debug=True)
    cpio(sysroot, out, "system-debug", slot=True)
    log("rescue")
    tmp = os.path.join(out, ".rootfs")
    make_rescue(cfg, parts, tmp)
    cpio(tmp, out, "rescue")
    shutil.rmtree(tmp)
    img = cfg["image"]
    variants = [("", "")]
    if img.get("debug"):  # the debug image: its debug services on by default
        variants.append(("-debug", img["debug"]["cmdline"]))
    for suffix, cmdline in variants:
        p = os.path.join(out, "payload%s.bin" % suffix.replace("-", "_"))
        payload(cfg, parts, os.path.join(out, "rescue.cpio.gz"), p, cmdline)
        wrap(p, os.path.join(out, "os%s.bcd" % suffix))
    install(base_nand, os.path.join(out, "os.bcd"), os.path.join(out, "nand_os.bin"))
    if zd1l_fw:
        lightning(cfg, parts, out, zd1l_fw, base_nand)
    # the emulator's dev boot: the debug system as one plain initramfs loaded straight into SDRAM, its init as /init
    log("initramfs-dev")
    os.symlink("sbin/init", os.path.join(sysroot, "init"))
    cpio(sysroot, out, "initramfs-dev")
    shutil.rmtree(sysroot)
    payload(
        cfg,
        parts,
        os.path.join(out, "initramfs-dev.cpio"),
        os.path.join(out, "payload_dev.bin"),
        "zd1.selftest zd1.debug",
        dev=True,
    )


def mini(tag):
    """The mini image printing TAG at boot (tells two flashed builds apart): emu/img/mini-TAG.bin."""
    expr = (
        "let e = builtins.getEnv; in "
        '(builtins.getFlake (e "ZD1_FLAKE")).packages.${builtins.currentSystem}.boot.override { tag = e "ZD1_TAG"; }'
    )
    res = subprocess.run(
        ["nix", "build", "--impure", "--no-link", "--print-out-paths", "--expr", expr],
        cwd=REPO,
        check=True,
        capture_output=True,
        text=True,
        env=dict(os.environ, ZD1_FLAKE="git+file://" + REPO, ZD1_TAG=tag),
    ).stdout.strip()
    dst = os.path.join(IMG, "mini-%s.bin" % tag)
    os.makedirs(IMG, exist_ok=True)
    if os.path.exists(dst):
        os.remove(dst)
    shutil.copyfile(os.path.join(res, "mini-%s.bin" % tag), dst)
    return dst


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("cmd", nargs="+", help="mini TAG | assemble (the nix build)")
    ap.add_argument("--parts", help="assemble: the compiled parts (module-os-parts)")
    ap.add_argument("--base-nand", help="assemble: the NAND to install the images into (the test unit)")
    ap.add_argument("--version", help="assemble: the version in /etc/os-release")
    ap.add_argument("--built", default="", help="assemble: the build date in /etc/os-release")
    ap.add_argument("--out", help="assemble: where the images go")
    ap.add_argument("--zd1l-fw", help="assemble: B&W's Lightning .fw (zd1lv20435.fw): also os-zd1l.bcd")
    a = ap.parse_args()
    cfg = tomllib.load(open(os.path.join(HERE, "os.toml"), "rb"))
    if a.cmd[:1] == ["assemble"]:
        assemble(cfg, a.parts, a.out, a.version, a.built, a.base_nand, a.zd1l_fw)
    elif a.cmd[:1] == ["mini"] and len(a.cmd) == 2:
        log(os.path.relpath(mini(a.cmd[1]), REPO))
    else:
        ap.error("unknown command: %s" % " ".join(a.cmd))


if __name__ == "__main__":
    main()
