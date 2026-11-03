//! gzip (RFC 1952) over miniz_oxide's raw inflate: the header, the deflate stream, the trailer's CRC-32 and length
//! checked at the end. A `Read`: it reads its input in chunks, so it may take bytes past the gzip member.

use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};
use std::io::{self, Read};

pub struct Gunzip<R: Read> {
    inner: R,
    state: Box<InflateState>,
    buf: Box<[u8; 16384]>,
    pos: usize,
    len: usize,
    header: bool,
    done: bool,
    crc: u32,
    size: u32,
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("gzip: {what}"))
}

impl<R: Read> Gunzip<R> {
    pub fn new(inner: R) -> Self {
        Gunzip {
            inner,
            state: InflateState::new_boxed(DataFormat::Raw),
            buf: Box::new([0; 16384]),
            pos: 0,
            len: 0,
            header: false,
            done: false,
            crc: !0,
            size: 0,
        }
    }
    fn fill(&mut self) -> io::Result<bool> {
        if self.pos < self.len {
            return Ok(true);
        }
        self.len = self.inner.read(&mut self.buf[..])?;
        self.pos = 0;
        Ok(self.len > 0)
    }
    fn byte(&mut self) -> io::Result<u8> {
        if !self.fill()? {
            return Err(bad("cut short"));
        }
        self.pos += 1;
        Ok(self.buf[self.pos - 1])
    }
    fn skip_header(&mut self) -> io::Result<()> {
        let mut h = [0u8; 10];
        for b in h.iter_mut() {
            *b = self.byte()?;
        }
        if h[0] != 0x1F || h[1] != 0x8B || h[2] != 8 {
            return Err(bad("not gzip (deflate)"));
        }
        let flg = h[3];
        if flg & 4 != 0 {
            let n = self.byte()? as usize | (self.byte()? as usize) << 8;
            for _ in 0..n {
                self.byte()?;
            }
        }
        if flg & 8 != 0 {
            while self.byte()? != 0 {}
        }
        if flg & 16 != 0 {
            while self.byte()? != 0 {}
        }
        if flg & 2 != 0 {
            self.byte()?;
            self.byte()?;
        }
        Ok(())
    }
    fn trailer(&mut self) -> io::Result<()> {
        let mut t = [0u8; 8];
        for b in t.iter_mut() {
            *b = self.byte()?;
        }
        if u32::from_le_bytes(t[..4].try_into().unwrap()) != !self.crc {
            return Err(bad("CRC-32 doesn't match"));
        }
        if u32::from_le_bytes(t[4..].try_into().unwrap()) != self.size {
            return Err(bad("length doesn't match"));
        }
        Ok(())
    }
}

impl<R: Read> Read for Gunzip<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if !self.header {
            self.skip_header()?;
            self.header = true;
        }
        while !self.done && !out.is_empty() {
            if !self.fill()? {
                return Err(bad("cut short"));
            }
            let r = inflate(&mut self.state, &self.buf[self.pos..self.len], out, MZFlush::None);
            self.pos += r.bytes_consumed;
            let n = r.bytes_written;
            self.crc = zd1_sys::crc32_update(self.crc, &out[..n]);
            self.size = self.size.wrapping_add(n as u32);
            match r.status {
                Ok(MZStatus::StreamEnd) => {
                    self.done = true;
                    self.trailer()?;
                }
                Ok(_) | Err(MZError::Buf) => {}
                Err(e) => return Err(bad(&format!("deflate: {e:?}"))),
            }
            if n > 0 {
                return Ok(n);
            }
        }
        Ok(0)
    }
}
