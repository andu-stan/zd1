//! The supervisor: starts the services in order, reaps every child, restarts what ended, answers its control socket, and stops
//! everything for a reboot. One thread, one loop: poll(2) on the signals (a signalfd), the control socket and the
//! pipes of the services' output (into their logs, which it keeps small: RAM), with the next timer as its timeout.

use super::services::{self, Command, Ctx, Lifetime, Output, Service, Start};
use super::start::Boot;
use super::{Profile, say};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// The services' PATH, and where init looks for a program.
pub const PATH: &str = "/sbin:/bin:/usr/sbin:/usr/bin";
/// How long a stopping service gets between SIGTERM and SIGKILL.
const GRACE: Duration = Duration::from_secs(3);
/// A daemon that ends sooner than this after starting is restarted ever later (up to a minute).
const STEADY: Duration = Duration::from_secs(10);
/// A service's log (/var/log, a tmpfs: RAM) at this size becomes NAME.log.1: each service holds at most twice this.
const LOG_LIMIT: u64 = 32 * 1024;
/// Reads of a service's pipe (4 KB each) per turn of the loop: one that floods doesn't hold the others.
const PUMP_READS: usize = 32;
/// A line for the kernel log is cut here (printk takes ~1 KB).
const KMSG_LINE: usize = 1000;
/// A changed setting restarts the services that use it this long after the last change: a burst of keys (`zd1
/// wifi` writes the key, the network, the mode) is one restart, not one per key.
const SETTLE: Duration = Duration::from_millis(1500);
/// How long a control client has to send its request; a request's longest line (`changed` with every key).
const REQUEST_WAIT: Duration = Duration::from_secs(1);
const REQUEST_MAX: usize = 2048;

/// What follows a stop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Then {
    /// Start it again (a restart).
    Start,
    /// Leave it stopped.
    Stay,
    /// One of its processes ended by itself and the others were stopped: like the service ending.
    Ended,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// Not reached yet in the start order.
    Pending,
    /// Started again (or for the first time) at this moment.
    Waiting(Instant),
    Running,
    Stopping {
        deadline: Instant,
        then: Then,
    },
    /// Not running, and not to be started by itself: off by its settings, stopped, ended (a one-off), or failed.
    Idle,
}

struct Live {
    svc: &'static Service,
    state: State,
    pids: Vec<i32>,
    starts: u32,
    /// [`Start::Later`]s in a row (the service's `start` sees it).
    later: u32,
    /// Ends in a row soon after starting (a daemon), or failures in a row (a one-off): the back-off.
    failed: u32,
    /// Why it isn't running, or how it last ended.
    note: String,
    since: Instant,
    /// How the last of its processes ended ("exit status 1", "killed by signal 9").
    ended: String,
    /// Output::Log and LogAndKernel: the read end of its output, and what has come of the current line (the kernel's).
    pipe: Option<(OwnedFd, Vec<u8>)>,
    /// A setting it uses changed: restarted at this moment unless another change moves it.
    restart_at: Option<Instant>,
}

/// A control client (zd1_svc::request) whose request hasn't all come yet: read as it comes, never waited for.
struct Client {
    s: UnixStream,
    line: Vec<u8>,
    deadline: Instant,
}

struct Supervisor {
    profile: Profile,
    boot: Boot,
    live: Vec<Live>,
    /// The debug services' mode from the MCU (zd1-linkd): None = the image's default.
    debug: Option<bool>,
    cursor: usize,
    announced: bool,
    clients: Vec<Client>,
}

pub fn run(profile: Profile, boot: Boot) -> ! {
    let signals = match Signals::block() {
        Ok(s) => s,
        Err(e) => {
            say(&format!("signals: {e}: nothing can be supervised"));
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
    };
    let _ = std::fs::remove_file(zd1_svc::SOCKET);
    let control = UnixListener::bind(zd1_svc::SOCKET).and_then(|l| l.set_nonblocking(true).map(|_| l));
    if let Err(e) = &control {
        say(&format!("{}: {e}: nothing can reach us", zd1_svc::SOCKET));
    }
    let now = Instant::now();
    let live = services::table(profile)
        .iter()
        .map(|svc| Live {
            svc,
            state: State::Pending,
            pids: Vec::new(),
            starts: 0,
            later: 0,
            failed: 0,
            note: String::new(),
            since: now,
            ended: String::new(),
            pipe: None,
            restart_at: None,
        })
        .collect();
    let mut s = Supervisor { profile, boot, live, debug: None, cursor: 0, announced: false, clients: Vec::new() };
    s.advance();
    loop {
        let now = Instant::now();
        let next = s
            .live
            .iter()
            .filter_map(|l| {
                match l.state {
                    State::Waiting(t) => Some(t),
                    State::Stopping { deadline, .. } => Some(deadline),
                    _ => None,
                }
                .into_iter()
                .chain(l.restart_at)
                .min()
            })
            .chain(s.clients.iter().map(|c| c.deadline))
            .min();
        let timeout = next.map(|t| t.saturating_duration_since(now).as_millis().min(60_000) as i32).unwrap_or(60_000);
        let mut fds = vec![libc::pollfd { fd: signals.fd.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        if let Ok(l) = &control {
            fds.push(libc::pollfd { fd: l.as_raw_fd(), events: libc::POLLIN, revents: 0 });
        }
        let piped: Vec<usize> = (0..s.live.len()).filter(|&i| s.live[i].pipe.is_some()).collect();
        for &i in &piped {
            if let Some((fd, _)) = &s.live[i].pipe {
                fds.push(libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 });
            }
        }
        for c in &s.clients {
            fds.push(libc::pollfd { fd: c.s.as_raw_fd(), events: libc::POLLIN, revents: 0 });
        }
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout) };
        for sig in signals.pending() {
            match sig {
                libc::SIGCHLD => s.reap(),
                libc::SIGTERM => s.shutdown("reboot", libc::RB_AUTOBOOT),
                libc::SIGUSR2 => s.shutdown("poweroff", libc::RB_POWER_OFF),
                libc::SIGUSR1 => s.shutdown("halt", libc::RB_HALT_SYSTEM),
                _ => {}
            }
        }
        if let Ok(l) = &control {
            while let Ok((c, _)) = l.accept() {
                if c.set_nonblocking(true).is_ok() {
                    s.clients.push(Client { s: c, line: Vec::new(), deadline: Instant::now() + REQUEST_WAIT });
                }
            }
        }
        s.requests();
        for i in piped {
            s.pump(i);
        }
        s.timers();
        s.advance();
    }
}

impl Supervisor {
    /// Start the services in order; a one-off that is waited for holds the rest until it has ended. The console's
    /// shell comes after the "up" line (its prompt would land in the middle of it).
    fn advance(&mut self) {
        while self.cursor < self.live.len() {
            let i = self.cursor;
            if self.live[i].svc.output == Output::Shell && !self.announced {
                self.announced = true;
                services::up(self.profile);
            }
            if self.live[i].state == State::Pending {
                self.start(i);
            }
            let l = &self.live[i];
            let waited = matches!(l.svc.lifetime, Lifetime::Once { wait: true, .. });
            if waited && matches!(l.state, State::Running | State::Waiting(_)) {
                return;
            }
            self.cursor += 1;
        }
        if !self.announced {
            self.announced = true;
            services::up(self.profile);
        }
    }

    fn start(&mut self, i: usize) {
        let ctx =
            Ctx { boot: &self.boot, settings: zd1_settings::load(), debug: self.debug, attempt: self.live[i].later };
        let plan = (self.live[i].svc.start)(&ctx);
        let l = &mut self.live[i];
        let name = l.svc.name;
        match plan {
            Start::Off(why) => {
                if l.state != State::Pending {
                    say(&format!("{name}: {why}"));
                } // at boot, `zd1-ctl status` says it
                l.state = State::Idle;
                l.note = why;
            }
            Start::Later(after, why) => {
                l.later += 1;
                if l.later > 1 || after >= Duration::from_secs(1) {
                    say(&format!("{name}: {why}; again in {}", human(after)));
                }
                l.state = State::Waiting(Instant::now() + after);
                l.note = why;
            }
            Start::Run(commands) => {
                let restarted = l.starts > 0 && matches!(l.svc.lifetime, Lifetime::Daemon { .. });
                l.later = 0;
                l.pids.clear();
                l.pipe = None;
                let mut sink = match Sink::open(l.svc.output) {
                    Ok(s) => s,
                    Err(e) => {
                        say(&format!("{name}: its output: {e}"));
                        Sink::Null
                    }
                };
                for c in &commands {
                    match spawn(c, &sink) {
                        Ok(pid) => l.pids.push(pid),
                        Err(e) => say(&format!("{name}: {}: {e}", c.argv[0])),
                    }
                }
                if let Sink::Pipe(r, _) = &mut sink {
                    l.pipe = r.take().map(|fd| (fd, Vec::new()));
                }
                l.since = Instant::now();
                if l.pids.is_empty() {
                    l.note = "didn't start".into();
                    l.ended = l.note.clone();
                    l.state = State::Waiting(Instant::now() + Duration::from_secs(10));
                    return;
                }
                l.starts += 1;
                l.state = State::Running;
                l.note = String::new();
                if restarted {
                    say(&format!("{name}: started again"));
                }
            }
        }
    }

    /// Every child that has ended: PID 1 also inherits the orphans, which are only reaped.
    fn reap(&mut self) {
        loop {
            let mut status = 0;
            let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if pid <= 0 {
                return;
            }
            let Some(i) = self.live.iter().position(|l| l.pids.contains(&pid)) else { continue };
            let l = &mut self.live[i];
            l.pids.retain(|&p| p != pid);
            l.ended = how(status);
            if !l.pids.is_empty() {
                if l.state == State::Running {
                    // one of a service's processes ended: the service has, the others go too
                    for &p in &l.pids {
                        unsafe {
                            libc::kill(p, libc::SIGTERM);
                        }
                    }
                    l.state = State::Stopping { deadline: Instant::now() + GRACE, then: Then::Ended };
                }
                continue;
            }
            self.ended(i, status);
        }
    }

    /// The last of a service's processes has ended.
    fn ended(&mut self, i: usize, status: i32) {
        self.pump(i);
        let l = &mut self.live[i];
        let svc = l.svc;
        l.pipe = None;
        let ran = l.since.elapsed();
        let how = l.ended.clone();
        log_end(svc, &how);
        if let Some(stop) = svc.stop {
            stop();
        }
        match (l.state, &svc.lifetime) {
            (State::Stopping { then: Then::Start, .. }, _) => {
                l.state = State::Pending;
                self.start(i);
            }
            (State::Stopping { then: Then::Stay, .. }, _) => {
                l.state = State::Idle;
                l.note = "stopped".into();
            }
            (_, Lifetime::Daemon { again_after }) => {
                l.failed = if ran >= STEADY { 0 } else { l.failed + 1 };
                let after = backoff(*again_after, l.failed);
                say(&format!("{}: {how}; again in {}", svc.name, human(after)));
                l.state = State::Waiting(Instant::now() + after);
                l.note = how;
            }
            (_, Lifetime::Once { retry, .. }) => {
                let ok = libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0;
                if ok {
                    l.failed = 0;
                    l.state = State::Idle;
                    l.note = format!("done ({})", human(ran));
                } else if let Some(b) = retry {
                    let after = b.after(l.failed);
                    l.failed += 1;
                    l.state = State::Waiting(Instant::now() + after);
                    l.note = how;
                } else {
                    say(&format!("{}: {how} (/var/log/{}.log)", svc.name, svc.name));
                    l.state = State::Idle;
                    l.note = how;
                }
            }
        }
    }

    fn timers(&mut self) {
        let now = Instant::now();
        for i in 0..self.live.len() {
            if self.live[i].restart_at.is_some_and(|t| t <= now) {
                self.live[i].restart_at = None;
                say(&format!("{}: its settings changed: restarting", self.live[i].svc.name));
                self.restart(i);
            }
            match self.live[i].state {
                State::Waiting(t) if t <= now => self.start(i),
                State::Stopping { deadline, then } if deadline <= now => {
                    for &p in &self.live[i].pids {
                        unsafe {
                            libc::kill(p, libc::SIGKILL);
                        }
                    }
                    self.live[i].state = State::Stopping { deadline: now + GRACE, then };
                }
                _ => {}
            }
        }
    }

    fn stop(&mut self, i: usize, then: Then) {
        let l = &mut self.live[i];
        if l.pids.is_empty() {
            if let Some(stop) = l.svc.stop
                && l.state == State::Running
            {
                stop();
            }
            l.state = State::Idle;
            l.note = "stopped".into();
            if then == Then::Start {
                l.later = 0;
                self.start(i);
            }
            return;
        }
        for &p in &l.pids {
            unsafe {
                libc::kill(p, libc::SIGTERM);
            }
        }
        l.state = State::Stopping { deadline: Instant::now() + GRACE, then };
        l.note.clear();
    }

    /// Restart a service, or start it if it isn't running (a setting may have switched it on). One still in the
    /// start order is left to it.
    fn restart(&mut self, i: usize) {
        match self.live[i].state {
            State::Pending => {}
            State::Running | State::Stopping { .. } => self.stop(i, Then::Start),
            State::Waiting(_) | State::Idle => {
                self.live[i].later = 0;
                self.start(i);
            }
        }
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.live.iter().position(|l| l.svc.name == name)
    }

    /// What the control clients have sent: a whole line (or the client's end of it) is answered and the connection
    /// closed; one that takes longer than REQUEST_WAIT, or sends more than a line, is dropped unanswered.
    fn requests(&mut self) {
        let now = Instant::now();
        let mut done = Vec::new();
        self.clients.retain_mut(|c| {
            let mut buf = [0u8; 512];
            loop {
                match (&c.s).read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => c.line.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if c.line.contains(&b'\n') {
                            break;
                        }
                        return c.line.len() <= REQUEST_MAX && now < c.deadline;
                    }
                    Err(_) => return false,
                }
            }
            let end = c.line.iter().position(|&b| b == b'\n').unwrap_or(c.line.len());
            if end <= REQUEST_MAX {
                done.push((c.s.try_clone(), String::from_utf8_lossy(&c.line[..end]).into_owned()));
            }
            false
        });
        for (c, line) in done {
            let reply = self.answer(&line);
            if let Ok(mut c) = c {
                let _ = c.write_all(reply.as_bytes()); // a few KB at most: the socket's buffer takes it
            }
        }
    }

    /// A control request, one line: the answer.
    fn answer(&mut self, line: &str) -> String {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            [] | ["status"] => format!("ok\n{}", self.status()),
            [verb @ ("start" | "stop" | "restart"), name] => match self.find(name) {
                None => format!("error no service {name} (zd1-ctl status)"),
                Some(i) => {
                    match *verb {
                        "start" if matches!(self.live[i].state, State::Running | State::Stopping { .. }) => {}
                        "start" => {
                            self.live[i].later = 0;
                            self.start(i);
                        }
                        "stop" => self.stop(i, Then::Stay),
                        _ => self.restart(i),
                    }
                    format!("ok {verb} {name}\n")
                }
            },
            ["changed", keys @ ..] if !keys.is_empty() => {
                // the restart once the burst has settled (SETTLE): each key of a burst moves it, one restart
                let uses = |s: &str, key: &str| if s.ends_with('.') { key.starts_with(s) } else { key == s };
                let users: Vec<usize> = (0..self.live.len())
                    .filter(|&i| self.live[i].svc.settings.iter().any(|s| keys.iter().any(|k| uses(s, k))))
                    .collect();
                let key = if keys.len() == 1 {
                    keys[0].to_string()
                } else {
                    format!("{} and {} more", keys[0], keys.len() - 1)
                };
                let at = Instant::now() + SETTLE;
                for &i in &users {
                    self.live[i].restart_at = Some(at);
                }
                let names: Vec<&str> = users.iter().map(|&i| self.live[i].svc.name).collect();
                say(&format!(
                    "setting {key} changed{}",
                    if names.is_empty() {
                        String::new()
                    } else {
                        format!(": {} restart once the changes settle", names.join(", "))
                    }
                ));
                format!("ok {}\n", if names.is_empty() { "nothing uses it".into() } else { names.join(" ") })
            }
            ["debug", mode @ ("on" | "off" | "default")] => {
                self.debug = match *mode {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                };
                say(&format!("debug services: {mode} (from the MCU)"));
                if let Some(i) = self.find("debug") {
                    self.restart(i);
                }
                format!("ok debug {mode}\n")
            }
            _ => "error status | start|stop|restart SERVICE | changed KEY... | debug on|off|default".into(),
        }
    }

    fn status(&self) -> String {
        let mut o = format!("{:<11} {:<34} {:>6} {:>6}  {}\n", "service", "state", "pid", "starts", "what");
        for l in &self.live {
            let state = match l.state {
                State::Pending => "not started yet".to_string(),
                State::Waiting(t) => format!(
                    "again in {}{}",
                    human(t.saturating_duration_since(Instant::now())),
                    if l.note.is_empty() { String::new() } else { format!(": {}", l.note) }
                ),
                State::Running => format!("running {}", human(l.since.elapsed())),
                State::Stopping { .. } => "stopping".into(),
                State::Idle => l.note.clone(),
            };
            let pid = l.pids.first().map(|p| p.to_string()).unwrap_or_default();
            let state: String = state.chars().take(34).collect();
            o += &format!("{:<11} {:<34} {:>6} {:>6}  {}\n", l.svc.name, state, pid, l.starts, l.svc.about);
        }
        o
    }

    /// A service's output: everything to its log file, and for LogAndKernel whole lines to /dev/kmsg.
    fn pump(&mut self, i: usize) {
        let l = &mut self.live[i];
        let kernel = l.svc.output == Output::LogAndKernel;
        let Some((fd, partial)) = &mut l.pipe else { return };
        let mut buf = [0u8; 4096];
        let mut closed = false;
        for _ in 0..PUMP_READS {
            let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr() as *mut _, buf.len()) };
            closed = n == 0;
            if n <= 0 {
                break;
            }
            let chunk = &buf[..n as usize];
            append(l.svc.name, chunk);
            if !kernel {
                continue;
            }
            partial.extend_from_slice(chunk);
            loop {
                let end = match partial.iter().take(KMSG_LINE).position(|&b| b == b'\n') {
                    Some(end) => end,
                    None if partial.len() >= KMSG_LINE => KMSG_LINE - 1,
                    None => break,
                };
                let mut line: Vec<u8> = partial.drain(..=end).collect();
                if line.last() != Some(&b'\n') {
                    line.push(b'\n');
                }
                zd1_sys::kmsg_raw(&line);
            }
        }
        if closed {
            l.pipe = None;
        } // every writer has closed it: poll would report it again and again
    }

    /// Stop everything and restart (or halt) the chip: what busybox's reboot, poweroff and halt ask of PID 1.
    fn shutdown(&mut self, what: &str, how: libc::c_int) -> ! {
        say(&format!("{what}: stopping everything"));
        unsafe {
            libc::kill(-1, libc::SIGTERM);
        }
        let until = Instant::now() + GRACE;
        while Instant::now() < until {
            while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
            std::thread::sleep(Duration::from_millis(100));
        }
        unsafe {
            libc::kill(-1, libc::SIGKILL);
            libc::sync();
            libc::umount(c"/data".as_ptr());
            libc::sync();
            libc::reboot(how);
        }
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
}

/// A daemon's restart delay: its own, doubled for each quick end in a row after the first, up to a minute.
fn backoff(again_after: Duration, quick_ends: u32) -> Duration {
    (again_after * 2u32.saturating_pow(quick_ends.saturating_sub(1).min(6))).min(Duration::from_secs(60))
}

fn log_path(name: &str) -> String {
    format!("/var/log/{name}.log")
}

/// Onto a service's log; one that has reached LOG_LIMIT becomes NAME.log.1 (what was there goes) and the next
/// output starts a new one.
fn append(name: &str, bytes: &[u8]) {
    let path = log_path(name);
    let Ok(mut f) = OpenOptions::new().append(true).create(true).open(&path) else { return };
    let _ = f.write_all(bytes);
    if f.metadata().is_ok_and(|m| m.len() >= LOG_LIMIT) {
        let _ = std::fs::rename(&path, format!("{path}.1"));
    }
}

/// How it ended, at the end of its log; a daemon's log then becomes NAME.log.1 (the next run starts a new one).
fn log_end(svc: &Service, how: &str) {
    if !matches!(svc.output, Output::Log | Output::LogAndKernel) {
        return;
    }
    append(svc.name, format!("-- {how}\n").as_bytes());
    let path = log_path(svc.name);
    if matches!(svc.lifetime, Lifetime::Daemon { .. }) {
        let _ = std::fs::rename(&path, format!("{path}.1"));
    }
}

fn how(status: i32) -> String {
    if libc::WIFEXITED(status) {
        format!("exit status {}", libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        format!("killed by signal {}", libc::WTERMSIG(status))
    } else {
        format!("status {status}")
    }
}

fn human(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, s / 60 % 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else if s >= 1 {
        format!("{s} s")
    } else {
        format!("{} ms", d.as_millis())
    }
}

/// Where a run's output goes.
enum Sink {
    Null,
    /// The read end (ours, taken once spawned), the write end (the processes').
    Pipe(Option<OwnedFd>, OwnedFd),
    Console,
    Shell,
}

impl Sink {
    fn open(output: Output) -> std::io::Result<Sink> {
        Ok(match output {
            Output::Log | Output::LogAndKernel => {
                let mut fds = [0; 2];
                if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                for fd in fds {
                    unsafe {
                        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                    }
                }
                unsafe {
                    libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK);
                }
                unsafe { Sink::Pipe(Some(OwnedFd::from_raw_fd(fds[0])), OwnedFd::from_raw_fd(fds[1])) }
            }
            Output::Console => Sink::Console,
            Output::Shell => Sink::Shell,
        })
    }

    fn stdio(&self) -> std::io::Result<(Stdio, Stdio)> {
        Ok(match self {
            Sink::Null | Sink::Shell => (Stdio::null(), Stdio::null()),
            Sink::Pipe(_, w) => (w.try_clone()?.into(), w.try_clone()?.into()),
            Sink::Console => (Stdio::inherit(), Stdio::inherit()),
        })
    }
}

fn spawn(c: &Command, sink: &Sink) -> std::io::Result<i32> {
    let mut p = std::process::Command::new(&c.argv[0]);
    p.args(&c.argv[1..]).env("PATH", PATH).current_dir("/").stdin(Stdio::null());
    if let Some(arg0) = c.arg0 {
        p.arg0(arg0);
    }
    let (out, err) = sink.stdio()?;
    p.stdout(out).stderr(err);
    let shell = matches!(sink, Sink::Shell);
    unsafe {
        p.pre_exec(move || {
            // what init blocks and std ignores (SIGPIPE) would otherwise be inherited: SIGTERM couldn't stop it
            let mut none: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut none);
            libc::sigprocmask(libc::SIG_SETMASK, &none, std::ptr::null_mut());
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            if shell {
                // a session of its own with the console as its terminal, so job control and Ctrl-C work there
                libc::setsid();
                let fd = libc::open(c"/dev/console".as_ptr(), libc::O_RDWR);
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                for target in 0..3 {
                    libc::dup2(fd, target);
                }
                if fd > 2 {
                    libc::close(fd);
                }
                libc::ioctl(0, libc::TIOCSCTTY as _, 1);
            }
            Ok(())
        });
    }
    Ok(p.spawn()?.id() as i32)
}

/// The signals PID 1 acts on, blocked and read from a signalfd.
struct Signals {
    fd: OwnedFd,
}

impl Signals {
    fn block() -> std::io::Result<Signals> {
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            for s in [libc::SIGCHLD, libc::SIGTERM, libc::SIGUSR1, libc::SIGUSR2, libc::SIGINT, libc::SIGHUP] {
                libc::sigaddset(&mut set, s);
            }
            libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
            let fd = libc::signalfd(-1, &set, 0); // flags 0: musl falls back to 2.6.24's signalfd
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
            Ok(Signals { fd: OwnedFd::from_raw_fd(fd) })
        }
    }

    fn pending(&self) -> Vec<i32> {
        let mut out = Vec::new();
        let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::signalfd_siginfo>();
        while unsafe { libc::read(self.fd.as_raw_fd(), &mut info as *mut _ as *mut _, size) } == size as isize {
            out.push(info.ssi_signo as i32);
        }
        out
    }
}
