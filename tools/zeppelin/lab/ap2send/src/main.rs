//! ap2send: plays raw AAC-LC frames (module/codecs/aac/tools/m4a2frames.py output) to an AirPlay 2 receiver as buffered audio (stream
//! type 103), the way an iPhone plays music: transient pair-setup (PIN 3939), the encrypted RTSP channel, SETUP of a
//! type 103 stream, frames over TCP encrypted with ChaCha20-Poly1305, SETRATEANCHORTIME to start playback. It keeps
//! LEAD seconds of audio ahead of real time (iOS runs far ahead, up to the receiver's audioBufferSize).
//!
//!   ap2send HOST[:PORT] FRAMES [--lead SECS] [--secs N]
//!   ap2send HOST[:PORT] PCM --alac48 ...   PCM (s32le stereo at 48 kHz, 24 bits used) as ALAC/48000/S24/2 instead:
//!                                          the lossless stream a HomePod or an Apple TV sends, 352 frames a packet
//!                                          (uncompressed ALAC frames: our encoder is FFmpeg's, 4096 frames only);
//!                                          or an ALAC vector (module/warp0/vectors, ALACGV01: Apple's encoder's
//!                                          frames, sent as they are)
//!   --aac48: the FRAMES are AAC-LC at 48 kHz (AAC-LC/48000, as an Apple TV sends 48 kHz lossy audio)
//!   ap2send HOST --ptp-only SECS      only be a PTP master to HOST (ptp.rs), for measuring its clock following
//!   --ptp: also be the PTP master while streaming and anchor playback on that clock, --start SECS after
//!   SETRATEANCHORTIME (default 1); --anchor-out FILE writes the anchor (JSON: net_ns, rtp, first_ts) for
//!   tools/zeppelin/lab/synctest.py; --pause A B: pause A s into playback (SETRATEANCHORTIME rate 0) and resume B s
//!   later with a new anchor (as iOS does), for checking that the receiver stops and restarts on time

use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use num_bigint::BigUint;
use sha2::{Digest, Sha512};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

mod ptp;
mod wire;
use wire::{EncryptedChannel, TlvType, TlvValues};

const N_HEX: &str = "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3BE39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864D87602733EC86A64521F2B18177B200CBBE117577A615D6C770988C0BAD946E208E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF";

fn sha512(d: &[u8]) -> [u8; 64] {
    Sha512::digest(d).into()
}
fn be(n: &BigUint) -> Vec<u8> {
    let b = n.to_bytes_be();
    if b.is_empty() { vec![0] } else { b }
}
fn padded(n: &BigUint, len: usize) -> Vec<u8> {
    let b = n.to_bytes_be();
    let mut p = vec![0u8; len.saturating_sub(b.len())];
    p.extend(b);
    p
}
fn h_pad(a: &BigUint, b: &BigUint) -> BigUint {
    let mut v = padded(a, 384);
    v.extend(padded(b, 384));
    BigUint::from_bytes_be(&sha512(&v))
}

/// An RTSP connection, plaintext until `enc` is set.
struct Rtsp {
    s: TcpStream,
    enc: Option<EncryptedChannel>,
    cseq: u32,
    pending: Vec<u8>,
}

impl Rtsp {
    fn request(&mut self, method: &str, url: &str, ctype: &str, body: &[u8], extra: &str) -> (String, Vec<u8>) {
        self.cseq += 1;
        let mut req = format!(
            "{method} {url} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: AirPlay/745.83\r\nX-Apple-HKP: 4\r\n{extra}",
            self.cseq
        )
        .into_bytes();
        if !body.is_empty() {
            req.extend(format!("Content-Type: {ctype}\r\nContent-Length: {}\r\n", body.len()).bytes());
        }
        req.extend(b"\r\n");
        req.extend(body);
        let wire = match &mut self.enc {
            Some(c) => c.encrypt_ctx.encrypt(&req).expect("encrypt"),
            None => req,
        };
        self.s.write_all(&wire).unwrap();
        self.response()
    }

    fn response(&mut self) -> (String, Vec<u8>) {
        let mut plain = Vec::new();
        loop {
            if let Some(h) = find(&plain, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&plain[..h]).to_string();
                let len = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if plain.len() >= h + 4 + len {
                    return (head, plain[h + 4..h + 4 + len].to_vec());
                }
            }
            let mut buf = [0u8; 8192];
            let n = self.s.read(&mut buf).unwrap();
            assert!(n > 0, "connection closed");
            match &mut self.enc {
                Some(c) => {
                    self.pending.extend(&buf[..n]);
                    let (p, used) = c.decrypt_ctx.decrypt(&self.pending).expect("decrypt");
                    self.pending.drain(..used);
                    plain.extend(p);
                }
                None => plain.extend(&buf[..n]),
            }
        }
    }
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn bplist(v: plist::Value) -> Vec<u8> {
    let mut out = Vec::new();
    plist::to_writer_binary(&mut out, &v).unwrap();
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--ptp-only") {
        let ip: std::net::IpAddr = args[1].split(':').next().unwrap().parse().expect("HOST as an IP address");
        let secs: f64 = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(60.0);
        println!("ap2send: PTP master (clock {:016x}) to {ip} for {secs} s", ptp::CLOCK_ID);
        ptp::serve(ip, Some(secs));
        return;
    }
    if args.len() < 3 {
        eprintln!("usage: ap2send HOST[:PORT] FRAMES [--lead SECS] [--secs N]");
        std::process::exit(2);
    }
    let host = if args[1].contains(':') { args[1].clone() } else { format!("{}:7000", args[1]) };
    let opt = |name: &str, d: f64| {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(d)
    };
    let (lead, secs) = (opt("--lead", 10.0), opt("--secs", 30.0));
    let with_ptp = args.iter().any(|a| a == "--ptp");
    let start = opt("--start", 1.0);
    let anchor_out = args.iter().position(|a| a == "--anchor-out").and_then(|i| args.get(i + 1)).cloned();
    let pause: Option<(f64, f64)> = args
        .iter()
        .position(|a| a == "--pause")
        .and_then(|i| Some((args.get(i + 1)?.parse().ok()?, args.get(i + 2)?.parse().ok()?)));
    let alac = args.iter().any(|a| a == "--alac48");
    let aac48 = args.iter().any(|a| a == "--aac48");
    // the stream: SETUP's audioFormat, compression type, frames per packet and rate; the RTP SSRC
    let (audio_format, ct, spf, sr, ssrc) = if alac {
        (0x20_0000u64, 2u64, 352u32, 48000u32, 0x1500_0000u32)
    } else if aac48 {
        (0x80_0000, 4, 1024, 48000, 0x1700_0000)
    } else {
        (0x40_0000, 4, 1024, 44100, 0x1600_0000)
    };
    let data = std::fs::read(&args[2]).expect("frames");
    let mut frames = Vec::new();
    if alac {
        frames = alac_frames(&data);
    } else {
        let mut i = 0;
        while i + 4 <= data.len() {
            let n = u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) as usize;
            frames.push(data[i + 4..i + 4 + n].to_vec());
            i += 4 + n;
        }
    }
    let per_s = sr as f64 / spf as f64; // packets a second

    let t0 = Instant::now();
    let mut r = Rtsp { s: TcpStream::connect(&host).expect("connect"), enc: None, cseq: 0, pending: Vec::new() };
    let ip = r.s.peer_addr().unwrap().ip();
    if with_ptp {
        std::thread::spawn(move || ptp::serve(ip, None));
    }

    // transient pair-setup (SRP-6a, 3072 bit, SHA-512, PIN 3939)
    let mut m1 = TlvValues::new();
    m1.add(TlvType::State as u8, &[1]);
    m1.add(TlvType::Method as u8, &[0]);
    m1.add(TlvType::Flags as u8, &[0x10]);
    let (_, body) = r.request("POST", "/pair-setup", "application/octet-stream", &m1.encode(), "");
    let m2 = TlvValues::decode(&body).expect("M2");
    let salt = BigUint::from_bytes_be(m2.get_type(TlvType::Salt).expect("salt"));
    let big_b = BigUint::from_bytes_be(m2.get_type(TlvType::PublicKey).expect("B"));
    let n = BigUint::parse_bytes(N_HEX.as_bytes(), 16).unwrap();
    let g = BigUint::from(5u32);
    let a = BigUint::from_bytes_be(&rand::random::<[u8; 32]>());
    let big_a = g.modpow(&a, &n);
    let (k, u) = (h_pad(&n, &g), h_pad(&big_a, &big_b));
    let mut xb = be(&salt);
    xb.extend(sha512(b"Pair-Setup:3939"));
    let x = BigUint::from_bytes_be(&sha512(&xb));
    let base = (&big_b + &n - (&k * g.modpow(&x, &n)) % &n) % &n;
    let session_key = sha512(&be(&base.modpow(&(&a + &u * &x), &n)));
    let (hn, hg) = (sha512(&be(&n)), sha512(&be(&g)));
    let mut h = Sha512::new();
    h.update(hn.iter().zip(hg.iter()).map(|(p, q)| p ^ q).collect::<Vec<u8>>());
    h.update(sha512(b"Pair-Setup"));
    h.update(be(&salt));
    h.update(be(&big_a));
    h.update(be(&big_b));
    h.update(session_key);
    let proof: [u8; 64] = h.finalize().into();
    let mut m3 = TlvValues::new();
    m3.add(TlvType::State as u8, &[3]);
    m3.add(TlvType::PublicKey as u8, &be(&big_a));
    m3.add(TlvType::Proof as u8, &proof);
    let (_, body) = r.request("POST", "/pair-setup", "application/octet-stream", &m3.encode(), "");
    let m4 = TlvValues::decode(&body).expect("M4");
    assert!(m4.get_type(TlvType::Error).is_none(), "pair-setup M4 error");
    println!("ap2send: paired in {:.2} s", t0.elapsed().as_secs_f64());
    r.enc = Some(
        EncryptedChannel::new(
            &session_key,
            "Control-Salt",
            "Control-Write-Encryption-Key",
            "Control-Salt",
            "Control-Read-Encryption-Key",
        )
        .unwrap(),
    );

    // SETUP: session, then the type 103 stream
    let url = format!("rtsp://{ip}/{}", rand::random::<u32>());
    let mut d = plist::Dictionary::new();
    d.insert("timingProtocol".into(), (if with_ptp { "PTP" } else { "None" }).into());
    d.insert("deviceID".into(), "02:11:22:33:44:55".into());
    d.insert("sessionUUID".into(), "8E8C9E9A-0000-4000-8000-000000000001".into());
    let (head, _) = r.request("SETUP", &url, "application/x-apple-binary-plist", &bplist(d.into()), "");
    assert!(head.contains("200"), "SETUP 1: {head}");
    let shk: [u8; 32] = rand::random();
    let mut st = plist::Dictionary::new();
    st.insert("type".into(), 103u64.into());
    st.insert("audioFormat".into(), audio_format.into());
    st.insert("ct".into(), ct.into());
    st.insert("spf".into(), (spf as u64).into());
    st.insert("sr".into(), (sr as u64).into());
    st.insert("shk".into(), plist::Value::Data(shk.to_vec()));
    st.insert("audioMode".into(), "default".into());
    let mut d = plist::Dictionary::new();
    d.insert("streams".into(), plist::Value::Array(vec![st.into()]));
    let (head, body) = r.request("SETUP", &url, "application/x-apple-binary-plist", &bplist(d.into()), "");
    assert!(head.contains("200"), "SETUP 2: {head}");
    let resp: plist::Value = plist::from_bytes(&body).expect("SETUP 2 plist");
    let data_port = resp
        .as_dictionary()
        .and_then(|d| d.get("streams"))
        .and_then(|s| s.as_array())
        .and_then(|a| a.first())
        .and_then(|s| s.as_dictionary())
        .and_then(|s| s.get("dataPort"))
        .and_then(|p| p.as_unsigned_integer())
        .expect("dataPort") as u16;
    r.request("RECORD", &url, "", &[], "");
    let mut data_sock = TcpStream::connect((ip, data_port)).expect("data port");
    println!("ap2send: stream set up (data port {data_port}) after {:.2} s", t0.elapsed().as_secs_f64());

    // frames: [u16 len][RTP header][ChaCha20-Poly1305(frame) + tag][8-byte nonce]
    let cipher = ChaCha20Poly1305::new((&shk).into());
    let send = |sock: &mut TcpStream, seq: u16, ts: u32, aac: &[u8]| {
        let mut hdr = [0u8; 12];
        hdr[0] = 0x80;
        hdr[1] = 0x60;
        hdr[2..4].copy_from_slice(&seq.to_be_bytes());
        hdr[4..8].copy_from_slice(&ts.to_be_bytes());
        hdr[8..12].copy_from_slice(&ssrc.to_be_bytes()); // the format: AAC-LC 44.1 kHz or ALAC 48 kHz 24-bit
        let trail = (seq as u64).to_le_bytes();
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&trail);
        let ct = cipher.encrypt(Nonce::from_slice(&nonce), Payload { msg: aac, aad: &hdr[4..12] }).unwrap();
        let len = (2 + 12 + ct.len() + 8) as u16;
        let mut pkt = len.to_be_bytes().to_vec();
        pkt.extend(hdr);
        pkt.extend(ct);
        pkt.extend(trail);
        sock.write_all(&pkt).unwrap();
    };
    let first_ts = 1_000u32;
    let total = ((secs * per_s) as usize).min(frames.len());
    let lead_frames = (lead * per_s) as usize;
    let mut sent = 0;
    // fill the lead, start playback, then keep the lead
    while sent < lead_frames.min(total) {
        send(&mut data_sock, sent as u16, first_ts + spf * sent as u32, &frames[sent]);
        sent += 1;
    }
    let mut d = plist::Dictionary::new();
    d.insert("rate".into(), 1u64.into());
    d.insert("rtpTime".into(), (first_ts as u64).into());
    let net_ns = if with_ptp { ptp::now_ns() + (start * 1e9) as u64 } else { 0 };
    d.insert("networkTimeSecs".into(), (net_ns / 1_000_000_000).into());
    d.insert(
        "networkTimeFrac".into(),
        (((net_ns % 1_000_000_000) as u128 * (1u128 << 64) / 1_000_000_000) as u64).into(),
    );
    if with_ptp {
        d.insert("networkTimeTimelineID".into(), ptp::CLOCK_ID.into());
    }
    if let Some(f) = &anchor_out {
        std::fs::write(f, format!("{{\"net_ns\": {net_ns}, \"rtp\": {first_ts}, \"first_ts\": {first_ts}}}")).unwrap();
    }
    r.request("SETRATEANCHORTIME", &url, "application/x-apple-binary-plist", &bplist(d.into()), "");
    let mut play_t0 = Instant::now();
    println!("ap2send: playing {total} frames ({:.0} s), {lead:.0} s ahead", total as f64 / per_s);
    let mut paused_once = false;
    let mut played_before = 0usize; // frames played before the pause
    while sent < total {
        if let Some((at, len)) = pause
            && !paused_once
            && play_t0.elapsed().as_secs_f64() >= at
        {
            paused_once = true;
            let mut d = plist::Dictionary::new();
            d.insert("rate".into(), 0u64.into());
            r.request("SETRATEANCHORTIME", &url, "application/x-apple-binary-plist", &bplist(d.into()), "");
            let playing = (at * per_s) as usize;
            println!("ap2send: paused at frame {playing} (sent {sent})");
            std::thread::sleep(Duration::from_secs_f64(len));
            // resume: the frame after the last one played starts `start` s from now
            let net_ns = ptp::now_ns() + (start * 1e9) as u64;
            let mut d = plist::Dictionary::new();
            d.insert("rate".into(), 1u64.into());
            d.insert("rtpTime".into(), ((first_ts + spf * playing as u32) as u64).into());
            d.insert("networkTimeSecs".into(), (net_ns / 1_000_000_000).into());
            d.insert(
                "networkTimeFrac".into(),
                (((net_ns % 1_000_000_000) as u128 * (1u128 << 64) / 1_000_000_000) as u64).into(),
            );
            d.insert("networkTimeTimelineID".into(), ptp::CLOCK_ID.into());
            r.request("SETRATEANCHORTIME", &url, "application/x-apple-binary-plist", &bplist(d.into()), "");
            if let Some(f) = &anchor_out {
                std::fs::write(
                    format!("{f}.resume"),
                    format!(
                        "{{\"net_ns\": {net_ns}, \"rtp\": {}, \"first_ts\": {first_ts}}}",
                        first_ts + spf * playing as u32
                    ),
                )
                .unwrap();
            }
            println!("ap2send: resumed at frame {playing}");
            played_before = playing;
            play_t0 = Instant::now();
        }
        let due = (play_t0.elapsed().as_secs_f64() * per_s) as usize + lead_frames + played_before;
        while sent < due.min(total) {
            send(&mut data_sock, sent as u16, first_ts + spf * sent as u32, &frames[sent]);
            sent += 1;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_secs_f64(lead + 1.0));
    r.request("TEARDOWN", &url, "", &[], "");
    println!("ap2send: done after {:.1} s", t0.elapsed().as_secs_f64());
}

/// s32le stereo PCM (24 bits used) as ALAC packets of 352 frames, each one uncompressed ALAC element (a stereo
/// channel pair, not compressed, the samples as 24-bit values L, R, ...) and the end tag.
fn alac_frames(pcm: &[u8]) -> Vec<Vec<u8>> {
    if pcm.starts_with(b"ALACGV01") {
        let u32at = |i: usize| u32::from_le_bytes(pcm[i..i + 4].try_into().unwrap()) as usize;
        let mut p = 20 + u32at(16);
        let n = u32at(p);
        p += 4;
        return (0..n)
            .map(|_| {
                let len = u32at(p);
                p += 4 + len;
                pcm[p - len..p].to_vec()
            })
            .collect();
    }
    let samples: Vec<i32> = pcm.as_chunks::<4>().0.iter().map(|c| i32::from_le_bytes(*c) >> 8).collect();
    samples
        .as_chunks::<{ 352 * 2 }>()
        .0
        .iter()
        .map(|f| {
            let (mut out, mut acc, mut n) = (Vec::new(), 0u64, 0u32);
            let mut put = |v: u32, bits: u32| {
                acc = (acc << bits) | (v as u64 & ((1 << bits) - 1));
                n += bits;
                while n >= 8 {
                    n -= 8;
                    out.push((acc >> n) as u8);
                }
            };
            put(1, 3); // a channel pair
            put(0, 4); // element instance
            put(0, 12);
            put(0, 1); // no sample count: a full packet
            put(0, 2); // no shifted bytes
            put(1, 1); // not compressed
            for &v in f {
                put(v as u32 & 0xff_ffff, 24);
            }
            put(7, 3); // end
            let pad = (8 - (23 + f.len() * 24 + 3) % 8) % 8;
            if pad > 0 {
                put(0, pad as u32);
            }
            out
        })
        .collect()
}
