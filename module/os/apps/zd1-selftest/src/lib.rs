//! zd1-selftest: the userspace platform check of the module OS. Exercises what std needs from the kernel (2.6.24.7,
//! older than musl and Rust assume): clocks, threads + atomics (kuser helpers on ARMv5), futex waits, TCP, files,
//! processes, randomness, /proc. One line per check, then `SELFTEST PASS` or `SELFTEST FAIL (n)` (what to wait for).
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn check(name: &str, f: impl FnOnce() -> Result<String, String>, failed: &mut u32) {
    match f() {
        Ok(detail) => println!("selftest: ok   {name}: {detail}"),
        Err(e) => {
            println!("selftest: FAIL {name}: {e}");
            *failed += 1;
        }
    }
}

pub fn main() {
    zd1_compat::init();
    let mut failed = 0;
    check(
        "clock",
        || {
            let t0 = Instant::now();
            std::thread::sleep(Duration::from_millis(20));
            let dt = t0.elapsed();
            let wall = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?;
            if dt < Duration::from_millis(15) {
                return Err(format!("slept {dt:?}"));
            }
            Ok(format!("sleep 20 ms took {} ms, wall {} s", dt.as_millis(), wall.as_secs()))
        },
        &mut failed,
    );
    check(
        "threads",
        || {
            let n = Arc::new(AtomicU32::new(0));
            let hs: Vec<_> = (0..4)
                .map(|_| {
                    let n = n.clone();
                    std::thread::spawn(move || {
                        for _ in 0..10_000 {
                            n.fetch_add(1, Ordering::SeqCst);
                        }
                    })
                })
                .collect();
            for h in hs {
                h.join().map_err(|_| "join".to_string())?;
            }
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || tx.send(42u32).unwrap());
            let v = rx.recv_timeout(Duration::from_secs(5)).map_err(|e| e.to_string())?;
            let total = n.load(Ordering::SeqCst);
            if total != 40_000 || v != 42 {
                return Err(format!("count {total}, channel {v}"));
            }
            Ok("4 threads x 10000 atomic adds, channel".into())
        },
        &mut failed,
    );
    // std blocks with FUTEX_WAIT_BITSET (Linux 2.6.25); on this kernel it fails and every wait would return at
    // once (busy loops): zd1-compat translates it. A 50 ms wait must take about 50 ms.
    check(
        "futex",
        || {
            let pair = (std::sync::Mutex::new(()), std::sync::Condvar::new());
            let t = Instant::now();
            let g = pair.0.lock().unwrap();
            let _ = pair.1.wait_timeout(g, Duration::from_millis(50)).unwrap();
            let ms = t.elapsed().as_millis();
            if ms < 40 {
                Err(format!("a 50 ms Condvar wait returned after {ms} ms: futex waits spin"))
            } else {
                Ok(format!("50 ms wait took {ms} ms"))
            }
        },
        &mut failed,
    );
    check(
        "tcp",
        || {
            let l = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
            let port = l.local_addr().map_err(|e| e.to_string())?.port();
            let srv = std::thread::spawn(move || -> std::io::Result<()> {
                let (mut s, _) = l.accept()?;
                let mut b = [0u8; 5];
                s.read_exact(&mut b)?;
                s.write_all(&b)
            });
            let mut c = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
            c.write_all(b"hello").map_err(|e| e.to_string())?;
            let mut b = [0u8; 5];
            c.read_exact(&mut b).map_err(|e| e.to_string())?;
            srv.join().map_err(|_| "join".to_string())?.map_err(|e| e.to_string())?;
            if &b != b"hello" {
                return Err(format!("echo {b:?}"));
            }
            Ok(format!("echo over 127.0.0.1:{port}"))
        },
        &mut failed,
    );
    check(
        "files",
        || {
            let p = "/tmp/zd1-selftest";
            std::fs::write(p, b"zd1").map_err(|e| e.to_string())?;
            let back = std::fs::read(p).map_err(|e| e.to_string())?;
            std::fs::remove_file(p).map_err(|e| e.to_string())?;
            if back != b"zd1" {
                return Err("read back".into());
            }
            Ok("write/read/remove in /tmp".into())
        },
        &mut failed,
    );
    check(
        "process",
        || {
            let o =
                std::process::Command::new("/bin/busybox").args(["uname", "-r"]).output().map_err(|e| e.to_string())?;
            if !o.status.success() {
                return Err(format!("{:?}", o.status));
            }
            Ok(format!("busybox uname -r = {}", String::from_utf8_lossy(&o.stdout).trim()))
        },
        &mut failed,
    );
    check(
        "random",
        || {
            let mut b = [0u8; 16];
            std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).map_err(|e| e.to_string())?;
            let s = std::collections::hash_map::RandomState::new(); // std's own seed (getrandom or /dev/urandom)
            let _ = std::hash::BuildHasher::hash_one(&s, 1u32);
            if b.iter().all(|&x| x == 0) {
                return Err("all zero".into());
            }
            Ok("/dev/urandom, std RandomState".into())
        },
        &mut failed,
    );
    check(
        "proc",
        || {
            let m = std::fs::read_to_string("/proc/meminfo").map_err(|e| e.to_string())?;
            let total = m.lines().next().unwrap_or("").to_string();
            Ok(total.split_whitespace().collect::<Vec<_>>().join(" "))
        },
        &mut failed,
    );
    if failed == 0 {
        println!("SELFTEST PASS");
    } else {
        println!("SELFTEST FAIL ({failed})");
        std::process::exit(1);
    }
}
