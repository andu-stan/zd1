//! Server time = client time + diff. Each Time round trip gives c2s = server_rx - client_tx (the server's reply
//! payload) and s2c = client_rx - server_tx; with symmetric latency diff = (c2s - s2c) / 2. Replies share the TCP
//! stream with the audio, so some wait behind a burst of chunks: their round trip (c2s + s2c) is long and their diff
//! off by half the wait. Like NTP, only the fastest third of the last 50 round trips count; diff is their median.

use std::collections::VecDeque;

pub struct TimeSync {
    samples: VecDeque<(i64, i64)>,
    diff: Option<i64>,
} // (round trip, diff)

impl TimeSync {
    pub fn new() -> TimeSync {
        TimeSync { samples: VecDeque::new(), diff: None }
    }

    pub fn add(&mut self, c2s_us: i64, server_tx_us: i64, client_rx_us: i64) {
        let s2c = client_rx_us - server_tx_us;
        if self.samples.len() == 50 {
            self.samples.pop_front();
        }
        self.samples.push_back((c2s_us + s2c, (c2s_us - s2c) / 2));
        let mut v: Vec<(i64, i64)> = self.samples.iter().copied().collect();
        v.sort_unstable(); // by round trip
        v.truncate(v.len().div_ceil(3));
        let mut d: Vec<i64> = v.iter().map(|x| x.1).collect();
        d.sort_unstable();
        self.diff = Some(d[d.len() / 2]);
    }

    pub fn diff(&self) -> Option<i64> {
        self.diff
    }
    pub fn count(&self) -> usize {
        self.samples.len()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn offset_with_symmetric_latency() {
        let mut t = super::TimeSync::new();
        // server = client + 5 s, 3 ms each way: sent at 100, server rx 5_000_103 (c2s 5_000_003), tx 5_000_104,
        // back at client 107
        t.add(5_000_003, 5_000_104, 107);
        assert_eq!(t.diff(), Some(5_000_000));
    }
    #[test]
    fn slow_round_trips_are_ignored() {
        let mut t = super::TimeSync::new();
        for k in 0..30 {
            let wait = if k % 2 == 0 { 0 } else { 400_000 }; // every other reply queued 400 ms behind audio
            t.add(5_000_003, 5_000_104, 107 + wait);
        }
        assert_eq!(t.diff(), Some(5_000_000));
    }
}
