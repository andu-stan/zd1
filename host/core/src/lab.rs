//! Measurements over the host link: the bulk pipe into DISCARD (throughput, the MCU's CRC-32 against ours).

use crate::spec::Sink;
use crate::usb::Usb;
use crate::{Mcu, Result, msg};

/// The bulk pipe test: `data` into DISCARD. A line for a person; Ok(false) if the MCU's count or CRC differ.
pub async fn bulk_test<U: Usb>(m: &mut Mcu<U>, data: &[u8]) -> Result<(bool, String)> {
    let n = data.len();
    m.call(&msg::Pipe { sink: Sink::Discard, reset: true }).await?;
    let t0 = crate::now_ms();
    m.bulk("bulk", data, async |_, _| Ok(true)).await?;
    let dt = (crate::now_ms() - t0) / 1000.0;
    let t1 = crate::now_ms();
    let r = loop {
        // the MCU empties its last buffers
        let r = m.call(&msg::Pipe { sink: Sink::Discard, reset: false }).await?;
        if r.bytes as usize >= n || crate::now_ms() - t1 > 5000.0 {
            break r;
        }
    };
    let crc = crc32fast::hash(data);
    let ok = r.bytes as usize == n && r.crc32 == crc;
    Ok((
        ok,
        format!(
            "{n} bytes in {dt:.2} s: {:.0} KB/s; the MCU got {} bytes in {} buffers, CRC-32 {}, {} times both \
                     buffers full",
            n as f64 / dt / 1024.0,
            r.bytes,
            r.packets,
            if ok { "OK".into() } else { format!("BAD ({:08x})", r.crc32) },
            r.full
        ),
    ))
}
