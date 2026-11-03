//! Talking to init (module/os/apps/zd1-init) over its socket: one request per connection, a line of text; the answer
//! is everything init writes back, `ok` or `error` first. From a shell: `zd1-ctl`.
//!
//!   status                     every service: what it is, its state, its process, how often it has started
//!   start | stop | restart S   a service by name
//!   changed KEY...             settings changed (zd1-settings): restart the services that use them
//!   debug on | off | default   the debug services' mode, from the MCU (zd1-linkd); default = the image's

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

pub const SOCKET: &str = "/run/zd1/init";

/// Send a request and wait for init's answer; Err with init's error, or why it couldn't be asked.
pub fn request(line: &str) -> Result<String, String> {
    let mut s = UnixStream::connect(SOCKET).map_err(|e| format!("init ({SOCKET}): {e}"))?;
    s.write_all(format!("{line}\n").as_bytes()).map_err(|e| e.to_string())?;
    let _ = s.shutdown(std::net::Shutdown::Write);
    let mut answer = String::new();
    s.read_to_string(&mut answer).map_err(|e| e.to_string())?;
    match answer.strip_prefix("error") {
        Some(why) => Err(why.trim().to_string()),
        None => Ok(answer.strip_prefix("ok").unwrap_or(&answer).trim_start_matches([' ', '\n']).to_string()),
    }
}

/// Settings changed (`keys`: one, or several apart by spaces): init restarts what uses them. In the background of
/// the caller: errors are only logged by init.
pub fn changed(keys: &str) -> Result<String, String> {
    request(&format!("changed {keys}"))
}
