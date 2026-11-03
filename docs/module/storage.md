# Storage: the module OS on the NAND

How our module OS keeps its programs and its settings in the module's NAND, so only what it needs to start lives in
RAM. Tags: `docs/README.md`. The NAND itself (layout, ECC, the vendor's partitions): `boot-chain.md`.

## Partitions
The kernel command line (`module/os/os.toml`) cuts the chip into four MTD partitions (`mtdparts=sulu-nand:...`, the
Nokia HD-1's own syntax for this driver, `re/netmodule/NOKIA_HD1.md`):

| | NAND | Name | What |
|---|---|---|---|
| mtd0 | 0 - 32 MB | `vendor`, **read-only** | BL1, PersParam, CnE, the user settings, BL2, the app partition (our boot stub, kernel and rescue), resources: the vendor's addresses are logical (bad blocks skipped), so the partition leaves 16 MB for bad blocks behind them; also 0x1300000, the stock app's `CnET` |
| mtd1 | 32 - 64 MB | `system-a` | system slot A (JFFS2: a whole root, the kernel's root file system, read-only) |
| mtd2 | 64 - 96 MB | `system-b` | system slot B |
| mtd3 | 96 - 128 MB | `data` | our data (JFFS2, read-write at `/data`): the settings (`settings.md`) |

The vendor partition is read-only to the kernel: our OS can't write BL1, BL2 or PersParam even by mistake. It is read
raw (`zd1-persparam`): the kernel's soft ECC miscorrects the vendor's pages (`boot-chain.md`, NAND).
The slots and `data` are Linux's own: written through MTD with the kernel's ECC (spare bytes 40-63) and JFFS2's
cleanmarkers (spare bytes 2-9), read back the same way, as on the Nokia HD-1, whose rootfs was JFFS2 on this driver.
Nothing of the vendor's chain reads them: BL2's updater erases and writes only its own partitions.

## What is where

| | Size (gzip), and the room | |
|---|---|---|
| app partition: the boot stub, the kernel (deflated: the stub inflates it), the rescue | 1.8 MB: kernel 1.40, rescue 0.40, stub 0.02 (`os.bcd` 2.0 MB with the stock preloader, CnE and resources); the vendor's app slot is 10 MB (0x240000-0xC3FFFF, `boot-chain.md`) | the rescue (an initramfs, RAM only while it runs): the rescue's busybox (27 applets, `os.toml [busybox] rescue_applets`), the core build of `zd1` (`/bin/zd1`: only what the rescue runs, `os.toml [apps] rescue`: `init`'s rescue profile, `zd1-linkd`, `zd1-update`, `zd1-slot`, `zd1-settings`, ...; the system's services and early start, `zd1-init/src/system.rs`, aren't in it), `/init`: 0.73 MB unpacked |
| a system slot (`/`) | 5.1 MB (10.3 MB unpacked) of the slot's 32 MB | busybox (106 applets, `os.toml [busybox] applets`), the full `zd1` (`/usr/bin/zd1`: init and the core programs in `/bin`, the receivers, the self test, the Warp and memory probes in `/usr/bin`), `/etc` (`module/os/rootfs`), Nokia's kernel modules and Wi-Fi firmware, `wpa_supplicant`, ALSA's configuration and tools |
| data partition (`/data`) | | `/data/zd1/settings` |

The running system holds no file system in RAM: the kernel mounts the slot as its root, its files are read from the
NAND into the page cache as they are used, and `/dev`, `/run`, `/tmp`, `/var` are tmpfs (init), each with a ceiling
(64 KB, 512 KB, 8 MB, 1 MB) so that no writer can take the receivers' memory; init keeps each service's log in
`/var/log` under 64 KB (`module/os/README.md`). `/etc/os-release`
(init: the system's `/usr/lib/os-release` plus the variant from the command line) and `/etc/resolv.conf` point into
`/run`. [HW 2026-10-26: 4.1 MB used, 24.3 MB available with every service running; the emulator said the same]

## The boot: which slot
The app partition holds the boot stub, the kernel and the rescue, and changes rarely (BL2 or the relay write it).
Updating the OS means writing the slot that doesn't run, from the running system (`zd1-slot install`;
`docs/module/updater.md`), and starting it once on trial. **Which slot starts is the MCU's choice**
(`mcu/src/modboot.c`), kept in its EEPROM (the settings block's flags: the good slot, a slot to try once):
1. **The stub asks.** Our boot stub (`module/boot`, `linuxboot`) says `zd1-boot` on reg 0x25 with status booting
   (0x2000) and waits up to 5 s for the MCU's answer on reg 0xC4 (`docs/protocol/registers.md`): b0 slot A, slot B
   or the rescue, b1 the good slot. A slot: the stub adds `root=/dev/mtdblock1` (or 2) `rootfstype=jffs2 ro
   zd1.slot=a|b zd1.good=a|b` to the command line and leaves the initramfs out; the kernel mounts the slot and runs
   its `/sbin/init` (`zd1-init`, `module/os/README.md`). The rescue: the initramfs, `zd1.rescue zd1.good=...`. Either way
   the command line starts with the boot image's own, which ends in `zd1.boot=ID`: its identity, by content
   (`module/os/assemble.py`), reported as the State record's `boot`. No answer (the MCU's
   bootloader relaying an image, the stock MCU firmware): the rescue, since without the MCU the good slot is unknown
   and a slot may hold an old system or none; the rescue's `zd1-linkd` takes the bootloader's HCR 0x21 into BL2.
2. **The MCU answers** a slot to try (once: the try is cleared as it is answered), else the good slot, else the other
   one, else the rescue, skipping the ones that failed since the last ready.
3. **Ready decides.** The module reporting ready (0x2100, `zd1-linkd`) makes the slot it booted the good one and
   clears the failures; at that ready the MCU also tells our OS the good slot (reg 0xC4 again: `zd1-linkd` keeps it in
   `/run/zd1/good`). A boot that doesn't get there fails: the stub asking again first (a root that doesn't mount
   panics, `panic=10` restarts the module), or no ready within 90 s (180 s for the rescue), when the MCU power-cycles
   the module. So a bad slot costs one boot and the good one starts again; a speaker with both slots empty goes A,
   B, then the rescue. A rescue that doesn't get ready is powered off.

`zd1 slot` shows the MCU's state; `zd1 slot try a|b|rescue`, `zd1 slot good a|b`, `zd1 slot clear`, with `--restart`
to start it now (`BootState` / `Boot` in `docs/protocol/host.md`).

**The rescue** (the same init, its rescue profile: `/etc/os-release` says `VARIANT_ID=rescue`) brings the board up
(`zd1-board up`: the SPI slave needs the pads and clock gate 3 or 19, which no driver turns on here), mounts `/data`
if it is ours, starts `zd1-linkd` (the MCU sees ready and
"zd1-os ID rescue", ID the rescue's content hash: the same until the rescue itself changes) and waits for a system with a shell on the console: `zd1 update` writes the good slot
(`updater.md`) and starts it.

**`zd1-slot`** (Rust, `module/os/apps/zd1-slot`, in both the rescue and the system): `status` (what runs, the good
slot, what each slot holds), `install SLOT ID` (a gzip'd newc cpio of a root on stdin: the partition erased as
`flash_eraseall -j` does, with JFFS2 cleanmarkers, mounted, unpacked, the stamp `/.zd1-system` written last; never the
running slot, nor the good one while another runs on trial), `mtd NAME` (its device nodes). A slot holds a system
when it has `/sbin/init` and its stamp: an interrupted install leaves none. The install also records the stamp in
`/data/zd1/stamp.SLOT` (empty from the erase until it ends): reading another slot's stamp needs no mount. A mount
scans the slot's JFFS2 in the kernel for ~12 s, which this kernel doesn't preempt: `zd1-linkd` stopped draining the
SPI slave meanwhile and an MCU write right after boot lost bytes [HW 2026-11-02]. A slot without a record is mounted
read-only once and recorded. One lock (`/run/zd1-slot.lock`) keeps an install from erasing a slot a read has
mounted.

**Data**: init mounts `/data`; one that isn't ours yet (no `/data/.zd1-data`: new, or the stock firmware's
leftovers) is formatted once. It is never reformatted for a new image, newer or older: every version keeps
`/data/.zd1-data` and reads what another version wrote (the settings: `settings.md`, Another version's settings).

The emulator's dev boot (`zemu run`) is the system as one initramfs, `/init` its `/sbin/init`, without the slots flag
in the payload: the stub boots it as it is (`zd1-ctl status`: no slot).

## How the system gets there
`zd1 update` (`docs/module/updater.md`): the image (`system.cpio.gz`, a slot's root) over USB through the MCU to
`zd1-update`, which writes the slot that doesn't run (from the rescue: the good one) and stamps it; then the slot is
started once. The `.bcd` that BL2 or the relay install carries the stub, the kernel and the rescue only: a speaker
whose slots are empty (its first install) boots slot A, slot B, then the rescue (each empty slot is one failed
boot: a panic and `panic=10`'s restart, ~15 s), and waits there for its first `zd1 update` [HW 2026-10-26].

## On the unit [HW 2026-10-26 on]
The stub's question and the MCU's answer, empty slots failing (`panic=10` restarts the module), the rescue, a slot
installed and started on trial, confirmed by its ready; every update since into the slot that doesn't run (5 MB:
82 s through the MCU). Not yet seen: a trial that fails and falls back.

## Open
- Bad blocks in the slots and `data`: JFFS2 skips the ones the kernel's scan finds; none have been seen on this chip
  outside the factory list (`boot-chain.md`).
