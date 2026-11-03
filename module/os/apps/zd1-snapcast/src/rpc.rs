//! The speaker's keys -> the stream: snapserver's JSON-RPC control port (TCP, one JSON object per line; snapserver's
//! default is the stream port + 1 = 1705),
//! `Stream.Control` on the stream our group plays. Only some stream sources take it (librespot, MPD, ...); a pipe
//! source answers with an error, and the caller falls back to muting locally.
//!
//! No JSON library (binary size): `Server.GetStatus` lists groups with their keys in order (`clients`, `id`, `muted`,
//! `name`, `stream_id`), so our group's stream is the first `"stream_id"` after our client id.

use super::proto::json_escape;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

/// `command`: play, pause, playPause, next, previous. Ok when the server did it.
pub fn control(host: &str, port: u16, client_id: &str, command: &str) -> Result<(), String> {
    let s = TcpStream::connect((host, port)).map_err(|e| format!("{host}:{port}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok();
    let mut w = s.try_clone().map_err(|e| e.to_string())?;
    let mut r = BufReader::new(s);
    let status = call(&mut w, &mut r, 1, r#""method":"Server.GetStatus""#)?;
    let stream = stream_of(&status, client_id).ok_or_else(|| format!("no group with client {client_id}"))?;
    let reply = call(
        &mut w,
        &mut r,
        2,
        &format!(r#""method":"Stream.Control","params":{{"id":"{}","command":"{command}"}}"#, json_escape(&stream)),
    )?;
    if reply.contains(r#""error""#) {
        Err(field(&reply, "data").or_else(|| field(&reply, "message")).unwrap_or_else(|| "error".into()))
    } else {
        Ok(())
    }
}

/// One request; the response line with our id (notifications in between are skipped).
fn call(w: &mut TcpStream, r: &mut BufReader<TcpStream>, id: u32, body: &str) -> Result<String, String> {
    w.write_all(format!("{{\"id\":{id},\"jsonrpc\":\"2.0\",{body}}}\n").as_bytes()).map_err(|e| e.to_string())?;
    let tag = format!("\"id\":{id}");
    let mut line = String::new();
    for _ in 0..32 {
        line.clear();
        if r.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("connection closed".into());
        }
        let l = line.replace(' ', "");
        if [',', '}'].iter().any(|c| l.contains(&format!("{tag}{c}"))) {
            return Ok(line);
        }
    }
    Err("no response".into())
}

/// The stream id of the group that has `client_id`.
pub fn stream_of(status: &str, client_id: &str) -> Option<String> {
    let at = status
        .find(&format!("\"id\":\"{}\"", json_escape(client_id)))
        .or_else(|| status.find(&format!("\"id\": \"{}\"", json_escape(client_id))))?;
    field(&status[at..], "stream_id")
}

/// The first `"key":"value"` string at or after the start of `s`.
fn field(s: &str, key: &str) -> Option<String> {
    let k = format!("\"{key}\"");
    let rest = &s[s.find(&k)? + k.len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            c => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // trimmed from snapserver 0.29's Server.GetStatus
    const STATUS: &str = r#"{"id":1,"jsonrpc":"2.0","result":{"server":{"groups":[{"clients":[{"config":{"instance":1,"latency":0,"name":"","volume":{"muted":false,"percent":74}},"connected":true,"host":{"arch":"x86_64","ip":"127.0.0.1","mac":"00:21:6a:7d:74:fc","name":"kitchen","os":"Arch Linux"},"id":"00:21:6a:7d:74:fc","lastSeen":{"sec":1488025901,"usec":864472},"snapclient":{"name":"Snapclient","protocolVersion":2,"version":"0.10.0"}}],"id":"4dcc4e3b-c699-a04b-7f0c-8260d23c43e1","muted":false,"name":"","stream_id":"pipe"},{"clients":[{"config":{"instance":1,"latency":0,"name":"","volume":{"muted":false,"percent":48}},"connected":true,"host":{"arch":"armv5te","ip":"192.0.2.30","mac":"02:5a:d1:00:00:01","name":"Zeppelin Air 000001","os":"zd1 module OS"},"id":"02:5a:d1:00:00:01","lastSeen":{"sec":1488025905,"usec":1},"snapclient":{"name":"Snapclient","protocolVersion":2,"version":"0.35.0"}}],"id":"c5c4ad30-77b6-11ee-9b86-0242ac120002","muted":false,"name":"","stream_id":"Spotify"}],"streams":[{"id":"pipe"},{"id":"Spotify"}]}}}"#;

    #[test]
    fn our_group_s_stream() {
        assert_eq!(stream_of(STATUS, "02:5a:d1:00:00:01").as_deref(), Some("Spotify"));
        assert_eq!(stream_of(STATUS, "00:21:6a:7d:74:fc").as_deref(), Some("pipe"));
        assert_eq!(stream_of(STATUS, "00:00:00:00:00:00"), None);
    }

    #[test]
    fn errors_and_escapes() {
        let e = r#"{"error":{"code":-32603,"data":"Stream can not be controlled","message":"Internal error"},"id":2,"jsonrpc":"2.0"}"#;
        assert_eq!(field(e, "message").as_deref(), Some("Internal error"));
        assert_eq!(field(r#"{"a" : "x\"y"}"#, "a").as_deref(), Some("x\"y"));
        assert_eq!(field(r#"{"a":1}"#, "a"), None);
    }
}
