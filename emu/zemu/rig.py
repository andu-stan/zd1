"""One emulator session on Renode (emu/renode): the machines, their logs, and the steps that drive them.

  * one Renode process per session, driven over its console (stdin/stdout); nothing shared between sessions;
  * every length and timeout (waits, sleeps) is VIRTUAL time: host load does not change what the guest sees, and
    idle time costs nothing;
  * with the main board, both machines are one emulation on one clock (the J2 link is a cross-machine event).

Normally used through `zemu run` (cli.py). From Python:

    from zemu.rig import Session
    s = Session(cfg, rundir)        # cfg: what cli.session_config() builds
    s.start(); s.wait(r'\\[os\\] up'); rc, out = s.sh('cat /proc/interrupts'); s.close()
"""

import hashlib
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time

from .images import fast_copy
from .paths import REPO

RENODE_DIR = os.path.join(REPO, "emu", "renode")
MODELS = [
    "avr32/Avr32Memory.cs",
    "avr32/Avr32Decoder.g.cs",
    "avr32/Avr32.cs",
    "avr32/UC3Common.cs",
    "avr32/UC3Core.cs",
    "avr32/UC3Io.cs",
    "avr32/UC3Buses.cs",
    "avr32/UC3Usb.cs",
    "avr32/UC3Audio.cs",
    "zd1/ZD1Board.cs",
    "cr870/CR870.cs",
    "cr870/CR870Warp.cs",
    "cr870/CR870Sol.cs",
    "cr870/CR870Macb.cs",
    "cr870/CR870UserNet.cs",
    "cr870/J2Link.cs",
]
# the stock module app's TVService stalls without a video provider; returning 1 from PicturePlayer start lets it run
STOCK_APP_PATCH = "0x6022d8d0=0xe3a00001,0x6022d8d4=0xe12fff1e"
# the stock app's microsecond busy-wait (FUN_60179820: `while(T(0x230) - r4 < r0)`): sleep instead (core DelayLoops)
STOCK_DELAY_LOOPS = "0x60179848:4:0"
# --keep: the module console, the MCU console, the MCU's USB (zd1: ZD1DFU_EMU=127.0.0.1:4620)
KEEP_MODCON, KEEP_USB = 4555, 4620
KEEP_FWD = [(8080, 80), (2323, 23), (2324, 2323)]
BIND = os.environ.get("ZEMU_BIND", "127.0.0.1")
CHUNK = 0.05  # virtual seconds per RunFor while waiting for output
WALL_LIMIT = 1800  # one RunFor that takes longer than this (host seconds) is a hang

STREAMS = {
    "console": "module_uart1.log",
    "mcu": "mcu_log.txt",
    "log": "module.log",
    "trace": "module.trace",
    "mcu-trace": "mcu_trace.txt",
    "host": "host.log",
    "net": "net.log",
    "uart0": "module_uart0.log",
}


def log(msg):
    print(msg, flush=True)


# ---- Renode ---------------------------------------------------------------------------------------------------------
def models_file():
    """All MODELS as one C# file (one compilation: ~1 s instead of ~5 s), cached by content in emu/renode/dist.
    The using directives move to the top; #line keeps compiler messages pointing at the original files."""
    head, body = [], []
    for m in MODELS:
        path = os.path.join(RENODE_DIR, m)
        body.append('#line 1 "%s"\n' % path)
        for line in open(path):
            if re.match(r"^using [\w.]+( = [\w.<>]+)?;\s*$", line):
                if line not in head:
                    head.append(line)
                line = "\n"
            body.append(line)
    text = "".join(head) + "".join(body)
    out = os.path.join(RENODE_DIR, "dist", "models-%s.cs" % hashlib.sha1(text.encode()).hexdigest()[:12])
    if not os.path.exists(out):
        os.makedirs(os.path.dirname(out), exist_ok=True)
        tmp = "%s.%d.%d.tmp" % (out, os.getpid(), threading.get_ident())  # parallel sessions race to write it
        with open(tmp, "w") as f:
            f.write(text)
        os.replace(tmp, out)
    return out


def gate_open(eeprom):
    """Our MCU firmware's settings block in the run's EEPROM copy (mcu/src/settings.c: 0x80, [Z][D][ver][vol][bass][src]
    [flags][crc8]) with the host link's gate open for good: nobody presses ON/OFF on an emulated speaker."""

    def crc8(b):
        c = 0xFF
        for x in b:
            c ^= x
            for _ in range(8):
                c = ((c << 1) ^ 0x31 if c & 0x80 else c << 1) & 0xFF
        return c

    with open(eeprom, "r+b") as f:
        f.seek(0x80)
        b = bytearray(f.read(8))
        if not (b[:3] == b"ZD\x01" and b[7] == crc8(b[:7]) and b[3] <= 71 and b[4] <= 4):
            b = bytearray(b"ZD\x01" + bytes([30, 0, 0, 0, 0]))  # the firmware's defaults
        b[6] |= 0x01  # SETTINGS_GATE_OPEN
        b[7] = crc8(b[:7])
        f.seek(0x80)
        f.write(b)


def models_dll():
    """The models precompiled: Renode loads assemblies next to its executable at start, so a DLL there saves the
    ~1.4 s compile of every session. Built once per models_file() hash (one Renode run that includes the .cs, then
    its temporary assembly is kept); older ones are removed (two would define the same types). None: use the .cs."""
    if os.environ.get("ZEMU_MODELS_CS"):
        return None
    import fcntl
    import glob

    cs = models_file()
    h = os.path.basename(cs)[len("models-") : -len(".cs")]
    d = os.path.dirname(os.path.realpath(renode_exe()))
    dll = os.path.join(d, "ZemuModels-%s.dll" % h)
    if os.path.exists(dll):
        return dll
    if not os.access(d, os.W_OK):
        return None  # a read-only Renode (e.g. in a nix store): compile per session
    with open(os.path.join(RENODE_DIR, "dist", "models.lock"), "w") as lk:
        fcntl.flock(lk, fcntl.LOCK_EX)
        if os.path.exists(dll):
            return dll
        for old in glob.glob(os.path.join(d, "ZemuModels-*.dll")) + [
            f for f in glob.glob(os.path.join(RENODE_DIR, "dist", "models-*.cs")) if f != cs
        ]:
            os.remove(old)
        tmp = tempfile.mkdtemp(prefix="zemu-models-")
        r = Renode(tmp)
        try:
            r.cmd("include @%s" % cs, timeout=300)
            built = [f for f in glob.glob(os.path.join(tempfile.gettempdir(), "renode-%d" % r.p.pid, "*.dll"))]
            built = [f for f in built if b"AVR32UC" in open(f, "rb").read()]
            if len(built) != 1:
                log("  (renode: compiled models not found in its temp dir; compiling per session)")
                return None
            shutil.copyfile(built[0], "%s.%d.tmp" % (dll, os.getpid()))
            os.replace("%s.%d.tmp" % (dll, os.getpid()), dll)
        finally:
            r.close()
            shutil.rmtree(tmp, ignore_errors=True)
    return dll


def renode_exe():
    e = os.environ.get("ZEMU_RENODE")
    if e:
        return e
    for c in (
        os.path.join(RENODE_DIR, "dist", "Renode.app", "Contents", "MacOS", "renode"),
        os.path.join(RENODE_DIR, "dist", "renode", "renode"),
    ):
        if os.path.exists(c):
            return c
    found = shutil.which("renode")
    if found:
        return found
    sys.exit("no Renode: run `python3 emu/zemu setup` (or set ZEMU_RENODE)")


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


class Renode:
    """A Renode process and its monitor, over pipes."""

    def __init__(self, rundir):
        self.log = open(os.path.join(rundir, "renode.log"), "w")
        env = dict(os.environ)
        env.setdefault("DOTNET_TieredPGO", "0")  # no dynamic-PGO instrumentation (~0.5 s per start)
        self.p = subprocess.Popen(
            [renode_exe(), "--console", "--disable-gui", "--plain", "--hide-log"],
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            cwd=REPO,
            start_new_session=True,
            bufsize=0,
        )
        # Renode outlives a killed driver (Ctrl-C, crash): a watcher ends its process group with us
        subprocess.Popen(
            [
                "sh",
                "-c",
                "while kill -0 %d 2>/dev/null && kill -0 %d 2>/dev/null; do sleep 1; done; "
                "kill -9 -%d 2>/dev/null" % (os.getpid(), self.p.pid, self.p.pid),
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        self.n = 0
        self.buf = b""
        self.cur = None
        self.logpath = os.path.join(rundir, "renode_log.txt")
        self.cmd("logFile @%s" % self.logpath)

    def on(self, machine):
        """Make `machine` the monitor's current machine (peripheral names are per machine)."""
        if self.cur != machine:
            self.cmd('mach set "%s"' % machine)
            self.cur = machine

    def cmd(self, c, timeout=600):
        """Run one monitor command, return its output. Errors in the output raise."""
        self.n += 1
        mark = "@@zemu%d@@" % self.n
        self.log.write(">> %s\n" % c)
        self.p.stdin.write(('%s\necho "%s"\n' % (c, mark)).encode())
        self.p.stdin.flush()
        out = self._until(mark.encode(), timeout)
        self.log.write(out)
        self.log.flush()
        if re.search(r"There was an error executing command|Errors during compilation|No such command or device", out):
            raise RuntimeError("renode: %s\n%s" % (c, out.strip()[-2000:]))
        return out

    def _until(self, mark, timeout):
        end = time.time() + timeout
        next_check = time.time() + 1
        os.set_blocking(self.p.stdout.fileno(), False)
        while mark not in self.buf:
            if time.time() > end:
                raise TimeoutError("renode: no answer for %d s" % timeout)
            if time.time() > next_check:  # an aborted machine never lets RunFor return
                next_check = time.time() + 1
                if getattr(self, "logpath", None):
                    self.check_abort()
            if self.p.poll() is not None:
                raise RuntimeError("renode exited (%s)" % self.p.returncode)
            try:
                chunk = self.p.stdout.read(65536)
            except BlockingIOError:
                chunk = None
            if chunk:
                self.buf += chunk
            else:
                time.sleep(0.005)
        i = self.buf.index(mark)
        out, self.buf = self.buf[:i], self.buf[i + len(mark) :]
        return out.decode("latin-1")

    def vtime(self):
        m = re.search(r"Elapsed Virtual Time: (\d+):(\d+):([0-9.]+)", self.cmd("emulation GetTimeSourceInfo"))
        return int(m.group(1)) * 3600 + int(m.group(2)) * 60 + float(m.group(3)) if m else 0.0

    def run_for(self, secs):
        sample = os.environ.get("ZEMU_SAMPLE")  # profiling: "machine" -> PC/LR/CPSR every 10 ms
        if not sample:
            self.cmd('emulation RunFor "%f"' % secs, timeout=WALL_LIMIT)
            self.check_abort()
            return
        prev = self.cur
        now = self.vtime()
        start = float(os.environ.get("ZEMU_SAMPLE_FROM", 0))  # one RunFor up to here: the same timing as unsampled
        step_s = float(os.environ.get("ZEMU_SAMPLE_STEP", 0.01))
        left = secs
        if start > now:
            first = min(secs, start - now)
            self.cmd('emulation RunFor "%f"' % first, timeout=WALL_LIMIT)
            self.check_abort()
            left -= first
        with open(os.path.join(os.path.dirname(self.logpath), "samples.txt"), "a") as f:
            while left > 1e-9:
                step = min(step_s, left)
                self.cmd('emulation RunFor "%f"' % step, timeout=WALL_LIMIT)
                self.check_abort()
                left -= step
                self.on(sample)
                vals = []
                regs = os.environ.get("ZEMU_SAMPLE_REGS", "")  # e.g. "0,1,2,3"
                for c in ["cpu PC", "cpu GetRegisterUnsafe 14", "cpu CPSR"] + [
                    "cpu GetRegisterUnsafe %s" % n for n in regs.split(",") if n
                ]:
                    try:
                        vals.append(re.findall(r"0x[0-9a-fA-F]+", self.cmd(c)))
                    except RuntimeError:
                        vals.append([])
                t = re.search(r"Elapsed Virtual Time: (\S+)", self.cmd("emulation GetTimeSourceInfo"))
                f.write(
                    "%s %.4f %s\n"
                    % (t.group(1) if t else "?", time.time(), " ".join(v[-1] if v else "?" for v in vals))
                )
                f.flush()
        if prev:
            self.on(prev)

    def check_abort(self):
        """An aborted machine stops the emulation for good (RunFor would hang): raise instead."""
        try:
            t = open(self.logpath, errors="replace").read()
        except OSError:
            return
        m = re.search(r"\[ERROR\] (\w+): This machine has aborted", t)
        if m:
            ctx = re.findall(r"\[ERROR\] .*", t)
            raise RuntimeError("machine %s aborted: %s" % (m.group(1), " | ".join(ctx[-3:])))

    def close(self):
        if self.p.poll() is None:
            try:
                self.p.stdin.write(b"quit\n")
                self.p.stdin.flush()
                self.p.wait(30)
            except (BrokenPipeError, subprocess.TimeoutExpired):
                pass
        if self.p.poll() is None:
            os.killpg(self.p.pid, signal.SIGKILL)  # the whole process group, nothing left behind
            self.p.wait()
        self.log.close()


# ---- a session ------------------------------------------------------------------------------------------------------
class StepFailed(Exception):
    pass


class Session:
    """The boards from power-up. cfg: {module: {nand, stage2, stock, bl1_real, trace, wav, patch, fwd, gdb} | None,
    mcu: {flash} | None, keep, quiet, quantum_us, renode_cmds}. Working copies of the images and every log go to
    rundir; the NAND, MCU flash and user page are saved back there at the end."""

    def __init__(self, cfg, rundir):
        self.cfg, self.dir = cfg, os.path.abspath(rundir)
        self.mod, self.mcu = cfg.get("module"), cfg.get("mcu")
        os.makedirs(self.dir, exist_ok=True)
        self.env = {}  # for host commands (ZD1DFU_EMU, ZD1_MODCON_EMU)
        self.side = []  # background host commands
        self.mark = {}  # stream -> offset where the current step's output starts
        self.echoed = {}
        self.part = {}
        self.nsh = 0
        self.r = None

    def path(self, name):
        return os.path.join(self.dir, name)

    def stream_file(self, stream):
        if stream == "console" and not self.mod:
            stream = "mcu"
        return self.path(STREAMS[stream])

    def text(self, stream):
        try:
            return open(self.stream_file(stream), "rb").read().decode("latin-1")
        except OSError:
            return ""

    # ---- the machines ----------------------------------------------------------------------------------------
    def start(self):
        for f, src in (
            ("nand.bin", (self.mod or {}).get("nand")),
            ("mcu_flash.bin", (self.mcu or {}).get("flash")),
            ("user_page.bin", (self.mcu or {}).get("user_page")),
            ("eeprom.bin", (self.mcu or {}).get("eeprom")),
        ):
            if src:
                fast_copy(src, self.path(f))
                if f == "nand.bin":  # the spare areas (CR870_NAND), when the image has them
                    if os.path.exists(src + ".oob"):
                        fast_copy(src + ".oob", self.path(f + ".oob"))
                    elif os.path.exists(self.path(f + ".oob")):
                        os.remove(self.path(f + ".oob"))
        if (self.mcu or {}).get("gate_open"):
            gate_open(self.path("eeprom.bin"))
        keep = {"nand.bin", "nand.bin.oob", "mcu_flash.bin", "user_page.bin", "eeprom.bin"}
        for f in os.listdir(self.dir):  # the last session's logs and traces
            if f not in keep and os.path.isfile(self.path(f)):
                os.remove(self.path(f))
        loaded = models_dll() is not None  # before Renode starts: it loads the DLL at start
        r = self.r = Renode(self.dir)
        if not loaded:
            r.cmd("include @%s" % models_file(), timeout=300)
        if self.mod:
            self.setup_module(r)
        if self.mcu:
            self.setup_mcu(r)
        r.cmd("emulation SetAdvanceImmediately true")
        # one thread for all machines: bit-for-bit repeatable (with a thread per machine linked runs diverge)
        r.cmd("emulation SetGlobalSerialExecution true")
        for c in self.cfg.get("renode_cmds", []):  # "machine:command"
            m, _, c = c.partition(":")
            r.on(m)
            r.cmd(c)
        # linked: 200 us (the B&W bootloader's register reads need the J2 round trip, 2 quanta, well under 1 ms);
        # one machine: 1 ms (each quantum costs a time-source hand-off; events still land on time inside it)
        q = self.cfg.get("quantum_us") or (200 if self.mod and self.mcu else 1000)
        r.cmd('emulation SetGlobalQuantum "%s"' % (q / 1e6))
        if self.mod:
            r.on("cr870")
            if not self.mcu:
                r.cmd("core PowerGated false")  # nobody drives the J2 power pin
                r.cmd("core PowerOnAtStart")
            else:
                r.cmd("cpu IsHalted true")  # unpowered until the MCU drives PA04 (like the unit)
        self.vt = 0.0
        self.t0 = time.time()

    def setup_module(self, r):
        m = self.mod
        r.cmd('mach create "cr870"')
        r.cur = "cr870"
        r.cmd(
            "machine LoadPlatformDescription @%s"
            % os.path.join(RENODE_DIR, "platforms", "cr870.repl" if self.mcu else "cr870-alone.repl")
        )
        open(self.path("module.log"), "a").close()
        r.cmd("core LogFile @%s" % self.path("module.log"))
        r.cmd("uart1 CreateFileBackend @%s true" % self.path(STREAMS["console"]))
        # UART1 (the module console) is wired to the MCU's USART1 on the unit: a hub joins it to the MCU; a socket
        # too (a terminal connects to the UART, not to the hub), a tap on the module's UART: host commands get it as
        # ZD1_MODCON_EMU (`zd1 update --uart`, the module alone)
        r.cmd('emulation CreateUARTHub "modcon"')
        r.cmd("connector Connect uart1 modcon")
        port = KEEP_MODCON if self.cfg.get("keep") else free_port()
        r.cmd('emulation CreateServerSocketTerminal %d "modcon_sock" false' % port)
        r.cmd("connector Connect uart1 modcon_sock")
        self.env["ZD1_MODCON_EMU"] = "127.0.0.1:%d" % port
        r.cmd("uart0 CreateFileBackend @%s true" % self.path(STREAMS["uart0"]))
        patch = ",".join(filter(None, [STOCK_APP_PATCH if m.get("stock") else "", m.get("patch") or ""]))
        if patch:
            r.cmd('core Patch "%s"' % patch)
        if m.get("stock"):
            r.cmd('core DelayLoops "%s"' % STOCK_DELAY_LOOPS)
        r.cmd("nand Load @%s" % self.path("nand.bin"))
        if m.get("stage2"):  # stage 2 straight into SDRAM, ROM/BL1/preloader skipped
            r.cmd("core Stage2Image @%s" % m["stage2"])
        else:  # from NAND through BL1, like the unit
            r.cmd("core BootFromNand true")
            if m.get("bl1_real"):
                r.cmd("core Bl1Image @%s" % self.real_bl1())
                r.cmd("core TimerFreerun true")
                r.cmd('core TimerWaitPcs "405830"')  # BL1's timeout helper: its waits cost no host time
        if m.get("trace"):
            r.cmd("core TraceFile @%s" % self.path(STREAMS["trace"]))
            if isinstance(m["trace"], str):
                r.cmd('core TraceOnly "%s"' % m["trace"])
        if m.get("wav"):
            r.cmd("core AudioWav @%s" % self.path(m["wav"]))
        if not self.mcu:
            r.cmd("spis.testhost LogFile @%s" % self.path(STREAMS["host"]))
        open(self.path("net.log"), "a").close()
        r.cmd("usernet LogFile @%s" % self.path("net.log"))
        r.cmd('usernet Bind "%s"' % BIND)
        self.fwd = list(m.get("fwd") or []) + (KEEP_FWD if self.cfg.get("keep") else [])
        if self.fwd:
            r.cmd('usernet Forward "%s"' % ",".join("%d:%d" % hg for hg in self.fwd))
        if m.get("gdb"):
            r.cmd("machine StartGdbServer %d" % m["gdb"])

    def setup_mcu(self, r):
        r.cmd('mach create "zd1"')
        r.cur = "zd1"
        r.cmd(
            "machine LoadPlatformDescription @%s"
            % os.path.join(RENODE_DIR, "platforms", "zd1-cr870.repl" if self.mod else "zd1-standin.repl")
        )
        r.cmd("pm TraceFile @%s" % self.path(STREAMS["mcu-trace"]))
        r.cmd("pm TraceMaxLines 200000")
        r.cmd("usart0 CreateFileBackend @%s true" % self.path(STREAMS["mcu"]))
        if self.mod:  # USART1 = the module console (setup_module's hub)
            r.cmd("connector Connect usart1 modcon")
            r.cmd("modcon Start")
        r.cmd("flash Load @%s" % self.path("mcu_flash.bin"))
        r.cmd("userpage Load @%s" % self.path("user_page.bin"))
        r.cmd("twi.eeprom File @%s" % self.path("eeprom.bin"))
        port = KEEP_USB if self.cfg.get("keep") else free_port()
        r.cmd("usbb Port %d" % port)
        self.env["ZD1DFU_EMU"] = "127.0.0.1:%d" % port

    def real_bl1(self):
        """The re-runnable real BL1, blobs/module/bl1.run (blobs/README.md, re/netmodule/bl1/README.md)."""
        return os.path.join(REPO, "blobs", "module", "bl1.run")

    def close(self):
        r = self.r
        try:
            if r and r.p.poll() is None:
                if self.mcu:
                    r.on("zd1")
                    r.cmd("flash Save @%s" % self.path("mcu_flash.bin"))
                    r.cmd("userpage Save @%s" % self.path("user_page.bin"))
                if self.mod:
                    r.on("cr870")
                    r.cmd("nand Save @%s" % self.path("nand.bin"))
                    if self.mod.get("trace"):  # the trace's 1 MiB buffer is lost unless closed
                        r.cmd('core TraceFile ""', timeout=30)
        except Exception as ex:
            log("  (saving the state failed: %s)" % ex)
        finally:
            if r:
                r.close()
            for p in self.side:
                if p.poll() is None:
                    try:
                        os.killpg(p.pid, signal.SIGTERM)
                    except OSError:
                        pass
            self.pump()

    # ---- output ----------------------------------------------------------------------------------------------
    def pump(self):
        """Echo new console output (module, and the MCU's as `[mcu] ...`) unless quiet."""
        if self.cfg.get("quiet"):
            return
        for stream, prefix in (("console", ""), ("mcu", "[mcu] ")) if self.mod else (("console", "[mcu] "),):
            if stream == "mcu" and not self.mcu:
                continue
            f = self.stream_file(stream)
            try:
                with open(f, "rb") as fh:
                    fh.seek(self.echoed.get(f, 0))
                    new = fh.read()
            except OSError:
                continue
            if not new:
                continue
            self.echoed[f] = self.echoed.get(f, 0) + len(new)
            text = self.part.get(f, "") + new.decode("latin-1").replace("\r\n", "\n").replace("\r", "\n")
            *lines, self.part[f] = text.split("\n")
            for line in lines:
                sys.stdout.write(prefix + line + "\n")
            sys.stdout.flush()

    def run_for(self, secs):
        self.r.run_for(secs)
        self.vt += secs
        self.pump()

    # ---- steps -----------------------------------------------------------------------------------------------
    def begin(self):
        """A new action: waits after it look at the output from here on."""
        self.mark = {s: len(self.text(s)) for s in STREAMS}

    def wait(self, rx, stream="console", timeout=300):
        """Run until `rx` (Python re, multiline) shows up in `stream` since the last action began; the match."""
        rc = re.compile(rx, re.M)
        start = self.mark.get(stream, 0)
        end = self.vt + timeout
        while True:
            t = self.text(stream)
            m = rc.search(t, start)
            if m:
                return m
            if self.mod and not self.cfg.get("allow_panic"):
                p = re.search(r"Kernel panic.*", self.text("console"))
                if p:
                    raise StepFailed("module: %s" % p.group(0).strip())
            if self.vt >= end:
                raise StepFailed("no /%s/ in %s within %g s" % (rx, stream, timeout))
            self.run_for(CHUNK)

    def sleep(self, secs):
        self.begin()
        self.run_for(secs)

    def send(self, text, cr=True):
        """Type into the module console (a shell, KnOS, the BL1/BL2 menus); `\\xNN` escapes reach the guest raw."""
        self.begin()
        text += "\r" if cr else ""
        self.r.on("cr870")
        self.r.cmd('uart1 Type "%s"' % text.replace('"', '\\"').replace("\r", "\\r"))
        self.run_for(0.002 * (len(text) + 1))

    def sh(self, command, timeout=300):
        """A command in the module's Linux shell: (exit code, output)."""
        self.nsh += 1
        n = self.nsh
        self.send('%s; echo "~zemu-$?-$((%d+0))~"' % (command, n))  # the echoed line itself doesn't match
        m = self.wait(r"~zemu-(\d+)-%d~" % n, "console", timeout)
        out = self.text("console")[self.mark["console"] : m.start()].replace("\r", "")
        out = out.split("\n", 1)[1] if "\n" in out else ""  # minus the echoed command line
        return int(m.group(1)), out

    def spi(self, commands, reply_timeout=0.5):
        """The SPI test host on the module's link (module alone): rd / rdv / wr / wrv / req (emu/renode/cr870/J2Link.cs)."""
        self.begin()
        self.r.on("cr870")
        self.r.cmd("spis.testhost ReplyTimeout %s" % reply_timeout)
        self.r.cmd('spis.testhost Run "%s"' % commands)
        end = self.vt + 60
        while "True" in self.r.cmd("spis.testhost Busy") and self.vt < end:
            self.run_for(CHUNK)
        return self.text("host")[self.mark["host"] :]

    def host(self, command, timeout=600, background=False):
        """A command on the host while the boards run (as fast as they go): zd1 over the emulated USB
        (ZD1DFU_EMU is set), tools against forwarded ports. The exit code."""
        self.begin()
        env = dict(os.environ, PYTHONUNBUFFERED="1", RUN=self.dir, **self.env)
        if background:
            out = open(self.path("host_bg%d.log" % len(self.side)), "w")
            self.side.append(
                subprocess.Popen(
                    command, shell=True, cwd=REPO, env=env, stdout=out, stderr=subprocess.STDOUT, start_new_session=True
                )
            )
            return 0
        with open(self.path(STREAMS["host"]), "a") as f:
            f.write("== %s\n" % command)
            f.flush()
            self.r.cmd("start")
            t0 = self.r.vtime()
            try:
                rc = subprocess.run(
                    command, shell=True, cwd=REPO, env=env, stdout=f, stderr=subprocess.STDOUT, timeout=timeout
                ).returncode
            except subprocess.TimeoutExpired:
                rc = "timeout"
            self.r.cmd("pause")
            self.vt += self.r.vtime() - t0
            f.write("   exit %s\n" % rc)
        self.pump()
        return rc

    def hold(self):
        """--keep: the boards go on in real time until Ctrl-C."""
        socks = []
        if self.mod:
            socks.append("module console nc 127.0.0.1:%d" % KEEP_MODCON)
        if self.mcu:
            socks.append("MCU USB ZD1DFU_EMU=127.0.0.1:%d" % KEEP_USB)
        if self.mod:
            socks.append("forwards " + ", ".join("%d->%d" % hg for hg in self.fwd))
        log("== boards up (real time): %s. Ctrl-C ends" % "; ".join(socks))
        self.r.cmd("emulation SetAdvanceImmediately false")
        t0 = self.r.vtime()
        self.r.cmd("start")
        try:
            while self.r.p.poll() is None:
                time.sleep(0.1)
                self.pump()
        except KeyboardInterrupt:
            pass
        self.r.cmd("pause")
        self.vt += self.r.vtime() - t0
