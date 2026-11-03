//! Warp0 itself: one booted image, owned by one user (a lock file keeps everyone else out), its buffers in the
//! window, its job ring; and when it stops answering, a diagnosis instead of a shrug.
//!
//! Everything goes through /dev/mem (uncached): the debug block to start and park the core (`dm870::warp`), the
//! window (`../w0abi.h`) for the firmware, the job ring and the buffers, Warp0's local RAM for the image.

use crate::abi::*;
use dm870::clocks::{self, Gate, Gates};
use dm870::warp::{DEBUG0, debug};
use dm870::{DevMem, Region};
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant};

mod images {
    //! The firmware images (module/warp0/build.py; W0_FW_DIR, set by build.rs) and their symbols
    macro_rules! blob {
        ($name:literal) => {
            include_bytes!(concat!(env!("W0_FW_DIR"), "/", $name, ".bin"))
        };
    }
    pub static INVAL: &[u8] = blob!("inval");
    pub static CRYPTO: &[u8] = blob!("w0fw");
    pub static AAC_LOCAL: &[u8] = blob!("w0aac_local");
    pub static AAC_SDRAM: &[u8] = blob!("w0aac_sdram");
    pub static VORBIS_LOCAL: &[u8] = blob!("w0vorbis_local");
    pub static VORBIS_SDRAM: &[u8] = blob!("w0vorbis_sdram");
    pub static FLAC: &[u8] = blob!("w0flac");
    include!(concat!(env!("OUT_DIR"), "/symbols.rs"));
}

/// One process at a time: whoever holds this file's lock has Warp0.
const LOCK: &str = "/tmp/zd1-warp0.lock";
/// The marker the I-cache invalidate stub leaves in its last word: "INVD".
const INVAL_DONE: u32 = 0x494E_5644;

/// Which firmware image to boot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Image {
    /// Realtime and buffered ALAC packets, decrypted first if they come encrypted (`W0_OP_RTP_AUDIO`), all in local
    /// RAM.
    Crypto,
    /// The AAC-LC decoder (`W0_OP_AAC_*`): local RAM and SDRAM at `W0_AAC_SDRAM`.
    Aac,
    /// The Vorbis decoder (`W0_OP_VORBIS_*`): local RAM and SDRAM at `W0_VORBIS_SDRAM`.
    Vorbis,
    /// The FLAC decoder (`W0_OP_FLAC_*`), all in local RAM.
    Flac,
}

impl Image {
    pub fn name(self) -> &'static str {
        match self {
            Image::Crypto => "crypto",
            Image::Aac => "aac",
            Image::Vorbis => "vorbis",
            Image::Flac => "flac",
        }
    }

    /// What C_IMAGE says once it runs (W0_IMAGE_*).
    fn id(self) -> u32 {
        match self {
            Image::Crypto => W0_IMAGE_CRYPTO,
            Image::Aac => W0_IMAGE_AAC,
            Image::Vorbis => W0_IMAGE_VORBIS,
            Image::Flac => W0_IMAGE_FLAC,
        }
    }

    /// The part for local RAM (at Warp address 0), the part for SDRAM and its address (the entry point: an image
    /// without one starts at 0).
    fn parts(self) -> (&'static [u8], &'static [u8], u32) {
        match self {
            Image::Crypto => (images::CRYPTO, &[], 0),
            Image::Aac => (images::AAC_LOCAL, images::AAC_SDRAM, W0_AAC_SDRAM),
            Image::Vorbis => (images::VORBIS_LOCAL, images::VORBIS_SDRAM, W0_VORBIS_SDRAM),
            Image::Flac => (images::FLAC, &[], 0),
        }
    }

    fn symbols(self) -> &'static [(u32, &'static str)] {
        match self {
            Image::Crypto => images::W0FW,
            Image::Aac => images::W0AAC,
            Image::Vorbis => images::W0VORBIS,
            Image::Flac => images::W0FLAC,
        }
    }

    /// "function+0xoffset" for a PC in this image
    pub fn symbol(self, pc: u32) -> String {
        let syms = self.symbols();
        match syms.partition_point(|&(a, _)| a <= pc).checked_sub(1) {
            Some(i) => format!("{}+0x{:x}", syms[i].1, pc - syms[i].0),
            None => "?".into(),
        }
    }
}

/// Why an image didn't start; the core is parked again in every case.
#[derive(Clone, Copy, Debug, PartialEq)]
enum BootError {
    /// The core didn't enter debug mode: stuck (a bus hang), maybe until a power cycle.
    Halt,
    /// The I-cache invalidate stub didn't finish.
    Inval,
    /// The debug block refused the start sequence.
    Start,
    /// The firmware started but never reached its job loop.
    Magic,
    /// It did, and says it is another image (or another version of the interface).
    Image,
}

impl std::fmt::Display for BootError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(match self {
            BootError::Halt => "the core doesn't enter debug mode (stuck on a bus access?)",
            BootError::Inval => "the I-cache invalidate stub didn't finish",
            BootError::Start => "the debug block refused the start sequence",
            BootError::Magic => "the firmware didn't reach its job loop",
            BootError::Image => "the firmware running is another image",
        })
    }
}

/// How many hangs this boot may see before Warp0 is left alone until the next one (ZD1_WARP0_MAX_HANGS). After a
/// few bus hangs in one power cycle Warp0 has hung on correct code, and one hang took the whole module down
/// (docs/module/warp.md section 7): better the ARM than a third.
fn max_hangs() -> u32 {
    std::env::var("ZD1_WARP0_MAX_HANGS").ok().and_then(|v| v.parse().ok()).unwrap_or(2)
}

/// Where hang reports go.
pub const HANG_LOG: &str = "/var/log/zd1-warp0.log";
/// This boot's hang count: /tmp is RAM, so it starts at 0 with every boot.
const HANG_COUNT: &str = "/tmp/zd1-warp0.hangs";

/// Hangs recorded this boot.
pub fn hangs_this_boot() -> u32 {
    std::fs::read_to_string(HANG_COUNT).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

/// What the core was doing when it stopped answering (`Warp0::hang`).
#[derive(Clone, Debug)]
pub struct Diagnosis {
    /// One line: state, job, stage, beat, and the PC with the function it is in (or that there was no debug mode)
    pub text: String,
    /// The hang's number this boot
    pub count: u32,
}

/// The control block twice, 20 ms apart (is it moving at all?), and the PC if the core still enters debug mode.
struct Diag {
    magic: u32,
    version: u32,
    image: u32,
    state: u32,
    beat: [u32; 2],
    done: [u32; 2],
    stage: [u32; 2],
    head: u32,
    job: u32,
    refused: u32,
    pc: Option<u32>,
}

impl std::fmt::Display for Diag {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let moving = |v: [u32; 2]| if v[0] != v[1] { " (moving)" } else { "" };
        let image = String::from_utf8_lossy(&self.image.to_le_bytes()).into_owned();
        let state = ["boot", "idle", "busy", "stopped"].get(self.state as usize).unwrap_or(&"?");
        write!(f, "image {image} v{} {state}", self.version)?;
        if self.magic != W0_MAGIC {
            f.write_str(" (no magic)")?;
        }
        match self.job {
            W0_NOJOB => f.write_str(", job none")?,
            j => write!(f, ", job slot {} op {}", j >> 16, j & 0xFFFF)?,
        }
        write!(
            f,
            ", stage {:#x}{}, beat {}{}, done {}{}, head {}, refused {}; ",
            self.stage[0],
            moving(self.stage),
            self.beat[0],
            moving(self.beat),
            self.done[0],
            moving(self.done),
            self.head,
            self.refused
        )?;
        match self.pc {
            Some(pc) => write!(f, "pc {pc:08x}"),
            None => f.write_str("no debug mode (stuck on a bus access)"),
        }
    }
}

/// One job's fields (w0abi.h D_*); the ring slot's state is the runtime's business.
#[derive(Clone, Copy, Debug, Default)]
pub struct Job {
    pub op: u32,
    pub len: u32,
    pub src: u32,
    pub dst: u32,
    pub key: u32,
    pub nonce: u32,
    pub aad: u32,
    pub aadlen: u32,
    pub tag: u32,
    pub ctr: u32,
    pub conf: u32,
}

impl Job {
    /// The descriptor as bytes, D_STATE's word left out (it is written last, on its own).
    fn bytes(&self) -> [u8; (D_WORDS as usize - 1) * 4] {
        let mut d = [0u32; D_WORDS as usize];
        d[D_OP as usize] = self.op;
        d[D_LEN as usize] = self.len;
        d[D_SRC as usize] = self.src;
        d[D_DST as usize] = self.dst;
        d[D_KEY as usize] = self.key;
        d[D_NONCE as usize] = self.nonce;
        d[D_AAD as usize] = self.aad;
        d[D_AADLEN as usize] = self.aadlen;
        d[D_TAG as usize] = self.tag;
        d[D_CTR as usize] = self.ctr;
        d[D_CONF as usize] = self.conf;
        let mut b = [0u8; (D_WORDS as usize - 1) * 4];
        for (i, w) in d[1..].iter().enumerate() {
            b[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
        }
        b
    }
}

/// A finished job's results.
#[derive(Clone, Copy, Debug)]
pub struct Done {
    pub result: u32,
    pub outlen: u32,
    pub ctr: u32,
    /// D_TAG as the job left it (FLAC_DECODE: the input bytes taken)
    pub tag: u32,
}

/// Warp0 with an image running.
pub struct Warp0 {
    mem: DevMem,
    win: Region,
    local: Region,
    _lock: File,
    image: Image,
    next: u32,     // the ring slot of the next submit
    brk: u32,      // the next free buffer address
    data_end: u32, // the end of the running image's buffer area (C_DATA_END)
}

fn wait(timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let t = Instant::now();
    while !ready() {
        if t.elapsed() > timeout {
            return false;
        }
    }
    true
}

impl Warp0 {
    /// Map Warp0, take its lock, boot `image`. Refused while another user has it, or after this boot's hang budget
    /// is spent.
    pub fn open(image: Image) -> Result<Self, String> {
        let hangs = hangs_this_boot();
        if hangs >= max_hangs() {
            return Err(format!("Warp0 off for this boot after {hangs} hangs ({HANG_LOG})"));
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(LOCK)
            .map_err(|e| format!("{LOCK}: {e}"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(format!("in use ({LOCK}: {})", std::io::Error::last_os_error()));
        }
        let mut mem = DevMem::open().map_err(|e| format!("open: {e}"))?;
        mem.map(DEBUG0).map_err(|e| format!("open: {e}"))?;
        let win = mem.region(W0_BASE, W0_SIZE as usize).map_err(|e| format!("open: {e}"))?;
        let local = mem.region(W0_LOCAL_ARM, W0_LOCAL_SIZE as usize).map_err(|e| format!("open: {e}"))?;
        let mut warp = Warp0 { mem, win, local, _lock: lock, image, next: 0, brk: W0_DATA, data_end: W0_DATA };
        if let Err(e) = warp.boot() {
            if e == BootError::Image {
                return Err(format!("the {} image did not start: {e} (a build mismatch?)", image.name()));
            }
            // a core that doesn't come up is a hang too: say what it is doing, count it
            let d = warp.hang(&format!("booting: {e}"));
            return Err(format!("the {} image did not start: {e} [{}]", image.name(), d.text));
        }
        Ok(warp)
    }

    /// Power, halt, run the I-cache invalidate stub (the I-cache survives a reset), load the image, start it, wait
    /// for its job loop. The core is parked again if any step fails.
    fn boot(&mut self) -> Result<(), BootError> {
        let r = self.try_boot();
        if r.is_err() {
            self.park();
        }
        r
    }

    fn try_boot(&mut self) -> Result<(), BootError> {
        let mut tgen = clocks::tgen(&mut self.mem);
        if !tgen.gates().contains(Gate::WARP0) {
            tgen.enable(Gates::of(&[Gate::WARP0])); // on after our OS boots
        }
        self.data_end = W0_DATA;
        // 1. the I-cache invalidate stub; its last word is the marker it sets
        if !debug(&mut self.mem, DEBUG0).halt() {
            return Err(BootError::Halt);
        }
        self.win.put(W0_INVAL, images::INVAL);
        let marker = W0_INVAL + images::INVAL.len() as u32 - 4;
        self.win.write32(marker, 0);
        if !debug(&mut self.mem, DEBUG0).start(W0_INVAL) {
            return Err(BootError::Start);
        }
        if !wait(Duration::from_millis(100), || self.win.read32(marker) == INVAL_DONE) {
            return Err(BootError::Inval);
        }
        // 2. the image, with the control block and the ring cleared
        if !debug(&mut self.mem, DEBUG0).halt() {
            return Err(BootError::Halt);
        }
        for a in (W0_CTRL..W0_RING + W0_RING_N * D_WORDS * 4).step_by(4) {
            self.win.write32(a, 0);
        }
        let (local, sdram, entry) = self.image.parts();
        for (i, w) in local.chunks(4).enumerate() {
            // local RAM a word at a time
            let mut v = [0u8; 4];
            v[..w.len()].copy_from_slice(w);
            self.local.write32(W0_LOCAL_ARM + 4 * i as u32, u32::from_le_bytes(v));
        }
        if !sdram.is_empty() {
            self.win.put(entry, sdram);
        }
        if !debug(&mut self.mem, DEBUG0).start(entry) {
            return Err(BootError::Start);
        }
        if !wait(Duration::from_secs(1), || self.ctrl(C_MAGIC) == W0_MAGIC) {
            return Err(BootError::Magic);
        }
        if self.ctrl(C_IMAGE) != self.image.id() || self.ctrl(C_VERSION) != W0_VERSION {
            return Err(BootError::Image);
        }
        self.next = 0;
        self.data_end = self.ctrl(C_DATA_END);
        self.brk = W0_DATA;
        Ok(())
    }

    fn park(&mut self) {
        debug(&mut self.mem, DEBUG0).park();
    }

    pub fn image(&self) -> Image {
        self.image
    }

    /// A buffer in the window, 64-byte aligned, below the image's C_DATA_END.
    pub fn alloc(&mut self, len: u32) -> Option<u32> {
        let a = self.brk;
        if u64::from(a) + u64::from(len) > u64::from(self.data_end) {
            return None;
        }
        self.brk = (a + len).next_multiple_of(64).min(self.data_end);
        Some(a)
    }

    /// Copy into the window (memcpy: multi-word bursts through the uncached mapping).
    pub fn put(&self, phys: u32, data: &[u8]) {
        self.win.put(phys, data)
    }

    pub fn get(&self, phys: u32, out: &mut [u8]) {
        self.win.get(phys, out)
    }

    fn desc(slot: u32, word: u32) -> u32 {
        W0_RING + slot * D_WORDS * 4 + word * 4
    }

    /// Into the ring; `None` when the next slot is still in use.
    pub fn submit(&mut self, job: &Job) -> Option<u32> {
        let slot = self.next;
        if self.win.read32(Self::desc(slot, D_STATE)) != W0_FREE {
            return None;
        }
        self.win.put(Self::desc(slot, 1), &job.bytes()); // one burst, not 15 bus cycles
        self.win.write32(Self::desc(slot, D_STATE), W0_SUBMIT); // last: strongly ordered, the Warp sees the rest
        self.next = (slot + 1) & (W0_RING_N - 1);
        Some(slot)
    }

    /// The job in `slot`, if it is done (the slot is then free again).
    pub fn poll(&mut self, slot: u32) -> Option<Done> {
        let w = |word| self.win.read32(Self::desc(slot, word));
        if w(D_STATE) != W0_DONE {
            return None;
        }
        let done = Done { result: w(D_RESULT), outlen: w(D_OUTLEN), ctr: w(D_CTR), tag: w(D_TAG) };
        self.win.write32(Self::desc(slot, D_STATE), W0_FREE);
        Some(done)
    }

    /// One job, waited for (yielding) up to `timeout`; a timeout is a hang (diagnosed, counted, the core parked).
    pub fn run(&mut self, job: &Job, timeout: Duration) -> Result<Done, String> {
        let slot = self.submit(job).ok_or("ring full")?;
        let t = Instant::now();
        loop {
            if let Some(d) = self.poll(slot) {
                return Ok(d);
            }
            if t.elapsed() > timeout {
                return Err(self.hang(&format!("op {} took over {} ms", job.op, timeout.as_millis())).text);
            }
            std::thread::yield_now();
        }
    }

    fn diagnose(&mut self) -> Diag {
        let c = |w: &Self, word| w.ctrl(word);
        let mut g = Diag {
            magic: c(self, C_MAGIC),
            version: c(self, C_VERSION),
            image: c(self, C_IMAGE),
            state: c(self, C_STATE),
            beat: [c(self, C_BEAT); 2],
            done: [c(self, C_DONE); 2],
            stage: [c(self, C_STAGE); 2],
            head: c(self, C_HEAD),
            job: c(self, C_JOB),
            refused: c(self, C_REFUSED),
            pc: None,
        };
        std::thread::sleep(Duration::from_millis(20));
        (g.beat[1], g.done[1], g.stage[1]) = (c(self, C_BEAT), c(self, C_DONE), c(self, C_STAGE));
        g.pc = debug(&mut self.mem, DEBUG0).inspect();
        g
    }

    /// Count a hang for this boot and append it to the log, with a timestamp; the new count.
    fn record(what: &str, line: &str) -> u32 {
        let n = hangs_this_boot() + 1;
        let _ = std::fs::write(HANG_COUNT, format!("{n}\n"));
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(HANG_LOG) {
            let _ = writeln!(f, "{secs} hang {n} this boot: {what}: {line}");
        }
        n
    }

    /// It stopped answering: what was it doing? The control block twice, the PC if it still enters debug mode
    /// (its function from the image's symbols), counted and written to the hang log; then the core is parked. The
    /// handle stays usable only for Drop.
    pub fn hang(&mut self, what: &str) -> Diagnosis {
        let g = self.diagnose();
        let mut text = g.to_string();
        if let Some(pc) = g.pc {
            text += &format!(" = {}", self.image.symbol(pc));
        }
        let count = Self::record(&format!("{} image: {what}", self.image.name()), &text);
        self.park();
        self.win.write32(W0_CTRL + 4 * C_MAGIC, 0); // parked: no job loop to answer a stop (Drop would wait for it)
        tracing::error!(image = self.image.name(), what, count, diagnosis = %text, "Warp0 hang");
        Diagnosis { text, count }
    }

    /// A control block word (w0abi.h C_*)
    pub fn ctrl(&self, word: u32) -> u32 {
        self.win.read32(W0_CTRL + 4 * word)
    }

    /// Leave the job loop (the runtime spins in registers) and park the core.
    fn stop(&mut self) {
        if self.ctrl(C_MAGIC) == W0_MAGIC {
            self.win.write32(W0_CTRL + 4 * C_CMD, W0_CMD_STOP);
            wait(Duration::from_secs(1), || self.ctrl(C_STATE) == W0_ST_STOPPED);
        }
        self.park();
        self.win.write32(W0_CTRL + 4 * C_MAGIC, 0);
        self.win.write32(W0_CTRL + 4 * C_CMD, W0_CMD_RUN);
        self.data_end = W0_DATA;
    }
}

impl Drop for Warp0 {
    fn drop(&mut self) {
        self.stop();
    }
}
