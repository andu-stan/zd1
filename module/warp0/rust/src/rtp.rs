//! Realtime AirPlay packets on Warp0 (the crypto image's `W0_OP_RTP_AUDIO`): decrypted (AES-128-CBC for AirPlay
//! 1, ChaCha20-Poly1305 for AirPlay 2 realtime) and ALAC-decoded, each packet a job. A packet not done in time (a
//! hang), or a job error, turns the queue off: the rest of the stream goes to the ARM ([`crate::OnWarp`]).

use crate::Jobs;
use crate::abi::*;
use crate::alac::Config;
use crate::queue::{Failed, Pipeline};
use crate::warp::{Image, Job, Warp0};
use shairplay::decoder::{AlacParams, Cipher, Decoded};

/// Packets in flight at most (each has its own buffers in the window).
const SLOTS: usize = 16;

/// One stream's packets on Warp0.
pub struct Packets {
    jobs: Pipeline,
    mode: u32,
    ap2: bool,
    conf: u32,
    key: u32,
    nonce: u32, // per buffer: 64 bytes each (nonce, aad, tag at +0, +16, +32)
}

impl Packets {
    pub fn new(cipher: &Cipher, alac: &AlacParams) -> Result<Self, String> {
        let config = Config::new(alac, &[16])?;
        let mut warp = Warp0::open(Image::Crypto)?;
        let conf = config.put(&mut warp)?;
        let key = warp.alloc(64).ok_or("window full")?;
        let nonce = warp.alloc(64 * SLOTS as u32).ok_or("window full")?;
        let (mode, ap2) = match cipher {
            Cipher::None => (W0_RTP_PLAIN, false),
            Cipher::Aes128Cbc { key: k, iv } => {
                warp.put(key, k);
                for b in 0..SLOTS as u32 {
                    warp.put(nonce + 64 * b, iv); // the IV restarts with every packet
                }
                (W0_RTP_AES_CBC, false)
            }
            Cipher::ChaCha20Poly1305 { key: k } => {
                warp.put(key, k);
                (W0_RTP_CHACHA_POLY, true)
            }
            other => return Err(format!("cipher {other:?}")),
        };
        let jobs = Pipeline::new(warp, "realtime ALAC", SLOTS, W0_RTP_MAX, W0_RTP_OUT)?;
        Ok(Packets { jobs, mode: mode | W0_RTP_ALAC, ap2, conf, key, nonce })
    }
}

impl Jobs for Packets {
    fn submit(&mut self, packet: &[u8]) -> bool {
        if packet.len() < 12 {
            return false;
        }
        let payload = if self.ap2 {
            // [header 12][ciphertext][tag 16][nonce tail 8]
            let n = packet.len();
            if n < 12 + 16 + 8 {
                return false;
            }
            &packet[12..n - 24]
        } else {
            &packet[12..]
        };
        if payload.is_empty() || payload.len() > W0_RTP_MAX as usize || self.jobs.full() {
            return false;
        }
        let nonce = self.nonce + 64 * self.jobs.next_buf() as u32;
        if self.ap2 {
            let n = packet.len();
            let mut nat = [0u8; 48]; // nonce (4 zero bytes + the 8 at the end), aad (RTP header 4..12), tag
            nat[4..12].copy_from_slice(&packet[n - 8..]);
            nat[16..24].copy_from_slice(&packet[4..12]);
            nat[32..48].copy_from_slice(&packet[n - 24..n - 8]);
            self.jobs.warp().put(nonce, &nat);
        }
        let job = Job {
            op: W0_OP_RTP_AUDIO,
            key: self.key,
            nonce,
            aad: nonce + 16,
            aadlen: 8,
            tag: nonce + 32,
            ctr: self.mode,
            conf: self.conf,
            ..Job::default()
        };
        self.jobs.submit(0, payload, job, |_, _| {})
    }

    fn collect(&mut self, done: &mut dyn FnMut(Decoded)) {
        let mut error = None;
        self.jobs.collect(&mut |_, r| match r {
            Ok(pcm) if !pcm.is_empty() => done(Decoded::S16(pcm.to_vec())),
            Ok(_) => done(Decoded::Failed),
            Err(Failed::Result { result, .. }) => {
                done(Decoded::Failed); // a packet that doesn't decrypt or decode: dropped
                if result != W0_EAUTH {
                    error.get_or_insert(result);
                }
            }
            Err(Failed::Hang) => done(Decoded::Failed),
        });
        if let Some(result) = error {
            self.jobs.turn_off(&format!("job result {result}"));
        }
    }

    fn live(&self) -> bool {
        !self.jobs.is_off()
    }

    fn full(&self) -> bool {
        self.jobs.full()
    }
}
