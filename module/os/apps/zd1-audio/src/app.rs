//! What each receiver's `main` starts with: its command line, its log and its name.

use std::str::FromStr;

/// A receiver's command line: `--option VALUE` and `--flag`, as the program declares them; anything else prints its
/// usage and exits.
pub struct Args {
    values: Vec<(String, String)>,
    flags: Vec<String>,
}

impl Args {
    pub fn parse(usage: &str, options: &[&str], flags: &[&str]) -> Args {
        let mut args = Args { values: Vec::new(), flags: Vec::new() };
        let mut it = std::env::args().skip(1);
        while let Some(a) = it.next() {
            if flags.contains(&a.as_str()) {
                args.flags.push(a);
            } else if let (true, Some(v)) = (options.contains(&a.as_str()), it.next()) {
                args.values.push((a, v));
            } else {
                eprintln!("usage: {usage}");
                std::process::exit(2);
            }
        }
        args
    }

    pub fn get(&self, option: &str) -> Option<&str> {
        self.values.iter().rev().find(|(k, _)| k == option).map(|(_, v)| v.as_str())
    }

    /// The option's value, or `default` if it isn't given or doesn't parse.
    pub fn get_or<T: FromStr>(&self, option: &str, default: T) -> T {
        self.get(option).and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    pub fn flag(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }

    /// `--name`, else "Zeppelin Air XXXXXX" from eth0's MAC, like the stock module.
    pub fn name(&self) -> String {
        match self.get("--name") {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => zd1_sys::default_name(zd1_sys::eth0_mac()),
        }
    }

    /// `--device`, else the module's output (snd-sulu).
    pub fn device(&self) -> String {
        self.get("--device").unwrap_or("hw:0,0").to_string()
    }

    /// The log to stderr (init keeps it in /var/log), `-v` with the debug lines.
    pub fn log(&self) {
        tracing_subscriber::fmt()
            .with_max_level(if self.flag("-v") { tracing::Level::DEBUG } else { tracing::Level::INFO })
            .with_ansi(false)
            // a line that can't be written is lost: tracing-subscriber's own report of it is an eprintln!, which
            // panics when stderr fails too (both in one full file: a receiver died so when /tmp filled)
            .log_internal_errors(false)
            .init();
    }
}

/// eth0's MAC, or a fixed local one off the unit (no eth0).
pub fn mac() -> [u8; 6] {
    zd1_sys::eth0_mac().unwrap_or([0x02, 0, 0, 0, 0, 1])
}
